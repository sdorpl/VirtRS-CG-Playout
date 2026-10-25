// Log lines as the Node.js sidecar wrote them: `<ISO time> LEVEL message`,
// INFO and DEBUG on stdout, WARN and ERROR on stderr; DEBUG only with DEBUG=1.

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::time;

static DEBUG: AtomicBool = AtomicBool::new(false);

pub fn set_debug(on: bool) {
    DEBUG.store(on, Ordering::Relaxed);
}

pub fn debug_enabled() -> bool {
    DEBUG.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

pub fn write(level: Level, message: std::fmt::Arguments<'_>) {
    let (name, to_stderr) = match level {
        Level::Debug if !debug_enabled() => return,
        Level::Debug => ("DEBUG", false),
        Level::Info => ("INFO", false),
        Level::Warn => ("WARN", true),
        Level::Error => ("ERROR", true),
    };
    let line = format!("{} {name} {message}\n", time::iso_time(time::now_ms()));
    // A closed stdout/stderr must not take the sidecar down.
    let _ = if to_stderr { std::io::stderr().write_all(line.as_bytes()) } else { std::io::stdout().write_all(line.as_bytes()) };
}

#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Debug, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Info, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Warn, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Error, format_args!($($arg)*)) };
}
