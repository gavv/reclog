use rustix::io::Errno;
use std::fmt;

#[derive(Debug)]
pub struct SysError(pub &'static str, pub Errno);

impl fmt::Display for SysError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}: {}", self.0, self.1)
    }
}
