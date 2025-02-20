use crate::error::SysError;
use crate::shim::{select, SelectFd};
use rustix::io::{read, write};
use rustix::pipe::pipe;
use std::io::{Error, Read};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(PartialEq)]
enum Mode {
    Timeout(Duration),
    NoTimeout,
    Closed,
}

/// Allows to read from fd in one thread and interrupt read or change
/// read timeout from another thread.
pub struct InterruptibleReader<Fd: AsFd> {
    mode: Mutex<Mode>,
    fd: Fd,
    pipe_rd: OwnedFd,
    pipe_wr: OwnedFd,
}

impl<Fd: AsFd> InterruptibleReader<Fd> {
    /// Construct new reader.
    /// Gains ownership of the fd.
    pub fn new(fd: Fd) -> Result<Self, SysError> {
        let (pipe_rd, pipe_wr) = match pipe() {
            Ok(fds) => fds,
            Err(err) => return Err(SysError::Syscall("pipe()", err)),
        };

        Ok(InterruptibleReader {
            mode: Mutex::new(Mode::NoTimeout),
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
            *locked_mode = Mode::Closed;
        }

        // wake up and abort blocked read
        if let Err(err) = write(&self.pipe_wr, &[0u8]) {
            return Err(SysError::Syscall("write(pipe)", err));
        }

        Ok(())
    }

    /// Set read timeout.
    /// Will wake up and restart ongoing reads.
    pub fn set_timeout(&self, duration: Duration) -> Result<(), SysError> {
        {
            // update mode
            let mut locked_mode = self.mode.lock().unwrap();
            if *locked_mode == Mode::Closed {
                return Err(SysError::Other("already closed"));
            }
            *locked_mode = Mode::Timeout(duration);
        }

        // wake up and restart blocked read
        if let Err(err) = write(&self.pipe_wr, &[0u8]) {
            return Err(SysError::Syscall("write(pipe)", err));
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
                    Mode::Timeout(d) => Some(d),
                    // read without timeout
                    Mode::NoTimeout => None,
                    // closeed, return EOF
                    Mode::Closed => {
                        return Ok(0);
                    }
                }
            };

            // wait until descriptor is ready or timeout expires
            let mut pipe_fd = SelectFd {
                fd: self.pipe_rd.as_fd(),
                ready: false,
            };
            let mut data_fd = SelectFd {
                fd: self.fd.as_fd(),
                ready: false,
            };
            select(&mut [&mut pipe_fd, &mut data_fd], timeout)?;

            if pipe_fd.ready {
                // wake up from set_timeout() or close()
                // drain bytes from pipe
                _ = read(&self.pipe_rd, &mut [0u8; 128]);
            }
            if data_fd.ready {
                // data from file
                break;
            }

            if !pipe_fd.ready && !data_fd.ready && timeout.is_some() {
                // timeout expired, return EOF
                return Ok(0);
            }
        }

        // if we're here, there is new data in file
        match read(&self.fd, buf) {
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
