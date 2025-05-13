mod buffer;
mod error;
mod format;
mod pty;
mod reader;
mod shim;
mod signal;
mod status;
mod term;
mod writer;

use crate::buffer::{BufferPool, BufferQueue};
use crate::error::SysError;
use crate::format::{Formatter, TimeSource};
use crate::pty::{PtyProc, PtyWait};
use crate::reader::InterruptibleReader;
use crate::signal::SignalEvent;
use crate::status::*;
use crate::term::{AnsiStripper, TtyMode};
use clap::Parser;
use clap::error::ErrorKind;
use exec::Command;
use rustix::process::Signal;
use rustix::stdio;
use rustix::termios::Termios;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Stdin, Write};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Before start, print header line (hostname, os, time, command).
    #[arg(short = 'H', long, default_value_t = false)]
    header: bool,

    /// Prepend each line of the command output with current time.
    #[arg(short, long, default_value_t = false)]
    ts: bool,

    /// If --ts is used, defines strftime() format string.
    #[arg(long, default_value = "%T%.3f ", value_name = "FMT")]
    ts_fmt: String,

    /// If --ts is used, defines what timestamps to use: wallclock, elapsed time
    /// since program start, or delta between subsequent timestamps.
    #[arg(long, default_value = "wall", value_enum, value_name = "SRC")]
    ts_src: TimeSource,

    /// Output file path (if omitted, select automatically).
    #[arg(
        short,
        long,
        default_value = "",
        hide_default_value = true,
        value_name = "PATH"
    )]
    output: String,

    /// Overwrite --output file if it exists.
    #[arg(short, long, default_value_t = false)]
    force: bool,

    /// Append to --output file if it exists.
    #[arg(conflicts_with = "force", short, long, default_value_t = false)]
    append: bool,

    /// Don't write --output file at all.
    #[arg(
        conflicts_with_all = ["output", "force", "append"],
        short = 'N',
        long,
        default_value_t = false
    )]
    null: bool,

    /// Don't strip ANSI escape codes when writing to --output file.
    #[arg(short = 'R', long, default_value_t = false)]
    raw: bool,

    /// Don't print anything to stdout.
    #[arg(short, long, default_value_t = false)]
    silent: bool,

    /// After EOF from command, wait the specified timeout and then quit (milliseconds).
    #[arg(short, long, default_value_t = 10, value_name = "MILLISECONDS")]
    quit: u64,

    /// When stdout is slower than command output, buffer at max the specified number
    /// of lines; doesn't affect --output file.
    #[arg(short, long, default_value_t = 10_000, value_name = "LINES")]
    buffer: usize,

    /// Enable debug logging to stderr.
    #[arg(short = 'D', long, default_value_t = false)]
    debug: bool,

    /// Print man page (troff).
    #[arg(long, default_value_t = false)]
    man: bool,

    /// Command to run.
    #[arg(
        required_unless_present = "man",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    command: Vec<String>,
}

/// Print usage error to stderr and exit with EXIT_USAGE code.
macro_rules! usage_error {
    ($fmt:expr $(,$args:expr)*) => ({
        use crate::status::*;
        eprint!(concat!("error: ", $fmt, "\n\nFor more information, try '--help'.\n"),
                $($args),*);
        std::process::exit(EXIT_USAGE);
    });
}

/// Parse CLI arguments.
/// Also handles --man, --help, --version, and usage errors.
fn parse_args() -> Args {
    match Args::try_parse() {
        Ok(args) => {
            if args.man {
                print!("{}", include_str!("../reclog.1"));
                process::exit(EXIT_SUCCESS);
            }

            if args.command.is_empty() {
                usage_error!("command can't be empty");
            }
            if args.command[0].starts_with('-') {
                usage_error!("unknown option '{}'", args.command[0]);
            }

            if args.debug {
                DEBUG.store(true, Ordering::SeqCst);
            }

            args
        }
        Err(err) if err.kind() == ErrorKind::DisplayHelp => {
            print!("{}", err);
            process::exit(EXIT_SUCCESS);
        }
        Err(err) if err.kind() == ErrorKind::DisplayVersion => {
            print!(
                "{} {}\nCopyright (C) {}\n",
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
                env!("CARGO_PKG_AUTHORS")
            );
            process::exit(EXIT_SUCCESS);
        }
        Err(err) => {
            eprint!("{}", err);
            process::exit(EXIT_USAGE);
        }
    }
}

