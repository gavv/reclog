use crate::error::SysError;
use crate::shim::{self, SelectFd};
use rustix::io::retry_on_intr;
use rustix::pipe;
use std::io::{Error, Read};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(PartialEq)]
enum ReaderMode {
    Timeout(Duration),
    NoTimeout,
    Closed,
}

/// Allows to read from fd in one thread and interrupt read or change
/// read timeout from another thread.
pub struct InterruptibleReader<Fd: AsFd> {
    mode: Mutex<ReaderMode>,
    fd: Fd,
    pipe_rd: OwnedFd,
    pipe_wr: OwnedFd,
}

impl<Fd: AsFd> InterruptibleReader<Fd> {
    /// Construct new reader.
    /// Gains ownership of the fd.
    pub fn open(fd: Fd) -> Result<Self, SysError> {
        let (pipe_rd, pipe_wr) = match retry_on_intr(|| pipe::pipe()) {
            Ok(fds) => fds,
            Err(err) => return Err(SysError("pipe()", err)),
        };

        Ok(InterruptibleReader {
            mode: Mutex::new(ReaderMode::NoTimeout),
            fd,
            pipe_rd,
            pipe_wr,
        })
    }

    /// Close reader.
    /// Will wake up and abort ongoing reads.
    pub fn close(&self) -> Result<(), SysError> {
        {
            // update mode
            let mut locked_mode = self.mode.lock().unwrap();
            if *locked_mode == ReaderMode::Closed {
                return Ok(());
            }
            *locked_mode = ReaderMode::Closed;
        }

        // wake up and abort blocked read
        if let Err(err) = shim::write(&self.pipe_wr, &[0u8]) {
            return Err(SysError("write(pipe)", err));
        }

        Ok(())
    }

    /// Set read timeout.
    /// Will wake up and restart ongoing reads.
    pub fn set_timeout(&self, duration: Duration) -> Result<(), SysError> {
        {
            // update mode
            let mut locked_mode = self.mode.lock().unwrap();
            if *locked_mode == ReaderMode::Closed {
                return Ok(());
            }
            *locked_mode = ReaderMode::Timeout(duration);
        }

        // wake up and restart blocked read
        if let Err(err) = shim::write(&self.pipe_wr, &[0u8]) {
            return Err(SysError("write(pipe)", err));
        }

        Ok(())
    }

    /// Construct blocking reader.
    /// Waits until there is *some* data, OR reader is closed, OR read timeout
    /// is set and expires.
    pub fn blocking_reader(self: &Arc<Self>) -> ArcTimeoutReader<Fd> {
        ArcTimeoutReader(Arc::clone(self))
    }

    /// Invoked by ArcTimeoutReader::read().
    fn read_imp(&self, buf: &mut [u8]) -> Result<usize, Error> {
        loop {
            // re-read mode
            let timeout = {
                let locked_mode = self.mode.lock().unwrap();
                match *locked_mode {
                    // read with timeout
                    ReaderMode::Timeout(d) => Some(d),
                    // read without timeout
                    ReaderMode::NoTimeout => None,
                    // closeed, return EOF
                    ReaderMode::Closed => {
                        return Ok(0);
                    }
                }
            };

            // wait until descriptor is ready or timeout expires
            let mut pipe_fd = SelectFd {
                fd: self.pipe_rd.as_fd(),
                mask: SelectFd::READABLE,
            };
            let mut data_fd = SelectFd {
                fd: self.fd.as_fd(),
                mask: SelectFd::READABLE,
            };
            shim::select(&mut [&mut pipe_fd, &mut data_fd], timeout)?;

            if pipe_fd.mask != 0 {
                // wake up from set_timeout() or close()
                // drain bytes from pipe
                _ = shim::read(&self.pipe_rd, &mut [0u8; 128]);
            }
            if data_fd.mask != 0 {
                // data from file
                break;
            }

            if pipe_fd.mask == 0 && data_fd.mask == 0 && timeout.is_some() {
                // timeout expired, return EOF
                return Ok(0);
            }
        }

        // if we're here, there is new data in file
        match shim::read(&self.fd, buf) {
            Ok(n) => Ok(n),
            Err(err) => Err(Error::new(err.kind(), err)),
        }
    }
}

/// Wrapper for Arc<TimeoutReader> that implements Read trait.
pub struct ArcTimeoutReader<Fd: AsFd>(Arc<InterruptibleReader<Fd>>);

impl<Fd: AsFd> Read for ArcTimeoutReader<Fd> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        self.0.read_imp(buf)
    }
}
