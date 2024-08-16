// This file is part of the Tiny Cloud project.
// You can find the source code of every repository here:
//		https://github.com/personal-tiny-cloud
//
// Copyright (C) 2024  hex0x0000
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Email: hex0x0000@protonmail.com

use std::{env, ffi::CString, sync::OnceLock};

use libc::{
    closelog, openlog, syslog, LOG_DAEMON, LOG_DEBUG, LOG_ERR, LOG_INFO, LOG_NDELAY, LOG_PID,
    LOG_WARNING,
};
use log::{Level, LevelFilter};
use tokio::task;

static IS_INITIALIZED: OnceLock<()> = OnceLock::new();

fn is_initialized() -> bool {
    IS_INITIALIZED.get().is_some()
}

fn level_translate(level: Level) -> i32 {
    match level {
        Level::Error => LOG_ERR,
        Level::Warn => LOG_WARNING,
        Level::Info => LOG_INFO,
        Level::Trace | Level::Debug => LOG_DEBUG,
    }
}

fn init_blocking() {
    // Name of the program (const char *ident). This variable must be leaked because syslog() always
    // re-uses the same pointer given to openlog(). Dropping it would cause a dangling pointer
    // to be used during the entire execution of the program when sending logs to the system logger.
    let progname: &'static Option<CString> = Box::leak(Box::new(
        env::current_exe()
            .ok()
            .and_then(|e| {
                e.file_stem()
                    .and_then(|s| s.to_str().map(|s| s.to_string()))
            })
            .and_then(|s| CString::new(s).ok()),
    ));
    let ident = if let Some(progname) = progname {
        progname.as_ptr()
    } else {
        // Giving a NULL pointer will cause the logger to set the executable's name automatically.
        // But since it is not guaranteed to work on every platform, we still prefer the first option
        // and use this as a fallback.
        std::ptr::null()
    };
    unsafe {
        openlog(ident, LOG_PID | LOG_NDELAY, LOG_DAEMON);
    }
}

fn log_blocking(level: Level, log: String) {
    let level = level_translate(level);
    // '%' must be escaped because syslog formats its given arguments like printf.
    // Since everything is done Rust-side, we aren't giving any format argument to syslog.
    // Any unescaped '%' in the log may cause some dangerous UB.
    let log = CString::new(log.replace('%', "%%")).expect("CString::new failed.");
    unsafe {
        syslog(level, log.as_ptr());
    }
}

fn close_blocking() {
    unsafe {
        closelog();
    }
}

pub async fn init(level: LevelFilter) {
    if level != LevelFilter::Off && !is_initialized() {
        task::spawn_blocking(init_blocking)
            .await
            .unwrap_or_else(|e| panic!("Failed to init syslog: {e}"));
        IS_INITIALIZED
            .set(())
            .expect("Tried to initialize syslog twice.");
    }
}

pub async fn log(level: Level, log: String) {
    if is_initialized() {
        task::spawn_blocking(move || log_blocking(level, log))
            .await
            .unwrap_or_else(|e| panic!("Failed to log with syslog: {e}"));
    }
}

pub async fn close() {
    if is_initialized() {
        task::spawn_blocking(close_blocking)
            .await
            .unwrap_or_else(|e| panic!("Failed to close syslog: {e}"));
    }
}