/// Choose output path.
fn choose_output(args: &Args) -> String {
    if args.null {
        return String::new();
    }

    if !args.output.is_empty() {
        return args.output.clone();
    }

    let base_name = match Path::new(&args.command[0]).file_stem() {
        Some(name) => name.to_str().unwrap().to_string(),
        None => usage_error!("invalid command '{}'", args.command[0]),
    };

    let mut out_path = format!("{}.log", base_name);

    if !args.force {
        let mut suffix = 1;
        while Path::new(&out_path).exists() {
            out_path = format!("{}-{}.log", base_name, suffix);
            suffix += 1;
        }
    }

    out_path
}

/// Enable debug logs.
static DEBUG: AtomicBool = AtomicBool::new(false);

/// Print message to stderr if debug logs are enabled.
macro_rules! debug {
    ($fmt:expr $(,$args:expr)*) => ({
        if DEBUG.load(Ordering::Relaxed) {
            eprintln!(
                concat!("reclog: {}: ", $fmt),
                thread::current().name().unwrap_or(shim::gettid().to_string().as_str()),
                $($args),*);
        }
    });
}

/// Print message to stderr, perform cleanup, and exit with given code.
/// Error message is optional.
/// Takes care of global cleanup.
macro_rules! terminate {
    ($code:expr) => {{
        before_exit();
        process::exit($code);
    }};
    ($code:expr; $fmt:expr) => ({
        eprintln!(concat!("reclog: ", $fmt));
        before_exit();
        process::exit($code);
    });
    ($code:expr; $fmt:expr, $($args:expr),+) => ({
        eprintln!(concat!("reclog: ", $fmt), $($args),+);
        before_exit();
        process::exit($code);
    });
}

/// Deliver signal to current process.
/// If it's a deadly signal like SIGTERM, kills current process.
/// If it's a stop signal like SIGTSTP, stops process until it receives SIGCONT.
/// Takes care of global cleanup.
fn raise_signal(sig: Signal) -> Result<(), SysError> {
    debug!("raising signal {}", signal::display_name(sig));
    before_exit();
    signal::deliver_signal(sig)?;

    // Awake after SIGCONT.
    before_start(StartMode::Wakeup);
    debug!("returned from signal {}", signal::display_name(sig));

    Ok(())
}

/// Saved original TTY state.
static TTY_STATE: OnceLock<Termios> = OnceLock::new();

#[derive(PartialEq)]
enum StartMode {
    Startup, // Initial startup
    Wakeup,  // Wake up by SIGCONT
}

/// Global initialization.
/// Called at startup and wakeup after SIGCONT.
fn before_start(mode: StartMode) {
    debug!("running before_start hook");

    if mode == StartMode::Startup {
        // Setup default dispositions and block all signals.
        debug!("initializing signals");
        if let Err(err) = signal::init_parent_signals() {
            terminate!(EXIT_FAILURE; "can't initialize signal handlers: {}", err);
        }
    }

    if term::is_tty(stdio::stdin()) {
        if mode == StartMode::Startup {
            // Save original tty state.
            debug!("saving tty state of stdin");
            let state = match term::save_tty_state(stdio::stdin()) {
                Ok(state) => state,
                Err(err) => {
                    terminate!(EXIT_FAILURE; "can't read tty state: {}", err);
                }
            };
            TTY_STATE.set(state).unwrap();
        }

        // Enable canonical mode for stdin.
        debug!("enabling canonical mode for stdin");
        if let Err(err) = term::set_tty_mode(stdio::stdin(), TtyMode::Canon) {
            terminate!(EXIT_FAILURE; "can't switch tty to canonical mode: {}", err);
        }
    }
}

/// Global cleanup.
/// Called before stop or exit.
fn before_exit() {
    debug!("running before_exit hook");

    // Restore original tty state if it was saved.
    debug!("restoring tty state of stdin");
    if let Some(state) = TTY_STATE.get() {
        _ = term::restore_tty_state(stdio::stdin(), state);
    }
}

