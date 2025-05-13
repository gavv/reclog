#![allow(clippy::unnecessary_cast)]

use libc::{self, fd_set, suseconds_t, time_t, timespec, timeval};
use libc::{FD_ISSET, FD_SET, FD_ZERO};
use rustix::io::Errno;
use rustix::process::{Pid, Signal};
use rustix::thread;
use std::cmp::max;
use std::ffi::CStr;
use std::io::Error;
use std::mem::{self, MaybeUninit};
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::ptr::null_mut;
use std::sync::Mutex;
use std::time::Duration;

/// Get errno from last libc call.
fn last_errno() -> Errno {
    Errno::from_io_error(&Error::last_os_error()).unwrap()
}

/// Get current thread ID.
pub fn gettid() -> i64 {
    thread::gettid().as_raw_nonzero().get() as i64
}

pub struct SelectFd<'fd> {
    pub fd: BorrowedFd<'fd>,
    pub ready: bool,
}

/// Safe shim for libc::select().
/// Handles EINTR.
pub fn select(select_fds: &mut [&mut SelectFd], timeout: Option<Duration>) -> Result<(), Errno> {
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

        let mut nfds;

        loop {
            nfds = libc::select(
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
                if last_errno() == Errno::INTR {
                    continue;
                }
                return Err(last_errno());
            }
            break;
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

/// Safe shim for libc::read().
/// Handles EINTR.
/// Unlike rustix version, doesn't consume the buffer.
pub fn read(fd: BorrowedFd, buf: &mut [u8]) -> Result<usize, Errno> {
    loop {
        let ret = unsafe {
            libc::read(
                fd.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
        if ret < 0 {
            if last_errno() == Errno::INTR {
                continue;
            }
            return Err(last_errno());
        }
        return Ok(ret as usize);
    }
}

/// Safe shim for libc::write().
/// Handles EINTR.
/// Handles partial writes.
pub fn write(fd: BorrowedFd, buf: &[u8]) -> Result<usize, Errno> {
    let mut pos = 0;
    while pos < buf.len() {
        let ret = unsafe {
            libc::write(
                fd.as_raw_fd(),
                buf[pos..].as_ptr() as *mut libc::c_void,
                buf.len() - pos,
            )
        };
        if ret < 0 {
            if last_errno() == Errno::INTR {
                continue;
            }
            return Err(last_errno());
        }
        if ret == 0 {
            break;
        }
        pos += ret as usize;
    }
    Ok(pos)
}

/// Shim for libc::close().
/// It violates OwnedFd/BorrowedFd contract by making it possible to close underlying
/// fd while it's still owned, hence marked unsafe.
/// Handles EINTR.
pub unsafe fn close_raw(fd: RawFd) {
    loop {
        if unsafe { libc::close(fd) } == 0 || last_errno() != Errno::INTR {
            break;
        }
    }
}

pub enum SigAction {
    Default,
    Ignore,
}

/// Safe shim for sigaction().
pub fn sigaction(sig: Signal, action: SigAction) -> Result<(), Errno> {
    let hnd = match action {
        SigAction::Default => libc::SIG_DFL,
        SigAction::Ignore => libc::SIG_IGN,
    };

    let ret = unsafe {
        let mut sa: libc::sigaction = mem::zeroed();
        sa.sa_sigaction = hnd;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigfillset(&mut sa.sa_mask as *mut libc::sigset_t);

        libc::sigaction(sig.as_raw(), &sa, null_mut())
    };
    if ret < 0 {
        return Err(last_errno());
    }

    Ok(())
}

pub enum SigMask {
    Block,
    Unblock,
}

/// Safe shim for pthread_sigmask().
pub fn sigmask(sig_list: &[Signal], action: SigMask) -> Result<(), Errno> {
    let how = match action {
        SigMask::Block => libc::SIG_BLOCK,
        SigMask::Unblock => libc::SIG_UNBLOCK,
    };

    let ret = unsafe {
        let mut sm: libc::sigset_t = mem::zeroed();
        for sig in sig_list {
            libc::sigaddset(&mut sm as *mut libc::sigset_t, sig.as_raw() as libc::c_int);
        }

        libc::pthread_sigmask(how, &mut sm as *mut libc::sigset_t, null_mut())
    };
    if ret < 0 {
        return Err(last_errno());
    }

    Ok(())
}

/// Safe shim for sigwait().
pub fn sigwait(sig_list: &[Signal], timeout: Option<Duration>) -> Result<Option<Signal>, Errno> {
    let mut ts_timeout = timeout.map(|d| timespec {
        tv_sec: d.as_secs() as time_t,
        tv_nsec: d.subsec_nanos() as i64,
    });

    let mut ret;
    loop {
        unsafe {
            let mut sm: libc::sigset_t = mem::zeroed();
            for sig in sig_list {
                libc::sigaddset(&mut sm as *mut libc::sigset_t, sig.as_raw() as libc::c_int);
            }

            let mut sig_info: libc::siginfo_t = mem::zeroed();
            if ts_timeout.is_some() {
                ret = libc::sigtimedwait(
                    &mut sm as *mut libc::sigset_t,
                    &mut sig_info as *mut libc::siginfo_t,
                    ts_timeout.as_mut().unwrap() as *mut timespec,
                );
            } else {
                ret = libc::sigwaitinfo(
                    &mut sm as *mut libc::sigset_t,
                    &mut sig_info as *mut libc::siginfo_t,
                )
            }
        };
        if ret < 0 {
            if last_errno() == Errno::AGAIN {
                return Ok(None);
            }
            if last_errno() == Errno::INTR {
                continue;
            }
            return Err(last_errno());
        }
        break;
    }

    let sig_no = ret as i32;
    match Signal::from_named_raw(sig_no) {
        Some(sig) => Ok(Some(sig)),
        None => Err(Errno::INVAL),
    }
}
