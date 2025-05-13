use crate::error::SysError;
use crate::shim::{self, SelectFd};
use rustix::io::retry_on_intr;
use rustix::pipe;
use std::io::{Error, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex};

#[derive(PartialEq)]
enum WriterMode {
    Open,
    Closed,
}

/// Allows to write from fd in one thwrite and interrupt write or change
/// write timeout from another thwrite.
pub struct InterruptibleWriter<Fd: AsFd> {
    mode: Mutex<WriterMode>,
    fd: Fd,
    pipe_rd: OwnedFd,
    pipe_wr: OwnedFd,
}

impl<Fd: AsFd> InterruptibleWriter<Fd> {
    /// Construct new writer.
    /// Gains ownership of the fd.
    pub fn open(fd: Fd) -> Result<Self, SysError> {
        let (pipe_rd, pipe_wr) = match retry_on_intr(|| pipe::pipe()) {
            Ok(fds) => fds,
            Err(err) => return Err(SysError("pipe()", err)),
        };

        Ok(InterruptibleWriter {
            mode: Mutex::new(WriterMode::Open),
            fd,
            pipe_rd,
            pipe_wr,
        })
    }

    /// Close writer.
    /// Will wake up and abort ongoing writes.
    pub fn close(&self) -> Result<(), SysError> {
        {
            // update mode
            let mut locked_mode = self.mode.lock().unwrap();
            if *locked_mode == WriterMode::Closed {
                return Ok(());
            }
            *locked_mode = WriterMode::Closed;
        }

        // wake up and abort blocked write
        if let Err(err) = shim::write(&self.pipe_wr, &[0u8]) {
            return Err(SysError("write(pipe)", err));
        }

        Ok(())
    }

    /// Construct blocking writer.
    /// Waits until descriptor is writable, or writer is closed.
    pub fn blocking_writer(self: &Arc<Self>) -> ArcTimeoutWriter<Fd> {
        ArcTimeoutWriter(Arc::clone(self))
    }

    /// Invoked by ArcTimeoutWriter::write().
    fn write_imp(&self, buf: &[u8]) -> Result<usize, Error> {
        loop {
            // re-read mode
            {
                let locked_mode = self.mode.lock().unwrap();
                if *locked_mode == WriterMode::Closed {
                    // closed, silently discard all bytes
                    return Ok(buf.len());
                }
            };

            // wait until descriptor is ready
            let mut pipe_fd = SelectFd {
                fd: self.pipe_rd.as_fd(),
                mask: SelectFd::READABLE,
            };
            let mut data_fd = SelectFd {
                fd: self.fd.as_fd(),
                mask: SelectFd::WRITEABLE,
            };
            shim::select(&mut [&mut pipe_fd, &mut data_fd], None)?;

            if pipe_fd.mask != 0 {
                // wake up from close()
                // drain bytes from pipe
                _ = shim::read(&self.pipe_rd, &mut [0u8; 128]);
            }
            if data_fd.mask != 0 {
                // file is writeable
                break;
            }
        }

        // if we're here, file is writeable
        match shim::write(&self.fd, buf) {
            Ok(n) => Ok(n),
            Err(err) => Err(Error::new(err.kind(), err)),
        }
    }
}

/// Wrapper for Arc<TimeoutWriter> that implements Write trait.
pub struct ArcTimeoutWriter<Fd: AsFd>(Arc<InterruptibleWriter<Fd>>);

impl<Fd: AsFd> Write for ArcTimeoutWriter<Fd> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        self.0.write_imp(buf)
    }

    fn flush(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