/// Thread that waits for next signal and processes it, in a loop.
/// All threads block all signals that we want to process, and this thread
/// fetches them one by one using sigwait().
/// Possible signals are SIGCHILD (child exited), various termination
/// signals, and stop/resume signals.
fn process_signals(
    pty_proc: Arc<PtyProc>,
    pty_reader: Arc<InterruptibleReader<OwnedFd>>,
    stdin_reader: Arc<InterruptibleReader<Stdin>>,
    timeout: Duration,
) -> Option<Signal> {
    debug!("entering process_signals thread");

    let mut pending_interrupt = None;
    let mut pending_stop = None;

    'wait_signal: loop {
        // Wait for SIGCHILD or other signal.
        debug!("waiting for next signal");
        let event = match signal::wait_signal(None) {
            Ok(ev) => ev,
            Err(err) => terminate!(EXIT_FAILURE; "can't wait for signal: {}", err),
        };

        debug!("received event: {:?}", event);
        match event {
            // Interrupt signal received first time.
            SignalEvent::Interrupt(sig) if pending_interrupt.is_none() => {
                // Ask child to exit and wait for SIGCHILD.
                debug!("sending signal {} to child", signal::display_name(sig));
                _ = pty_proc.kill_child(sig);
                pending_interrupt = Some(sig);
                continue 'wait_signal;
            }

            // Interrupt signal received second time, or quit signal received.
            SignalEvent::Interrupt(sig) | SignalEvent::Quit(sig) => {
                // Ask child to exit, if not asked before, wait until it exits, OR timeout expires,
                // OR termination signal is received again (e.g. user hits ^\ twice).
                if pending_interrupt.is_none() {
                    debug!("sending signal {} to child", signal::display_name(sig));
                    _ = pty_proc.kill_child(sig);

                    debug!("waiting for any signal or timeout");
                    if let Err(err) = signal::wait_signal(Some(timeout)) {
                        terminate!(EXIT_FAILURE; "can't wait for signal: {}", err);
                    }
                }
                match pty_proc.wait_child(PtyWait::NoHang) {
                    Ok(Some(status)) if status.exited() || status.signaled() => {
                        debug!("child exited");
                    }
                    _ => {
                        // If child is still alive, kill it forcibly.
                        debug!("child still running, sending SIGKILL");
                        _ = pty_proc.kill_child(Signal::KILL);
                    }
                }
                // Deliver signal to ourselves, which should kill us.
                debug!("sending signal {} to ourselves", signal::display_name(sig));
                if let Err(err) = raise_signal(sig) {
                    terminate!(EXIT_FAILURE; "can't raise signal: {}", err);
                }
            }

            // Stop signal received first time.
            SignalEvent::Stop(sig) if pending_stop.is_none() => {
                // Ask child to stop and wait for SIGCHILD.
                debug!("sending signal SIGSTOP to child");
                _ = pty_proc.kill_child(Signal::STOP);
                pending_stop = Some(sig);
                continue 'wait_signal;
            }

            // Stop signal received second time.
            SignalEvent::Stop(sig) => {
                // Forcibly stop child, stop ourselves until we get SIGCONT.
                debug!("sending signal SIGSTOP to child");
                _ = pty_proc.kill_child(Signal::STOP);

                debug!("sending signal {} to ourselves", signal::display_name(sig));
                if let Err(err) = raise_signal(sig) {
                    terminate!(EXIT_FAILURE; "can't raise signal: {}", err);
                }

                // We received SIGCONT.
                debug!("fetching SIGCONT signal");
                if let Err(err) = signal::drop_signal(Signal::CONT) {
                    terminate!(EXIT_FAILURE; "can't drop signal: {}", err);
                }

                debug!("sending SIGCONT signal to child");
                _ = pty_proc.kill_child(Signal::CONT);
                pending_stop = None;
                continue 'wait_signal;
            }

            // Resume signal received while we were NOT stopped.
            SignalEvent::Continue(_) => {
                // Re-ensure child is running.
                debug!("sending SIGCONT signal to child");
                _ = pty_proc.kill_child(Signal::CONT);
                pending_stop = None;
                continue 'wait_signal;
            }

            // Parent tty window change (SIGWINCH).
            SignalEvent::Resize(_) => {
                // Propagate resize to child.
                debug!("propagating tty window resize");
                if let Err(err) = pty_proc.resize_child() {
                    terminate!(EXIT_FAILURE; "can't resize pty: {}", err);
                }
                continue 'wait_signal;
            }

            // Child exited or stopped or resumed.
            SignalEvent::Child(_) => {
                match pty_proc.wait_child(PtyWait::NoHang) {
                    // Child exited.
                    Ok(Some(status)) if status.exited() || status.signaled() => {
                        debug!("child exited, terminating wait loop");
                        break 'wait_signal;
                    }
                    // Child stopped.
                    Ok(Some(status)) if status.stopped() => {
                        debug!("child stopped");
                        if let Some(stop_sig) = pending_stop {
                            // Stop ourselves until we get SIGCONT.
                            debug!(
                                "sending signal {} to ourselves",
                                signal::display_name(stop_sig)
                            );
                            if let Err(err) = raise_signal(stop_sig) {
                                terminate!(EXIT_FAILURE; "can't raise signal: {}", err);
                            }

                            // We received SIGCONT.
                            debug!("fetching SIGCONT signal");
                            if let Err(err) = signal::drop_signal(Signal::CONT) {
                                terminate!(EXIT_FAILURE; "can't drop signal: {}", err);
                            }

                            debug!("sending SIGCONT signal to child");
                            _ = pty_proc.kill_child(Signal::CONT);
                            pending_stop = None;
                            continue 'wait_signal;
                        }
                    }
                    Ok(_) => {
                        debug!("ignoring child event");
                        continue 'wait_signal;
                    }
                    Err(err) => {
                        terminate!(EXIT_COMMAND_FAILED; "can't wait child process: {}", err);
                    }
                }
            }

            _ => {
                // Nothing interesting.
                debug!("ignoring event");
                continue 'wait_signal;
            }
        }
    }

    // Set timeout for PTY. After there is no data from child during timeout, PTY
    // reading thread will get EOF and exit. Timeout is needed to ensure we've
    // read all pending data after child exit.
    debug!("setting pty reader timeout to {:?}", timeout);
    if let Err(err) = pty_reader.set_timeout(timeout) {
        terminate!(EXIT_FAILURE; "can't set read timeout: {}", err);
    }

    // Interrupt STDIN.
    // Stdin reading thread will get EOF and exit.
    debug!("closing stdin reader");
    if let Err(err) = stdin_reader.close() {
        terminate!(EXIT_FAILURE; "can't close stdin: {}", err);
    }

    debug!("leaving process_signals thread");

    pending_interrupt
}

