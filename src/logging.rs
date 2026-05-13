use std::io::{self, Write};
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Error = 0,
    Info = 1,
    Debug = 2,
    Trace = 3,
}

static LOG_LEVEL: AtomicU8 = AtomicU8::new(LogLevel::Info as u8);

pub fn set_level(level: LogLevel) {
    LOG_LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn level() -> LogLevel {
    match LOG_LEVEL.load(Ordering::Relaxed) {
        0 => LogLevel::Error,
        1 => LogLevel::Info,
        2 => LogLevel::Debug,
        _ => LogLevel::Trace,
    }
}

pub fn parse_level(value: &str) -> Option<LogLevel> {
    match value.to_ascii_lowercase().as_str() {
        "error" => Some(LogLevel::Error),
        "info" => Some(LogLevel::Info),
        "debug" => Some(LogLevel::Debug),
        "trace" => Some(LogLevel::Trace),
        _ => None,
    }
}

pub fn error(message: impl AsRef<str>) {
    log(LogLevel::Error, message.as_ref());
}

pub fn info(message: impl AsRef<str>) {
    log(LogLevel::Info, message.as_ref());
}

pub fn debug(message: impl AsRef<str>) {
    log(LogLevel::Debug, message.as_ref());
}

pub fn trace(message: impl AsRef<str>) {
    log(LogLevel::Trace, message.as_ref());
}

pub fn http_request(method: &str, endpoint: &str) {
    debug(format!("HTTP {method} {endpoint}"));
}

pub fn http_success(method: &str, endpoint: &str, status: reqwest::StatusCode) {
    debug(format!("HTTP {method} {endpoint} -> {}", status.as_u16()));
}

pub fn http_trace_response(method: &str, endpoint: &str, body: &str) {
    trace(format!("HTTP {method} {endpoint} response body:\n{body}"));
}

pub fn http_error(method: &str, endpoint: &str, message: impl AsRef<str>) {
    error(format!("HTTP {method} {endpoint}: {}", message.as_ref()));
}

fn log(level: LogLevel, message: &str) {
    if level > self::level() {
        return;
    }

    let mut stream: Box<dyn Write> = match level {
        LogLevel::Error => Box::new(io::stderr().lock()),
        LogLevel::Info | LogLevel::Debug | LogLevel::Trace => Box::new(io::stdout().lock()),
    };
    let prefix = match level {
        LogLevel::Error => "ERROR",
        LogLevel::Info => "INFO",
        LogLevel::Debug => "DEBUG",
        LogLevel::Trace => "TRACE",
    };
    let _ = writeln!(stream, "[{prefix}] {message}");
}
