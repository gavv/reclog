use crate::error::SysError;
use rustix::io::retry_on_intr;
use rustix::termios::{self, LocalModes, OptionalActions, SpecialCodeIndex, Termios};
use std::io::{Error, LineWriter, Write};
use std::os::fd::BorrowedFd;
use std::slice;

/// Check if descriptor is a tty.
pub fn is_tty(fd: &BorrowedFd) -> bool {
    termios::isatty(fd)
}

/// Input mode of a tty.
pub enum TtyMode {
    Canon,
    CanonNoEcho,
}

/// Enable canonical mode w/o echo.
pub fn set_tty_mode(tty_fd: &BorrowedFd, mode: TtyMode) -> Result<(), SysError> {
    let mut term = match retry_on_intr(|| termios::tcgetattr(tty_fd)) {
        Ok(term) => term,
        Err(err) => return Err(SysError("tcgetattr()", err)),
    };

    match mode {
        TtyMode::Canon => term.local_modes |= LocalModes::ICANON,
        TtyMode::CanonNoEcho => {
            term.local_modes |= LocalModes::ICANON;
            term.local_modes &= !LocalModes::ECHO;
        }
    };

    if let Err(err) = retry_on_intr(|| termios::tcsetattr(tty_fd, OptionalActions::Now, &term)) {
        return Err(SysError("tcsetattr()", err));
    }

    Ok(())
}

/// Table of tty special codes.
#[allow(non_snake_case)]
pub struct TtyCodes {
    pub VEOF: char,
}

/// Enable canonical mode w/o echo.
pub fn get_tty_codes(tty_fd: &BorrowedFd) -> Result<TtyCodes, SysError> {
    let term = match retry_on_intr(|| termios::tcgetattr(tty_fd)) {
        Ok(term) => term,
        Err(err) => return Err(SysError("tcgetattr()", err)),
    };

    let codes = TtyCodes {
        VEOF: term.special_codes[SpecialCodeIndex::VEOF] as char,
    };

    Ok(codes)
}

/// Copy win size from src to dst.
pub fn copy_tty_size(dst_tty_fd: &BorrowedFd, src_tty_fd: &BorrowedFd) -> Result<(), SysError> {
    let win_size = match retry_on_intr(|| termios::tcgetwinsize(src_tty_fd)) {
        Ok(win_size) => win_size,
        Err(err) => return Err(SysError("tcgetwinsize()", err)),
    };

    if let Err(err) = retry_on_intr(|| termios::tcsetwinsize(dst_tty_fd, win_size)) {
        return Err(SysError("tcsetwinsize()", err));
    }

    Ok(())
}

/// Save tty state into a variable.
pub fn save_tty_state(tty_fd: &BorrowedFd) -> Result<Termios, SysError> {
    match retry_on_intr(|| termios::tcgetattr(tty_fd)) {
        Ok(term) => Ok(term),
        Err(err) => Err(SysError("tcgetattr()", err)),
    }
}

/// Restore tty state from a variable.
pub fn restore_tty_state(tty_fd: &BorrowedFd, term: &Termios) -> Result<(), SysError> {
    if let Err(err) = retry_on_intr(|| termios::tcsetattr(tty_fd, OptionalActions::Now, term)) {
        return Err(SysError("tcsetattr()", err));
    }
    Ok(())
}

/// Wrapper writer that strips ANSI escape codes from text and passes the
/// stripped text to the underlying writer.
/// Use of full-fledged VTE parser (from `vte` crate) instead of a naive
/// regex allows to handle complicates cases e.g. when we need to remove
/// a range of text surrounded by special pair of codes.
pub struct AnsiStripper<W: Write> {
    parser: vte::Parser,
    performer: AnsiPerformer<W>,
}

impl<W: Write> AnsiStripper<W> {
    pub fn new(output: W) -> Self {
        AnsiStripper {
            parser: vte::Parser::new(),
            performer: AnsiPerformer {
                line_writer: LineWriter::new(output),
                last_err: None,
            },
        }
    }
}

impl<W: Write> Write for AnsiStripper<W> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        // We write bytes to parser, parse invokes performer,
        // performer writes bytes to output vector.
        self.parser.advance(&mut self.performer, buf);

        if let Some(err) = self.performer.last_err.take() {
            return Err(err);
        }

        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), Error> {
        self.performer.line_writer.flush()
    }
}

/// Implements callbacks for vte::Parser.
struct AnsiPerformer<W: Write> {
    line_writer: LineWriter<W>,
    last_err: Option<Error>,
}

impl<W: Write> vte::Perform for AnsiPerformer<W> {
    /// Called for each regular character.
    fn print(&mut self, c: char) {
        // Write all regular characters as-is.
        self.last_err = self
            .line_writer
            .write_all(slice::from_ref(&(c as u8)))
            .err();
    }

    /// Called for each special character.
    fn execute(&mut self, b: u8) {
        // Handle only selected special characters and ignore others.
        if b == b'\t' || b == b'\n' {
            self.last_err = self.line_writer.write_all(slice::from_ref(&b)).err();
        }
    }

    // For all other sequences, keep default no-op implementation
    // from vte::Perform trait.
}