/// Thread that reads lines from stdin and writes to master pty
/// (i.e. to child's stdin).
fn stdin_2_pty(
    pty_proc: Arc<PtyProc>,
    mut pty_writer: File,
    stdin_reader: Arc<InterruptibleReader<Stdin>>,
) {
    debug!("entering stdin_2_pty thread");

    let tty_codes = {
        let slave_fd = match pty_proc.dup_slave() {
            Ok(fd) => fd,
            Err(err) => terminate!(EXIT_FAILURE; "can't duplicate slave fd: {}", err),
        };
        match term::get_tty_codes(&slave_fd) {
            Ok(codes) => codes,
            Err(err) => terminate!(EXIT_FAILURE; "can't read pty attributes: {}", err),
        }
    };

    let mut buf_reader = BufReader::new(stdin_reader.blocking_reader());
    let mut buf = String::new();

    let mut stdin_eof = false;
    while !stdin_eof {
        buf.clear();
        let size = match buf_reader.read_line(&mut buf) {
            Ok(size) => size,
            Err(err) => terminate!(EXIT_FAILURE; "can't read from stdin: {}", err),
        };

        stdin_eof = size == 0;
        if stdin_eof {
            // Propagate EOF by writing VEOF to master PTY.
            // We've enabled canonical mode, which should translate this
            // symbol to end-of-file condition.
            debug!("got eof from stdin, propagating to child");
            buf.clear();
            buf.push(tty_codes.VEOF);
        }

        if let Err(err) = pty_writer.write_all(buf.as_bytes()) {
            terminate!(EXIT_FAILURE; "can't write to pty: {}", err);
        }
    }

    debug!("leaving stdin_2_pty thread");
}

/// Thread that reads lines from buffer queue and writes them to stdout.
fn queue_2_stdout(buf_queue: Arc<BufferQueue>) {
    debug!("entering queue_2_stdout thread");

    loop {
        let buf = match buf_queue.read() {
            Some(buf) => buf,
            None => break, // queue closed, exit loop
        };

        if let Err(err) = io::stdout().write_all(buf.as_bytes()) {
            terminate!(EXIT_FAILURE; "can't write to stdout: {}", err);
        }

        // buf is returned to pool here.
    }

    debug!("leaving queue_2_stdout thread");
}

