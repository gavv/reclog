use rustix::io::Errno;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SysError {
    #[error("{0}: {1}")]
    Syscall(&'static str, Errno),

    #[error("{0}")]
    Other(&'static str),
}
