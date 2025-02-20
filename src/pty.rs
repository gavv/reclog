use crate::error::SysError;
use crate::shim::{self, Fork};
use crate::status::*;
use crate::term::{self, InputMode};
use exec::Command;
use rustix::fs::{self, Mode, OFlags};
use rustix::io::Errno;
use rustix::process::{self, Pid, WaitOptions, WaitStatus};
use rustix::pty::{self, OpenptFlags};
use rustix::stdio;
use std::os::fd::{AsFd, OwnedFd, RawFd};
use std::path::Path;
use std::sync::Mutex;
use sysconf::raw::{sysconf, SysconfVariable};

/// Allows to create PTY pair and spawn child process.
/// I haven't found existing create for PTY that would allow keeping slave_fd
/// opened in parent, which we need to properly read pending data after child
/// exits. Hence we have our own implementation.
pub struct PtyProc {
    master_fd: OwnedFd,
    slave_fd: OwnedFd,
    child: Mutex<Child>,
}

struct Child {
    pid: Option<Pid>,
    status: Option<WaitStatus>,
}

impl PtyProc {
    /// Open master/slave pair.
    pub fn open() -> Result<Self, SysError> {
        // open master pty
        let master_fd = match pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY) {
            Ok(fd) => fd,
            Err(err) => return Err(SysError::Syscall("openpt()", err)),
        };

        // unlock slave pty
        if let Err(err) = pty::grantpt(&master_fd) {
            return Err(SysError::Syscall("grantpt()", err));
        }
        if let Err(err) = pty::unlockpt(&master_fd) {
            return Err(SysError::Syscall("unlockpt()", err));
        }

        // open slave pty
        let pts_name = match shim::ptsname(master_fd.as_fd()) {
            Ok(s) => s,
            Err(err) => return Err(SysError::Syscall("ptsname()", err)),
        };
        let slave_fd = match fs::open(
            Path::new(&pts_name),
            OFlags::RDWR | OFlags::NOCTTY,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(err) => return Err(SysError::Syscall("open()", err)),
        };

        Ok(PtyProc {
            master_fd,
            slave_fd,
            child: Mutex::new(Child {
                pid: None,
                status: None,
            }),
        })
    }

    /// Duplicate master fd.
    pub fn dup_fd(&self) -> Result<OwnedFd, SysError> {
        self.master_fd
            .try_clone()
            .map_err(|err| SysError::Syscall("dup()", Errno::from_io_error(&err).unwrap()))
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
                    return Err(SysError::Syscall("fork()", err));
                }
            }
        };

        Ok(())
    }

    /// Wait until spawned child exits.
    pub fn wait_child(&self) -> Result<(), SysError> {
        let mut locked_child = self.child.lock().unwrap();

        if !locked_child.pid.is_some() {
            panic!("attempt to call wait_child() before spawn_child()");
        }
        if locked_child.status.is_some() {
            panic!("attempt to call wait_child() twice");
        }

        loop {
            let wait_status = match process::waitpid(locked_child.pid, WaitOptions::empty()) {
                Ok(Some((_, status))) => status,
                Ok(None) => return Err(SysError::Other("waitpid() failed")),
                Err(err) => return Err(SysError::Syscall("waitpid()", err)),
            };

            if wait_status.exited() || wait_status.signaled() {
                locked_child.status = Some(wait_status);
                return Ok(());
            }
        }
    }

    /// Get child exit status.
    pub fn child_status(&self) -> WaitStatus {
        let locked_child = self.child.lock().unwrap();

        if !locked_child.status.is_some() {
            panic!("attempt to call child_status() before wait_child()");
        }

        locked_child.status.unwrap()
    }

    fn prepare_parent(&self) -> Result<(), SysError> {
        term::set_input_mode(&self.slave_fd.as_fd(), InputMode::CanonNoEcho)?;
        term::copy_window_size(&self.slave_fd.as_fd(), &stdio::stdout())?;

        Ok(())
    }

    fn prepare_child(&self) -> Result<(), SysError> {
        // create new session and become session leader
        if let Err(err) = process::setsid() {
            return Err(SysError::Syscall("setsid()", err));
        }

        // set pty slave as controlling terminal
        if let Err(err) = process::ioctl_tiocsctty(&self.slave_fd) {
            return Err(SysError::Syscall("ioctl(TIOCSCTTY)", err));
        }

        // redirect stdin/stdout/stderr to pty slave
        for dup_fn in &[
            stdio::dup2_stdin::<&OwnedFd>,
            stdio::dup2_stdout::<&OwnedFd>,
            stdio::dup2_stderr::<&OwnedFd>,
        ] {
            if let Err(err) = dup_fn(&self.slave_fd) {
                return Err(SysError::Syscall("dup2()", err));
            }
        }

        // close file descriptors except stdin/stdout/stderr
        let max_fd = match sysconf(SysconfVariable::ScOpenMax) {
            Ok(n) => n,
            Err(_) => return Err(SysError::Syscall("sysconf(_SC_OPEN_MAX)", Errno::INVAL)),
        };
        unsafe {
            for fd in 3..=max_fd {
                // SAFETY: this breaks invariants of opened OwnedFd, BorrowFd, etc.
                // However, we call this function right before exec(), in the context
                // where we guaranteedely have only one thread (after forking), so
                // these broken invariants don't have a chance to have any effect.
                libc::close(fd as RawFd);
            }
        };

        Ok(())
    }
}
