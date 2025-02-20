use crate::error::SysError;
use crate::shim::{self, Fork};
use crate::signal;
use crate::status::*;
use crate::term::{self, TtyMode};
use exec::Command;
use rustix::fs::{self, Mode, OFlags};
use rustix::io::{self, Errno, retry_on_intr};
use rustix::process::{self, Pid, Signal, WaitOptions, WaitStatus};
use rustix::pty::{self, OpenptFlags};
use rustix::stdio;
use std::os::fd::{AsFd, OwnedFd, RawFd};
use std::path::Path;
use std::sync::Mutex;
use sysconf::raw::{SysconfVariable, sysconf};

/// Allows to create PTY pair and spawn child process.
/// I haven't found existing create for PTY that would allow keeping slave_fd
/// opened in parent, which we need to properly read pending data after child
/// exits (to avoid EIO). Hence we have our own implementation.
pub struct PtyProc {
    master_fd: OwnedFd,
    slave_fd: OwnedFd,
    child: Mutex<Child>,
}

struct Child {
    pid: Option<Pid>,
    last_status: Option<WaitStatus>,
    final_status: Option<WaitStatus>,
}

/// Wait mode.
#[derive(PartialEq)]
pub enum PtyWait {
    Hang,
    NoHang,
}

impl PtyProc {
    /// Open master/slave pair.
    pub fn open() -> Result<Self, SysError> {
        // open master pty
        let master_fd = match retry_on_intr(|| pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY))
        {
            Ok(fd) => fd,
            Err(err) => return Err(SysError("openpt()", err)),
        };

        // unlock slave pty
        if let Err(err) = retry_on_intr(|| pty::grantpt(&master_fd)) {
            return Err(SysError("grantpt()", err));
        }
        if let Err(err) = retry_on_intr(|| pty::unlockpt(&master_fd)) {
            return Err(SysError("unlockpt()", err));
        }

        // open slave pty
        let pts_name = match shim::ptsname(master_fd.as_fd()) {
            Ok(s) => s,
            Err(err) => return Err(SysError("ptsname()", err)),
        };
        let slave_fd = match retry_on_intr(|| {
            fs::open(
                Path::new(&pts_name),
                OFlags::RDWR | OFlags::NOCTTY,
                Mode::empty(),
            )
        }) {
            Ok(fd) => fd,
            Err(err) => return Err(SysError("open()", err)),
        };

