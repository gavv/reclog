mod buffer;
mod error;
mod pty;
mod reader;
mod shim;
mod status;
mod term;

use crate::buffer::{BufferPool, BufferQueue};
use crate::pty::PtyProc;
use crate::reader::InterruptibleReader;
use crate::status::*;
use crate::term::{AnsiStripper, TtyMode};
use clap::Parser;
use exec::Command;
use rustix::process::Signal;
use rustix::stdio;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::AsFd;
use std::path::Path;
use std::process;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Output file path.
    #[arg(short, long, default_value = "", value_name = "PATH")]
    output: String,

    /// Append to output file instead of overwriting.
    #[arg(short, long, default_value_t = false)]
    append: bool,

    /// Don't strip ANSI escape codes when writing to file.
    #[arg(short, long, default_value_t = false)]
    raw: bool,

    /// Timeout to wait after program exits and doesn't produce output (milliseconds).
    #[arg(short, long, default_value_t = 50, value_name = "MILLISECONDS")]
    timeout: u64,

    /// Maximum queue size when writing to stdout (number of lines).
    #[arg(short, long, default_value_t = 10_000, value_name = "LINES")]
    queue: usize,

    /// Command to run.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

fn main() {
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("reclog: {}", err);
            process::exit(EXIT_USAGE);
        }
    };

    let cmd0 = &args.command[0];

    // "/usr/bin/ls" => "ls"
    let cmd_name = match Path::new(cmd0).file_stem() {
        Some(name) => name.to_str().unwrap().to_string(),
        None => {
            eprintln!("reclog: error: empty command name");
            process::exit(EXIT_USAGE);
        }
    };

    // File to write logs to.
    let out_path = if args.output != "" {
        args.output
    } else {
        cmd_name + ".log"
    };

    // Configure our input tty.
    if term::is_tty(&stdio::stdin()) {
        if let Err(err) = term::set_tty_mode(&stdio::stdin(), TtyMode::Canon) {
            eprintln!("reclog: error: can't switch tty to canonical mode: {}", err);
            process::exit(EXIT_FAILURE);
        }
    }

    // Open output file to write logs to.
    let mut out_file = match OpenOptions::new()
        .write(true)
        .create(true)
        .append(args.append)
        .truncate(!args.append)
        .open(&out_path)
    {
        Ok(file) => file,
        Err(err) => {
            eprintln!(
                "reclog: error: can't open output file \"{}\": {}",
                out_path, err
            );
            process::exit(EXIT_FAILURE);
        }
    };

    // Open master/slave pty pair.
    let pty_proc = match PtyProc::open() {
        Ok(pty) => Arc::new(pty),
        Err(err) => {
            eprintln!("reclog: can't open pty: {}", err);
            process::exit(EXIT_FAILURE);
        }
    };

    // Writer for master pty.
    let pty_write_fd = match pty_proc.dup_master() {
        Ok(fd) => fd,
        Err(err) => {
            eprintln!("reclog: can't duplicate master: {}", err);
            process::exit(EXIT_FAILURE);
        }
    };
    let mut pty_writer = File::from(pty_write_fd);

    // Reader for master pty.
    let pty_read_fd = match pty_proc.dup_master() {
        Ok(fd) => fd,
        Err(err) => {
            eprintln!("reclog: can't duplicate master: {}", err);
            process::exit(EXIT_FAILURE);
        }
    };
    let pty_reader = match InterruptibleReader::open(pty_read_fd) {
        Ok(reader) => Arc::new(reader),
        Err(err) => {
            eprintln!("reclog: can't open master for reading: {}", err);
            process::exit(EXIT_FAILURE);
        }
    };

    // Launch child process.
    let mut cmd = Command::new(&args.command[0]);
    if args.command.len() > 1 {
        cmd.args(&args.command[1..]);
    }
    if let Err(err) = pty_proc.spawn_child(&mut cmd) {
        eprintln!("reclog: can't execute command \"{}\": {}", cmd0, err);
        process::exit(EXIT_COMMAND_FAILED);
    }

    // Thread-safe buffer pool and queue.
    let buf_pool = Arc::new(BufferPool::new());
    let buf_queue = Arc::new(BufferQueue::new(args.queue));

    // Allows to read from stdin from one thread and interrupt it
    // by calling close() from another thread.
    let stdin_reader = Arc::new(match InterruptibleReader::open(io::stdin()) {
        Ok(reader) => reader,
        Err(err) => {
            eprintln!("reclog: can't open stdin for reading: {}", err);
            process::exit(EXIT_FAILURE);
        }
    });

    // Read from our stdin and write to child's stdin.
    let stdin_2_pty_thread = {
        let cmd0 = cmd0.clone();
        let stdin_reader = Arc::clone(&stdin_reader);

        let tty_codes = {
            let pty_slave_fd = match pty_proc.dup_slave() {
                Ok(fd) => fd,
                Err(err) => {
                    eprintln!("reclog: can't duplicate slave: {}", err);
                    process::exit(EXIT_FAILURE);
                }
            };
            match term::get_tty_codes(&pty_slave_fd.as_fd()) {
                Ok(codes) => codes,
                Err(err) => {
                    eprintln!("reclog: error: can't read pty attributes: {}", err);
                    process::exit(EXIT_FAILURE);
                }
            }
        };

        thread::spawn(move || {
            let mut buf_reader = BufReader::new(stdin_reader.blocking_reader());
            let mut buf = String::new();

            let mut stdin_eof = false;
            while !stdin_eof {
                buf.clear();
                let size = match buf_reader.read_line(&mut buf) {
                    Ok(size) => size,
                    Err(err) => {
                        eprintln!("reclog: error: can't read from stdin: {}", err);
                        process::exit(EXIT_FAILURE);
                    }
                };

                stdin_eof = size == 0;
                if stdin_eof {
                    // Propagate EOF by writing VEOF to master PTY.
                    // We've enabled canonical mode, which should translate this
                    // symbol to end-of-file condition.
                    buf.clear();
                    buf.push(tty_codes.VEOF);
                }

                if let Err(err) = pty_writer.write_all(buf.as_bytes()) {
                    eprintln!(
                        "reclog: error: can't write input to stdin of command \"{}\": {}",
                        cmd0, err
                    );
                    process::exit(EXIT_COMMAND_FAILED);
                }
            }
        })
    };

    // Read from buffer queue and write to our stdout.
    let pty_2_stdout_thread = {
        let buf_queue = Arc::clone(&buf_queue);

        thread::spawn(move || {
            loop {
                let buf = match buf_queue.read() {
                    Some(buf) => buf,
                    None => return, // queue closed, exit thread
                };

                if let Err(err) = io::stdout().write_all(buf.as_bytes()) {
                    eprintln!("reclog: error: can't write to stdout: {}", err);
                    process::exit(EXIT_FAILURE);
                }

                // buf is returned to pool here.
            }
        })
    };

    // Wait process exit.
    let proc_wait_thread = {
        let pty = Arc::clone(&pty_proc);
        let pty_reader = Arc::clone(&pty_reader);

        thread::spawn(move || {
            // Wait until child exits.
            if let Err(err) = pty.wait_child() {
                eprintln!("reclog: can't wait child process: {}", err);
                process::exit(EXIT_COMMAND_FAILED);
            }

            // Enable timeout for reading from child's pty.
            // After there is no more data during timeout, the reading loop in
            // main thread will terminate.
            if let Err(err) = pty_reader.set_timeout(Duration::from_millis(args.timeout)) {
                eprintln!("reclog: can't set read timeout: {}", err);
                process::exit(EXIT_COMMAND_FAILED);
            }
        })
    };

    // Read from child stdout and write to log file and to buffer queue.
    // pty_2_stdout_thread will read from buffer queue and write to our stdout.
    let mut pty_line_reader = BufReader::new(pty_reader.blocking_reader());

    let mut out_stripper;

    let out_writer: &mut dyn Write = if args.raw {
        // Write directly to file.
        &mut out_file
    } else {
        // Write to stripper, which in turn writes to file.
        out_stripper = AnsiStripper::new(out_file);
        &mut out_stripper
    };

    loop {
        let mut buf = buf_pool.alloc();

        let size = match pty_line_reader.read_line(&mut buf) {
            Ok(size) => size,
            Err(err) => {
                eprintln!(
                    "reclog: error: can't read output from command \"{}\": {}",
                    cmd0, err
                );
                process::exit(EXIT_COMMAND_FAILED);
            }
        };
        if size == 0 {
            // EOF, exit loop
            break;
        }

        // Write buffer (probably stripped) to output file, synchronously.
        if let Err(err) = out_writer.write_all(buf.as_bytes()) {
            eprintln!(
                "reclog: error: can't write output file \"{}\": {}",
                out_path, err
            );
            process::exit(EXIT_FAILURE);
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

    // Tell pty_2_stdout_thread to exit after writing all pending buffers.
    buf_queue.close();

    // Wait until process exits.
    proc_wait_thread.join().unwrap();

    // Tell stdin_2_pty_thread to terminate.
    _ = stdin_reader.close();

    // Wait remaining threads.
    pty_2_stdout_thread.join().unwrap();
    stdin_2_pty_thread.join().unwrap();

    // Forward exit status.
    match pty_proc.child_status() {
        // command exited normally, forward its exit code
        status if status.exited() => {
            let code = status.exit_status().unwrap();
            if code != 0 {
                eprintln!(
                    "reclog: error: command \"{}\" exited with code {}",
                    cmd0, code
                );
            }
            process::exit(code);
        }
        // command killed by signal, forward signal number
        status if status.signaled() => {
            let sig = Signal::from_named_raw(status.terminating_signal().unwrap()).unwrap();
            eprintln!(
                "reclog: error: command \"{}\" terminated by signal {:?} [{}]",
                cmd0,
                sig,
                sig.as_raw(),
            );
            process::exit(EXIT_COMMAND_SIGNALED + sig.as_raw());
        }
        // should not happen
        _ => {
            eprintln!("reclog: error: command \"{}\" failed", cmd0);
            process::exit(EXIT_COMMAND_FAILED);
        }
    };
}