/// Thread that reads lines from master pty (i.e. child's stdout) and writes
/// them to output file and to buffer queue.
fn pty_2_queue_and_file(
    pty_reader: Arc<InterruptibleReader<OwnedFd>>,
    out_writer: &mut dyn Write,
    buf_queue: &Arc<BufferQueue>,
    buf_pool: &Arc<BufferPool>,
    fm: &mut Formatter,
) {
    debug!("entering pty_2_queue_and_file thread");

    let mut pty_line_reader = BufReader::new(pty_reader.blocking_reader());

    loop {
        let mut buf = buf_pool.alloc();

        if fm.need_header() {
            if let Err(err) = fm.format_header(&mut buf) {
                terminate!(EXIT_FAILURE; "can't format header: {}", err);
            }
        } else {
            if fm.need_timestamp() {
                if let Err(err) = fm.format_timestamp(&mut buf) {
                    terminate!(EXIT_FAILURE; "can't format timestamp: {}", err);
                }
            }
            let size = match pty_line_reader.read_line(&mut buf) {
                Ok(size) => size,
                Err(err) => terminate!(EXIT_FAILURE; "can't read from pty: {}", err),
            };
            if size == 0 {
                // EOF, exit loop
                debug!("got eof from pty, exiting");
                break;
            }
        }

        // Write buffer (probably stripped) to output file, synchronously.
        if let Err(err) = out_writer.write_all(buf.as_bytes()) {
            terminate!(EXIT_FAILURE; "can't write output file: {}", err);
        }

        // Move buffer to queue.
        // pty_2_stdout_thread will fetch it, write to stdout, and return buffer to pool.
        // If queue is full, oldest elements are removed. That's fine - our stdout is
        // supposed to be a TTY, and if it's too slow to display all lines in time,
        // there is no need trying to write all of them - user won't see them
        // anyway at that speed and VTE scrollback is usually limited and will
        // anyway remove them.
        buf_queue.write(buf);
    }

    debug!("leaving pty_2_queue_and_file thread");
}

/// Get child process exit code and exit with same code.
fn forward_exit_status(pty_proc: Arc<PtyProc>, pending_interrupt: Option<Signal>) -> ! {
    match pty_proc.child_status() {
        // Command exited normally.
        status if status.exited() => {
            let exit_code = status.exit_status().unwrap();
            if exit_code == EXIT_SUCCESS {
                debug!("exiting with code {}", exit_code);
                terminate!(exit_code);
            } else {
                terminate!(exit_code; "command exited with code {}", exit_code);
            }
        }

        // Command killed by signal.
        status if status.signaled() => {
            if let Some(sig) = pending_interrupt {
                // Command was not killed by itself - we killed it because *we* received
                // death signal from user (e.g. ^C) - then we don't need to print any error
                // message, just process original signal and die.
                debug!("delivering pending {:?} signal to ourselves", sig);
                if let Err(err) = raise_signal(sig) {
                    terminate!(EXIT_FAILURE; "can't raise signal: {}", err);
                }
            }

            // Command was killed unexpectedly, not by us - then report error and
            // forward death signal N as exit code 128+N.
            let sig_number = status.terminating_signal().unwrap();
            let exit_code = EXIT_COMMAND_SIGNALED + sig_number;

            if let Some(sig) = Signal::from_named_raw(sig_number) {
                terminate!(exit_code;
                           "command terminated by signal {}",
                           signal::display_name(sig)
                );
            } else {
                terminate!(exit_code;
                    "command terminated by signal {}",
                    sig_number
                );
            }
        }

        // Should not happen.
        _ => {
            terminate!(EXIT_COMMAND_FAILED; "command failed");
        }
    };
}