        Ok(PtyProc {
            master_fd,
            slave_fd,
            child: Mutex::new(Child {
                pid: None,
                last_status: None,
                final_status: None,
            }),
        })
    }

    /// Duplicate master fd.
    pub fn dup_master(&self) -> Result<OwnedFd, SysError> {
        retry_on_intr(|| io::dup(&self.master_fd)).map_err(|err| SysError("dup()", err))
    }

    /// Duplicate slave fd.
    pub fn dup_slave(&self) -> Result<OwnedFd, SysError> {
        retry_on_intr(|| io::dup(&self.slave_fd)).map_err(|err| SysError("dup()", err))
    }

    /// Fork child process, attach to pty slave, and exec command.
    pub fn spawn_child(&self, command: &mut Command) -> Result<(), SysError> {
        let mut locked_child = self.child.lock().unwrap();

        if locked_child.pid.is_some() {
            panic!("attempt to call spawn_child() twice");
        }

        self.prepare_parent()?;

        // SAFETY: we ensure that the child process does not run any code other
        // than setup code from prepare_child() followed by exec(). Parent
        // continues execution normally.
        unsafe {
            match shim::fork() {
                Ok(Fork::Parent(pid)) => {
                    locked_child.pid = Some(pid);
                }
                Ok(Fork::Child) => {
                    // In case of error, use fast_exit() to avoid execution
                    // of any registered exit handlers.
                    if let Err(_) = self.prepare_child() {
                        shim::fast_exit(EXIT_FAILURE);
                    }

                    // This will call execvp() and replace child process.
                    _ = command.exec();
                    shim::fast_exit(EXIT_COMMAND_FAILED);
                }
                Err(err) => {
                    return Err(SysError("fork()", err));
                }
            }
        };

        Ok(())
    }

    /// Resize pty according to current parent's tty.
    pub fn resize_child(&self) -> Result<(), SysError> {
        let _locked_child = self.child.lock().unwrap();

        if term::is_tty(&stdio::stdout()) {
            // Kernel will update slave pty and send SIGWINCH to child process.
            term::copy_tty_size(&self.master_fd.as_fd(), &stdio::stdout())?;
        }

        Ok(())
    }

    /// Send signal to child's process group.
    pub fn kill_child(&self, sig: Signal) -> Result<(), SysError> {
        let locked_child = self.child.lock().unwrap();

        if !locked_child.pid.is_some() {
            panic!("attempt to call kill_child() before spawn_child()");
        }
        if locked_child.final_status.is_some() {
            panic!("attempt to call kill_child() after wait_child()");
        }

        if let Err(err) = process::kill_process_group(locked_child.pid.unwrap(), sig) {
            return Err(SysError("kill()", err));
        }

        Ok(())
    }

    /// Wait until spawned child exits.
    pub fn wait_child(&self, wait_mode: PtyWait) -> Result<Option<WaitStatus>, SysError> {
        let mut locked_child = self.child.lock().unwrap();

        if !locked_child.pid.is_some() {
            panic!("attempt to call wait_child() before spawn_child()");
        }
        if let Some(final_status) = locked_child.final_status {
            return Ok(Some(final_status));
        }

        let mut wait_opts = WaitOptions::UNTRACED | WaitOptions::CONTINUED;
        if wait_mode == PtyWait::NoHang {
            wait_opts |= WaitOptions::NOHANG;
        }

        loop {
            let wait_status = match process::waitpid(locked_child.pid, wait_opts) {
                Ok(Some((_, status))) => status,
                Ok(None) => return Ok(None),
                Err(Errno::INTR) => continue,
                Err(err) => return Err(SysError("waitpid()", err)),
            };

            locked_child.last_status = Some(wait_status);
            if wait_status.exited() || wait_status.signaled() {
                locked_child.final_status = Some(wait_status);
            }
            return Ok(Some(wait_status));
        }
    }

    /// Get child exit status.
    pub fn child_status(&self) -> WaitStatus {
        let locked_child = self.child.lock().unwrap();

        if !locked_child.last_status.is_some() {
            panic!("attempt to call child_status() before wait_child()");
        }

        locked_child.last_status.unwrap()
    }

    fn prepare_parent(&self) -> Result<(), SysError> {
        // Kernel will update slave pty as well.
        term::set_tty_mode(&self.master_fd.as_fd(), TtyMode::CanonNoEcho)?;

        if term::is_tty(&stdio::stdout()) {
            term::copy_tty_size(&self.master_fd.as_fd(), &stdio::stdout())?;
        }

        Ok(())
    }

    fn prepare_child(&self) -> Result<(), SysError> {
        // restore signal dispositions and mask
        signal::init_child_signals()?;

        // create new session and become session leader
        if let Err(err) = retry_on_intr(|| process::setsid()) {
            return Err(SysError("setsid()", err));
        }

        // set pty slave as controlling terminal
        if let Err(err) = retry_on_intr(|| process::ioctl_tiocsctty(&self.slave_fd)) {
            return Err(SysError("ioctl(TIOCSCTTY)", err));
        }

        // redirect stdin/stdout/stderr to pty slave
        for dup_fn in &[
            stdio::dup2_stdin::<&OwnedFd>,
            stdio::dup2_stdout::<&OwnedFd>,
            stdio::dup2_stderr::<&OwnedFd>,
        ] {
            if let Err(err) = retry_on_intr(|| dup_fn(&self.slave_fd)) {
                return Err(SysError("dup2()", err));
            }
        }

        // close file descriptors except stdin/stdout/stderr
        let max_fd = match sysconf(SysconfVariable::ScOpenMax) {
            Ok(n) => n,
            Err(_) => return Err(SysError("sysconf(_SC_OPEN_MAX)", Errno::INVAL)),
        };
        unsafe {
            for fd in 3..=max_fd {
                // SAFETY: this breaks invariants of opened OwnedFd, BorrowFd, etc.
                // However, we call this function right before exec(), in the context
                // where we guaranteedely have only one thread (after forking), so
                // these broken invariants don't have a chance to have any effect.
                shim::close_raw(fd as RawFd);
            }
        };

        Ok(())
    }
}
