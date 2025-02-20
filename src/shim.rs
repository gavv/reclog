use libc::{self, fd_set, suseconds_t, time_t, timeval};
use libc::{FD_ISSET, FD_SET, FD_ZERO};
use rustix::io::Errno;
use rustix::process::Pid;
use std::cmp::max;
use std::ffi::CStr;
use std::io::Error;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::ptr::null_mut;
use std::sync::Mutex;
use std::time::Duration;

pub struct SelectFd<'fd> {
    pub fd: BorrowedFd<'fd>,
    pub ready: bool,
}

/// Safe shim for libc::select().
pub fn select(select_fds: &mut [&mut SelectFd], timeout: Option<Duration>) -> Result<(), Error> {
    let mut tv_timeout = timeout.map(|d| timeval {
        tv_sec: d.as_secs() as time_t,
        tv_usec: d.subsec_micros() as suseconds_t,
    });

    let max_fd = select_fds
        .iter()
        .fold(0, |max_fd, wfd| max(max_fd, wfd.fd.as_raw_fd()));

    // SAFETY: We're holding an BorrowedFd (via SelectFd) for every descriptor
    // during the call, so they're guaranteed to be valid.
    //
    // NOTE: We use libc::select() instead of rustix::event::select() or
    // rustix::event::poll() because:
    //  - rustix::event::select() is not available on all platforms
    //  - rustix::event::poll() does not work with TTYs on macOS
    unsafe {
        let mut fds = MaybeUninit::<fd_set>::uninit();
        FD_ZERO(fds.as_mut_ptr());
        fds.assume_init();

        for wfd in select_fds.iter() {
            FD_SET(wfd.fd.as_raw_fd(), fds.as_mut_ptr());
        }

        let nfds = libc::select(
            max_fd + 1,
            fds.as_mut_ptr(),
            null_mut(),
            null_mut(),
            if tv_timeout.is_some() {
                tv_timeout.as_mut().unwrap() as *mut timeval
            } else {
                null_mut()
            },
        );
        if nfds < 0 {
            return Err(Error::last_os_error());
        }

        for wfd in select_fds.iter_mut() {
            if FD_ISSET(wfd.fd.as_raw_fd(), fds.as_mut_ptr()) {
                wfd.ready = true;
            }
        }
    };

    Ok(())
}

/// Safe (in context of this program) shim for libc::ptsname().
pub fn ptsname(fd: BorrowedFd) -> Result<String, Errno> {
    // SAFETY: ptsname() returns a pointer to static storage and hence is not
    // thread-safe. However, `reclog` is a program, not a library, and we know
    // in advance that this is the only place that calls it. The guard below
    // guarantees that the shim is not running concurrently.
    //
    // NOTE: We use libc::ptsname() instead of rustix::pty::ptsname() because
    // the latter is not available on all platforms. Rustix version works
    // only on platforms where non-standard thread-safe versions are available
    // (such as ptsname_r), but we don't want to limit supported platforms.
    static MUTEX: Mutex<()> = Mutex::new(());
    let _guard = MUTEX.lock();

    let s_ref = unsafe {
        let s_ptr = libc::ptsname(fd.as_raw_fd());
        if s_ptr.is_null() {
            return Err(last_errno());
        }

        CStr::from_ptr(s_ptr).to_str().unwrap()
    };

    Ok(s_ref.to_string())
}

pub enum Fork {
    Parent(Pid),
    Child,
}

/// Convenience shim for libc::fork().
/// In Rust, fork() is not safe in general case, only its specific usages can be proven so.
/// Hence we mark shim as unsafe, and leave the safe usage as responsibility of the caller.
pub unsafe fn fork() -> Result<Fork, Errno> {
    match unsafe { libc::fork() } {
        pid if pid > 0 => Ok(Fork::Parent(Pid::from_raw(pid).unwrap())),
        0 => Ok(Fork::Child),
        _ => Err(last_errno()),
    }
}

/// Shim for libc::_exit().
/// It's like process::exit(), but it doesn't run atexit handlers or any other destructors,
/// just kills the process immediately.
/// While it's not really unsafe, we still mark it so, to make its usage bolder in code
/// when implementing safe use of fork().
pub unsafe fn fast_exit(code: i32) {
    unsafe { libc::_exit(code) }
}

/// Get errno from last libc call.
fn last_errno() -> Errno {
    Errno::from_io_error(&Error::last_os_error()).unwrap()
}