fn main() {
    // Parse CLI arguments.
    let args = parse_args();
    let out_path = choose_output(&args);

    // Global initialization.
    before_start(StartMode::Startup);

    // Construct output file writer.
    let mut out_file;
    let out_writer: &mut dyn Write = if args.null {
        &mut io::empty()
    } else {
        debug!("opening output file: {}", out_path);
        out_file = match OpenOptions::new()
            .write(true)
            .create(args.force || args.append)
            .create_new(!(args.force || args.append))
            .append(args.append)
            .truncate(!args.append)
            .open(&out_path)
        {
            Ok(file) => file,
            Err(err) => terminate!(
                EXIT_FAILURE; "can't open output file \"{}\": {}",
                out_path, err
            ),
        };
        if args.raw {
            &mut out_file
        } else {
            &mut AnsiStripper::new(out_file)
        }
    };

    // Construct output formatter.
    let mut formatter = Formatter::new(
        args.header,
        args.ts,
        &args.ts_fmt,
        args.ts_src,
        &args.command,
    );

    // Master/slave pty pair and child process attached to it.
    debug!("opening pty pair");
    let pty_proc = match PtyProc::open() {
        Ok(pty) => Arc::new(pty),
        Err(err) => terminate!(EXIT_FAILURE; "can't open pty: {}", err),
    };

    // Writer for master pty (writes to child's stdin).
    let pty_writer = {
        let master_fd = match pty_proc.dup_master() {
            Ok(fd) => fd,
            Err(err) => terminate!(EXIT_FAILURE; "can't duplicate master: {}", err),
        };
        File::from(master_fd)
    };

    // Reader for master pty (reads from child's stdout+stderr).
    let pty_reader = {
        let master_fd = match pty_proc.dup_master() {
            Ok(fd) => fd,
            Err(err) => terminate!(EXIT_FAILURE; "can't duplicate master: {}", err),
        };
        match InterruptibleReader::open(master_fd) {
            Ok(reader) => Arc::new(reader),
            Err(err) => terminate!(EXIT_FAILURE; "can't open master for reading: {}", err),
        }
    };

    // Launch child process.
    debug!("launching command: {:?}", args.command);
    let mut cmd = Command::new(&args.command[0]);
    if args.command.len() > 1 {
        cmd.args(&args.command[1..]);
    }
    if let Err(err) = pty_proc.spawn_child(&mut cmd) {
        terminate!(EXIT_COMMAND_FAILED; "can't execute command: {}", err);
    }

    // Thread-safe buffer pool and queue.
    let buf_pool = Arc::new(BufferPool::new());
    let buf_queue = Arc::new(BufferQueue::new(args.buffer));

    // Closed queue will silently discard everything written to it.
    if args.silent {
        debug!("closing buffer queue");
        buf_queue.close();
    }

    // Allows to read from stdin from one thread and interrupt it from another thread.
    let stdin_reader = Arc::new(match InterruptibleReader::open(io::stdin()) {
        Ok(reader) => reader,
        Err(err) => terminate!(EXIT_FAILURE; "can't open stdin for reading: {}", err),
    });

    // Process signals on separate thread.
    let process_signals_thread = {
        let pty_proc = Arc::clone(&pty_proc);
        let pty_reader = Arc::clone(&pty_reader);
        let stdin_reader = Arc::clone(&stdin_reader);

        debug!("spawning process_signals thread");
        thread::Builder::new()
            .name("process_signals".to_string())
            .spawn(move || -> Option<Signal> {
                process_signals(
                    pty_proc,
                    pty_reader,
                    stdin_reader,
                    Duration::from_millis(args.quit),
                )
            })
            .unwrap()
    };

    // Read from our stdin and write to child's stdin.
    let stdin_2_pty_thread = {
        let pty_proc = Arc::clone(&pty_proc);
        let stdin_reader = Arc::clone(&stdin_reader);

        debug!("spawning stdin_2_pty_thread thread");
        thread::Builder::new()
            .name("stdin_2_pty".to_string())
            .spawn(move || {
                stdin_2_pty(pty_proc, pty_writer, stdin_reader);
            })
            .unwrap()
    };

    // Read from buffer queue and write to our stdout.
    let pty_2_stdout_thread = {
        let buf_queue = Arc::clone(&buf_queue);

        debug!("spawning pty_2_stdout_thread thread");
        thread::Builder::new()
            .name("pty_2_stdout".to_string())
            .spawn(move || {
                queue_2_stdout(buf_queue);
            })
            .unwrap()
    };

    // Read from child stdout and write to output file and to buffer queue.
    // pty_2_stdout() will read from buffer queue and write to our stdout.
    //
    // This function works until it gets EOF or process_signals() tells it
    // to exit by interrupting pty_reader.
    debug!("running pty_2_queue_and_file thread");
    pty_2_queue_and_file(
        pty_reader,
        out_writer,
        &buf_queue,
        &buf_pool,
        &mut formatter,
    );

    // Tell pty_2_stdout() to exit (after writing all pending buffers).
    debug!("closing buffer queue");
    buf_queue.close();

    // Wait until child process exits.
    debug!("waiting for process_signals_thread");
    let pending_interrupt = process_signals_thread.join().unwrap();

    // Tell stdin_2_pty() to terminate.
    debug!("closing stdin reader");
    _ = stdin_reader.close();

    // Wait remaining threads.
    debug!("waiting for pty_2_stdout_thread");
    pty_2_stdout_thread.join().unwrap();
    debug!("waiting for stdin_2_pty_thread");
    stdin_2_pty_thread.join().unwrap();

    // Forward exit status.
    debug!("forwarding exit status");
    forward_exit_status(pty_proc, pending_interrupt);
}
