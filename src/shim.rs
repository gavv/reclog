#![allow(clippy::unnecessary_cast)]

use libc::{self, FD_ISSET, FD_SET, FD_ZERO};
use rustix::io::Errno;
use rustix::process::{Pid, Signal};
use std::cmp::max;
use std::ffi::CStr;
use std::io::Error;
use std::mem::{self, MaybeUninit};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::ptr::null_mut;
use std::sync::Mutex;
use std::time::Duration;

/// Get errno from last libc call.
fn last_errno() -> Errno {
    Errno::from_io_error(&Error::last_os_error()).unwrap()
}

pub struct SelectFd<'fd> {
    pub fd: BorrowedFd<'fd>,
    pub mask: u32,
}

impl SelectFd<'_> {
    pub const READABLE: u32 = 0x1;
    pub const WRITEABLE: u32 = 0x2;
    pub const EXCEPTION: u32 = 0x4;
}

/// Safe shim for libc::select().
/// Handles EINTR.
pub fn select(select_fds: &mut [&mut SelectFd], timeout: Option<Duration>) -> Result<(), Errno> {
    let mut tv_timeout = timeout.map(|d| libc::timeval {
        tv_sec: d.as_secs() as libc::time_t,
        tv_usec: d.subsec_micros() as libc::suseconds_t,
    });

    let max_fd = select_fds
        .iter()
        .fold(0, |max_fd, sel_fd| max(max_fd, sel_fd.fd.as_raw_fd()));

    // SAFETY: We're holding an BorrowedFd (via SelectFd) for every descriptor
    // during the call, so they're guaranteed to be valid.
    //
    // NOTE: We use libc::select() instead of rustix::event::select() or
    // rustix::event::poll() because:
    //  - rustix::event::select() is not available on all platforms
    //  - rustix::event::poll() does not work with TTYs on macOS
    unsafe {
        let mut rd_fds = MaybeUninit::<libc::fd_set>::uninit();
        let mut wr_fds = MaybeUninit::<libc::fd_set>::uninit();
        let mut ex_fds = MaybeUninit::<libc::fd_set>::uninit();

        FD_ZERO(rd_fds.as_mut_ptr());
        FD_ZERO(wr_fds.as_mut_ptr());
        FD_ZERO(ex_fds.as_mut_ptr());

        rd_fds.assume_init();
        wr_fds.assume_init();
        ex_fds.assume_init();

        for sel_fd in select_fds.iter() {
            if sel_fd.mask & SelectFd::READABLE != 0 {
                FD_SET(sel_fd.fd.as_raw_fd(), rd_fds.as_mut_ptr());
            }
            if sel_fd.mask & SelectFd::WRITEABLE != 0 {
                FD_SET(sel_fd.fd.as_raw_fd(), wr_fds.as_mut_ptr());
            }
            if sel_fd.mask & SelectFd::EXCEPTION != 0 {
                FD_SET(sel_fd.fd.as_raw_fd(), ex_fds.as_mut_ptr());
            }
        }

        let mut nfds;

        loop {
            nfds = libc::select(
                max_fd + 1,
                rd_fds.as_mut_ptr(),
                wr_fds.as_mut_ptr(),
                ex_fds.as_mut_ptr(),
                if tv_timeout.is_some() {
                    tv_timeout.as_mut().unwrap() as *mut libc::timeval
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

        for sel_fd in select_fds.iter_mut() {
            sel_fd.mask = 0;
            if FD_ISSET(sel_fd.fd.as_raw_fd(), rd_fds.as_mut_ptr()) {
                sel_fd.mask |= SelectFd::READABLE;
            }
            if FD_ISSET(sel_fd.fd.as_raw_fd(), wr_fds.as_mut_ptr()) {
                sel_fd.mask |= SelectFd::WRITEABLE;
            }
            if FD_ISSET(sel_fd.fd.as_raw_fd(), ex_fds.as_mut_ptr()) {
                sel_fd.mask |= SelectFd::EXCEPTION;
            }
        }
    };

    Ok(())
}

/// Safe (in context of this program) shim for libc::ptsname().
pub fn ptsname<Fd: AsFd>(fd: Fd) -> Result<String, Errno> {
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
        let s_ptr = libc::ptsname(fd.as_fd().as_raw_fd());
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
pub fn read<Fd: AsFd>(fd: Fd, buf: &mut [u8]) -> Result<usize, Errno> {
    loop {
        let ret = unsafe {
            libc::read(
                fd.as_fd().as_raw_fd(),
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
pub fn write<Fd: AsFd>(fd: Fd, buf: &[u8]) -> Result<usize, Errno> {
    let mut pos = 0;
    while pos < buf.len() {
        let ret = unsafe {
            libc::write(
                fd.as_fd().as_raw_fd(),
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

/// Safe shim for fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK).
/// Handles EINTR.
pub fn fcntl_nonblock<Fd: AsFd>(fd: Fd, non_block: bool) -> Result<(), Errno> {
    loop {
        let mut flags = unsafe { libc::fcntl(fd.as_fd().as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            if last_errno() == Errno::INTR {
                continue;
            }
            return Err(last_errno());
        }

        if non_block {
            flags |= libc::O_NONBLOCK;
        } else {
            flags &= !libc::O_NONBLOCK;
        }

        let ret =
            unsafe { libc::fcntl(fd.as_fd().as_raw_fd(), libc::F_SETFL, flags as libc::c_uint) };
        if ret < 0 {
            if last_errno() == Errno::INTR {
                continue;
            }
            return Err(last_errno());
        }

        return Ok(());
    }
}

pub enum SigAction {
    Default,
    Ignore,
}

/// Safe shim for sigaction().
pub fn sigaction(sig: Signal, action: SigAction) -> Result<(), Errno> {
    let handler = match action {
        SigAction::Default => libc::SIG_DFL,
        SigAction::Ignore => libc::SIG_IGN,
    };

    let ret = unsafe {
        let mut sa: libc::sigaction = mem::zeroed();
        sa.sa_sigaction = handler;
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
        let mut sig_set: libc::sigset_t = mem::zeroed();
        libc::sigemptyset(&mut sig_set as *mut libc::sigset_t);
        for sig in sig_list {
            libc::sigaddset(
                &mut sig_set as *mut libc::sigset_t,
                sig.as_raw() as libc::c_int,
            );
        }

        #[cfg(has_pthread_sigmask)]
        {
            libc::pthread_sigmask(how, &mut sig_set as *mut libc::sigset_t, null_mut())
        }

        // If pthread_sigmask() isn't available, we fallback to sigprocmask().
        // POSIX does not specify how sigprocmask behaves in case of multiple threads.
        // If we're lucky, sigprocmask() works per-thread on this system (i.e. same as
        // pthread_sigmask), which is true at least on some platforms.
        #[cfg(not(has_pthread_sigmask))]
        {
            libc::sigprocmask(how, &mut sig_set as *mut libc::sigset_t, null_mut())
        }
    };
    if ret < 0 {
        return Err(last_errno());
    }

    Ok(())
}

/// Safe shim for sigwait() with optional timeout.
/// Uses sigtimedwait() or sigwaitinfo().
#[cfg(has_sigtimedwait)]
pub fn sigwait(sig_list: &[Signal], timeout: Option<Duration>) -> Result<Option<Signal>, Errno> {
    let mut ts_timeout = timeout.map(|d| libc::timespec {
        tv_sec: d.as_secs() as libc::time_t,
        tv_nsec: d.subsec_nanos() as i64,
    });

    let mut ret;
    loop {
        unsafe {
            let mut sig_set: libc::sigset_t = mem::zeroed();
            libc::sigemptyset(&mut sig_set as *mut libc::sigset_t);
            for sig in sig_list {
                libc::sigaddset(
                    &mut sig_set as *mut libc::sigset_t,
                    sig.as_raw() as libc::c_int,
                );
            }

            let mut sig_info: libc::siginfo_t = mem::zeroed();
            if ts_timeout.is_some() {
                ret = libc::sigtimedwait(
                    &mut sig_set as *mut libc::sigset_t,
                    &mut sig_info as *mut libc::siginfo_t,
                    ts_timeout.as_mut().unwrap() as *mut libc::timespec,
                );
            } else {
                ret = libc::sigwaitinfo(
                    &mut sig_set as *mut libc::sigset_t,
                    &mut sig_info as *mut libc::siginfo_t,
                )
            }
        };
        if ret < 0 {
            if last_errno() == Errno::AGAIN {
                // Timeout expired.
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

/// Safe shim for sigwait() with optional timeout.
/// Uses sigwait() and timer_create().
#[cfg(all(not(has_sigtimedwait), has_timer_create))]
pub fn sigwait(sig_list: &[Signal], timeout: Option<Duration>) -> Result<Option<Signal>, Errno> {
    // We use SIGALRM, which makes this function not usable from concurrent threads.
    static MUTEX: Mutex<()> = Mutex::new(());
    let _guard = MUTEX.lock();

    let ts_timeout = timeout.map(|d| libc::timespec {
        tv_sec: d.as_secs() as libc::time_t,
        tv_nsec: d.subsec_nanos() as i64,
    });

    let mut sig_no: libc::c_int = 0;
    loop {
        unsafe {
            // If zero timeout is given, call sigwait() only if signal is already pending.
            if timeout.is_some() && timeout.unwrap().is_zero() {
                let mut sig_set: libc::sigset_t = mem::zeroed();
                libc::sigemptyset(&mut sig_set as *mut libc::sigset_t);
                libc::sigpending(&mut sig_set as *mut libc::sigset_t);

                if !sig_list.iter().any(|sig| {
                    libc::sigismember(
                        &mut sig_set as *mut libc::sigset_t,
                        sig.as_raw() as libc::c_int,
                    ) == 1
                }) {
                    return Ok(None);
                }
            }

            // If positive timeout is given, set timer for SIGALRM.
            let mut timer: libc::timer_t = mem::zeroed();

            if timeout.is_some() && !timeout.unwrap().is_zero() {
                let mut timer_sig: libc::sigevent = mem::zeroed();
                timer_sig.sigev_notify = libc::SIGEV_SIGNAL;
                timer_sig.sigev_signo = libc::SIGALRM;

                if libc::timer_create(
                    libc::CLOCK_MONOTONIC,
                    &mut timer_sig as *mut libc::sigevent,
                    &mut timer as *mut libc::timer_t,
                ) < 0
                {
                    if last_errno() == Errno::AGAIN || last_errno() == Errno::INTR {
                        continue;
                    }
                    return Err(last_errno());
                }

                let mut timer_spec: libc::itimerspec = mem::zeroed();
                timer_spec.it_value = ts_timeout.unwrap();

                if libc::timer_settime(
                    timer,
                    0,
                    &mut timer_spec as *mut libc::itimerspec,
                    null_mut(),
                ) < 0
                {
                    if last_errno() == Errno::AGAIN || last_errno() == Errno::INTR {
                        continue;
                    }
                    return Err(last_errno());
                }
            }

            // Wait for signal.
            let mut sig_set: libc::sigset_t = mem::zeroed();
            libc::sigemptyset(&mut sig_set as *mut libc::sigset_t);
            for sig in sig_list {
                libc::sigaddset(
                    &mut sig_set as *mut libc::sigset_t,
                    sig.as_raw() as libc::c_int,
                );
            }
            if timeout.is_some() {
                libc::sigaddset(&mut sig_set as *mut libc::sigset_t, libc::SIGALRM);
            }

            let err = libc::sigwait(
                &mut sig_set as *mut libc::sigset_t,
                &mut sig_no as *mut libc::c_int,
            );

            if timeout.is_some() {
                // Delete timer.
                libc::timer_delete(timer);

                // Clear pending SIGALRM.
                let mut sig_set: libc::sigset_t = mem::zeroed();
                libc::sigemptyset(&mut sig_set as *mut libc::sigset_t);
                libc::sigpending(&mut sig_set as *mut libc::sigset_t);

                if libc::sigismember(&mut sig_set as *mut libc::sigset_t, libc::SIGALRM) == 1 {
                    libc::sigemptyset(&mut sig_set as *mut libc::sigset_t);
                    libc::sigaddset(&mut sig_set as *mut libc::sigset_t, libc::SIGALRM);

                    let mut ignore_sig = 0;
                    libc::sigwait(
                        &mut sig_set as *mut libc::sigset_t,
                        &mut ignore_sig as *mut libc::c_int,
                    );
                }
            }

            if err != 0 {
                if last_errno() == Errno::AGAIN || last_errno() == Errno::INTR {
                    continue;
                }
                return Err(Errno::from_raw_os_error(err));
            }
            if sig_no == libc::SIGALRM {
                return Ok(None); // timeout expired
            }
            break;
        }
    }

    match Signal::from_named_raw(sig_no) {
        Some(sig) => Ok(Some(sig)),
        None => Err(Errno::INVAL),
    }
}

/// Safe shim for sigwait() with optional timeout.
/// Uses sigwait() and setitimer().
#[cfg(all(not(has_sigtimedwait), not(has_timer_create)))]
pub fn sigwait(sig_list: &[Signal], timeout: Option<Duration>) -> Result<Option<Signal>, Errno> {
    // We use SIGALRM, which makes this function not usable from concurrent threads.
    static MUTEX: Mutex<()> = Mutex::new(());
    let _guard = MUTEX.lock();

    let tv_timeout = timeout.map(|d| libc::timeval {
        tv_sec: d.as_secs() as libc::time_t,
        tv_usec: d.subsec_micros() as libc::suseconds_t,
    });

    let mut sig_no: libc::c_int = 0;
    loop {
        unsafe {
            // If zero timeout is given, call sigwait() only if signal is already pending.
            if timeout.is_some() && timeout.unwrap().is_zero() {
                let mut sig_set: libc::sigset_t = mem::zeroed();
                libc::sigemptyset(&mut sig_set as *mut libc::sigset_t);
                libc::sigpending(&mut sig_set as *mut libc::sigset_t);

                if !sig_list.iter().any(|sig| {
                    libc::sigismember(
                        &mut sig_set as *mut libc::sigset_t,
                        sig.as_raw() as libc::c_int,
                    ) == 1
                }) {
                    return Ok(None);
                }
            }

            // If positive timeout is given, set timer for SIGALRM.
            if timeout.is_some() && !timeout.unwrap().is_zero() {
                let mut timer_val: libc::itimerval = mem::zeroed();
                timer_val.it_value = tv_timeout.unwrap();

                if libc::setitimer(
                    libc::ITIMER_REAL,
                    &mut timer_val as *mut libc::itimerval,
                    null_mut(),
                ) < 0
                {
                    if last_errno() == Errno::AGAIN || last_errno() == Errno::INTR {
                        continue;
                    }
                    return Err(last_errno());
                }
            }

            // Wait for signal.
            let mut sig_set: libc::sigset_t = mem::zeroed();
            libc::sigemptyset(&mut sig_set as *mut libc::sigset_t);
            for sig in sig_list {
                libc::sigaddset(
                    &mut sig_set as *mut libc::sigset_t,
                    sig.as_raw() as libc::c_int,
                );
            }
            if timeout.is_some() {
                libc::sigaddset(&mut sig_set as *mut libc::sigset_t, libc::SIGALRM);
            }

            let err = libc::sigwait(
                &mut sig_set as *mut libc::sigset_t,
                &mut sig_no as *mut libc::c_int,
            );

            if timeout.is_some() {
                // Clear timer.
                let mut timer_val: libc::itimerval = mem::zeroed();

                if libc::setitimer(
                    libc::ITIMER_REAL,
                    &mut timer_val as *mut libc::itimerval,
                    null_mut(),
                ) < 0
                {
                    if last_errno() == Errno::AGAIN || last_errno() == Errno::INTR {
                        continue;
                    }
                    return Err(last_errno());
                }

                // Clear pending SIGALRM.
                let mut sig_set: libc::sigset_t = mem::zeroed();
                libc::sigemptyset(&mut sig_set as *mut libc::sigset_t);
                libc::sigpending(&mut sig_set as *mut libc::sigset_t);

                if libc::sigismember(&mut sig_set as *mut libc::sigset_t, libc::SIGALRM) == 1 {
                    libc::sigemptyset(&mut sig_set as *mut libc::sigset_t);
                    libc::sigaddset(&mut sig_set as *mut libc::sigset_t, libc::SIGALRM);

                    let mut ignore_sig = 0;
                    libc::sigwait(
                        &mut sig_set as *mut libc::sigset_t,
                        &mut ignore_sig as *mut libc::c_int,
                    );
                }
            }

            if err != 0 {
                if last_errno() == Errno::AGAIN || last_errno() == Errno::INTR {
                    continue;
                }
                return Err(Errno::from_raw_os_error(err));
            }
            if sig_no == libc::SIGALRM {
                return Ok(None); // timeout expired
            }
            break;
        }
    }

    match Signal::from_named_raw(sig_no) {
        Some(sig) => Ok(Some(sig)),
        None => Err(Errno::INVAL),
    }
}
