// These constants follow bash conventions for exit codes.
// They are not standartizied, but are quite common.

/// General error.
/// E.g. resource not available, permission denined, etc.
pub const EXIT_FAILURE: i32 = 1;

/// Invalid usage.
/// E.g. missing required option.
pub const EXIT_USAGE: i32 = 2;

/// Command invoked cannot execute.
/// E.g. execvp() returned error.
pub const EXIT_COMMAND_FAILED: i32 = 126;

/// Command killed by signal.
/// The actual exit code is EXIT_COMMAND_SIGNALED + N, where
/// N is the signal number.
pub const EXIT_COMMAND_SIGNALED: i32 = 128;
