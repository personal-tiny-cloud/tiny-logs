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

#![warn(missing_docs)]
//! # Simple logger for Tiny Cloud.
//!
//! This is [Tiny Cloud](https://github.com/personal-tiny-cloud/tiny-cloud)'s logger.
//!
//! tiny-logs uses [`tokio`] as a backend to log files asynchronously to avoid blocking operations
//! (like writing to files or to stdout) during the execution of the program.
//!
//! It is also able to log by using libc's [syslog](https://www.man7.org/linux/man-pages/man3/openlog.3.html)
//! (feature `syslog`). Since it is part of the Standard C Library, it should cover most platforms.
//! On Linux, logs made with `syslog()` will be saved to either syslogd or systemd-journald (depending on the system).
//!
//! This feature is useful for low-spec hardware where logging to files might slow down the
//! system. For example, Raspberry Pis usually prefer using RAM logs for their system logs,
//! due to their slow writing speed. It might not be useful on systems with systemd, because the
//! standard output of every daemon is logged by default.
//!
//! It implements [`log::Log`] of the [`log`] crate. Logging has to be done by using its macros.
//!
//! # Usage
//!
//! ```rust
//! # tokio_test::block_on(async {
//! # let tmp = tempfile::NamedTempFile::new().unwrap();
//! # let path_to_logfile = tmp.path().as_os_str().to_str().unwrap().to_string();
//! use log::LevelFilter;
//!
//! // 1. Level filter for the terminal's output.
//! // 2. Path to a log file.
//! // 3. Level filter of the log file.
//! // 4. (feature 'syslog') Level filter of the system logger.
//! // Remember to show the error to the user if there's any.
//! tiny_logs::init(
//!     LevelFilter::Info,
//!     Some(path_to_logfile),
//!     LevelFilter::Warn,
//!     #[cfg(feature = "syslog")] LevelFilter::Error
//! ).await.unwrap();
//!
//! // -- Anywhere in the code --
//! log::info!("Some useful info");
//! log::error!("An error");
//! // --------------------------
//!
//! // Remember to always close the logger at the end of the program
//! // to ensure that everything is written and closed correctly.
//! tiny_logs::end().await;
//! # tmp.close().unwrap();
//! # });
//! ```

#[cfg(feature = "syslog")]
mod syslog;

use std::{cmp::max, pin::Pin, sync::LazyLock};

use log::{Level, LevelFilter, Metadata, Record};
use owo_colors::{colors::css::DimGray, OwoColorize, Stream::Stdout};
use time::{format_description::FormatItem, macros::format_description, OffsetDateTime};
use tokio::{
    fs::File,
    io::{self, AsyncWrite, AsyncWriteExt},
    pin,
    sync::{
        mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
        Mutex,
    },
    task::{self, JoinHandle},
};

static LOGGER_HANDLER: LazyLock<Mutex<Option<LoggerHandler>>> = LazyLock::new(|| Mutex::new(None));

static DATE_FMT: &[FormatItem] =
    format_description!("[year]/[month]/[day]-[hour]:[minute]:[second].[subsecond digits:2]");

fn now() -> String {
    let now = OffsetDateTime::now_local().unwrap_or(OffsetDateTime::now_utc());
    now.format(DATE_FMT).unwrap_or(now.to_string())
}

fn create_log(record: &Record, args: &str, now: &str) -> String {
    let level = match record.level() {
        Level::Trace => "[TRACE]",
        Level::Debug => "[DEBUG]",
        Level::Info => "[INFO]",
        Level::Warn => "[WARN]",
        Level::Error => "[ERROR]",
    };

    format!(
        "{level} [{now}]{submod}- {args}\n",
        submod = {
            if let Some(s) = record.module_path() {
                format!(" {s} ")
            } else if let Some(s) = record.module_path_static() {
                format!(" {s} ")
            } else {
                " ".into()
            }
        }
    )
}

fn create_log_colored(record: &Record, args: &str, now: &str) -> String {
    let level = match record.level() {
        Level::Trace => format!(
            "{}",
            "[TRACE]".if_supports_color(Stdout, |t| t.truecolor(225, 237, 248))
        ),
        Level::Debug => format!(
            "{}",
            "[DEBUG]".if_supports_color(Stdout, |t| t.truecolor(109, 166, 218))
        ),
        Level::Info => format!(
            "{}",
            "[INFO]".if_supports_color(Stdout, |t| t.truecolor(128, 182, 92))
        ),
        Level::Warn => format!(
            "{}",
            "[WARN]".if_supports_color(Stdout, |t| t.truecolor(255, 199, 29))
        ),
        Level::Error => format!(
            "{}",
            "[ERROR]".if_supports_color(Stdout, |t| t.truecolor(200, 31, 31))
        ),
    };

    format!(
        "{level} [{now}]{submod}- {args}\n",
        now = now.if_supports_color(Stdout, |t| t.fg::<DimGray>()),
        submod = {
            if let Some(s) = record.module_path() {
                format!(" {} ", s.if_supports_color(Stdout, |t| t.bold()))
            } else if let Some(s) = record.module_path_static() {
                format!(" {} ", s.if_supports_color(Stdout, |t| t.bold()))
            } else {
                " ".into()
            }
        }
    )
}

enum LogMsg {
    Stdout(String),
    File(String),
    #[cfg(feature = "syslog")]
    Syslog(Level, String),
    Close,
}

/// Writes to file or stdout and flushes
async fn write_log<D: AsyncWrite>(dest: &mut Pin<&mut D>, log: String) {
    dest.write_all(log.as_bytes())
        .await
        .unwrap_or_else(|e| panic!("Failed to write log: {e}"));
    dest.flush()
        .await
        .unwrap_or_else(|e| panic!("Failed to flush log: {e}"));
}

/// Receives [`LogMsg`]s and writes them
async fn writer(mut recv: UnboundedReceiver<LogMsg>, file: Option<File>) {
    let stdout = io::stdout();
    pin!(stdout);
    if let Some(file) = file {
        pin!(file);
        while let Some(log) = recv.recv().await {
            match log {
                LogMsg::Stdout(log) => write_log(&mut stdout, log).await,
                LogMsg::File(log) => write_log(&mut file, log).await,
                #[cfg(feature = "syslog")]
                LogMsg::Syslog(level, log) => syslog::log(level, log).await,
                LogMsg::Close => recv.close(),
            }
        }
    } else {
        while let Some(log) = recv.recv().await {
            match log {
                LogMsg::Stdout(log) => write_log(&mut stdout, log).await,
                #[cfg(feature = "syslog")]
                LogMsg::Syslog(level, log) => syslog::log(level, log).await,
                LogMsg::Close => recv.close(),
                _ => (),
            }
        }
    }
}

/// Initializes Tiny Logger.
///
/// - `level`: Log level filter (See [`LevelFilter`]).
/// - `file`: Outputs logs to this path (Optional, if [`None`] outputs just to the standard output).
/// - `file_level`: Log level filter of the file (Optional, if [`None`] uses `level`, or `off` if file is none) (See [`LevelFilter`]).
/// - `syslog_level`: (feature `syslog`) Log level filter for the system logger (See [`LevelFilter`]).
///
/// At the end of your program, you must call [`end`] to end the logger correctly.
///
/// # Return
///
/// On error it returns an error message that can be displayed to the user.
pub async fn init(
    level: LevelFilter,
    file: Option<String>,
    mut file_level: LevelFilter,
    #[cfg(feature = "syslog")] syslog_level: LevelFilter,
) -> Result<(), String> {
    let file: Option<File> = match file {
        Some(path) if file_level != LevelFilter::Off => Some(
            File::options()
                .create(true)
                .append(true)
                .open(path)
                .await
                .map_err(|e| format!("Failed to open log file: {e}"))?,
        ),
        Some(_) => None,
        None => {
            file_level = LevelFilter::Off;
            None
        }
    };
    let (send, recv) = unbounded_channel::<LogMsg>();

    let logger = Box::new(TinyLogger {
        level,
        file_level,
        #[cfg(feature = "syslog")]
        syslog_level,
        sender: send.clone(),
    });

    #[cfg(feature = "syslog")]
    syslog::init(syslog_level).await;

    let joinhandle = task::spawn(async move { writer(recv, file).await });
    
    #[cfg(not(feature = "syslog"))]
    let max_level = max(level, file_level);
    
    #[cfg(feature = "syslog")]
    let max_level = max(level, max(file_level, syslog_level));

    log::set_boxed_logger(logger)
        .map(|_| log::set_max_level(max_level))
        .map_err(|e| format!("Failed to initialize logger: {e}"))?;

    let handler = &mut *LOGGER_HANDLER.lock().await;
    handler.replace(LoggerHandler {
        sender: send,
        joinhandle,
    });
    Ok(())
}

/// Must be called at the end of your program.
pub async fn end() {
    let handler = &mut *LOGGER_HANDLER.lock().await;
    if let Some(handler) = handler.take() {
        handler.end().await;
    }
}

struct LoggerHandler {
    sender: UnboundedSender<LogMsg>,
    joinhandle: JoinHandle<()>,
}

impl LoggerHandler {
    /// Ensures that all the last logs are logged.
    async fn end(self) {
        self.sender
            .send(LogMsg::Close)
            .expect("Logger was already closed.");

        self.joinhandle
            .await
            .expect("Failed to close the writer's task");

        #[cfg(feature = "syslog")]
        syslog::close().await;
    }
}

/// Tiny Logger instance.
///
/// You don't have to use this struct directly. [`init`] initializes the logger on its own.
/// After initializing, use the [`log`] crate and its macros for logging.
pub struct TinyLogger {
    level: LevelFilter,
    file_level: LevelFilter,
    #[cfg(feature = "syslog")]
    syslog_level: LevelFilter,
    sender: UnboundedSender<LogMsg>,
}

impl log::Log for TinyLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        let lvl = metadata.level();

        #[cfg(feature = "syslog")]
        {
            lvl <= self.level || lvl <= self.file_level || lvl <= self.syslog_level
        }

        #[cfg(not(feature = "syslog"))]
        {
            lvl <= self.level || lvl <= self.file_level
        }
    }

    fn log(&self, record: &Record) {
        let metadata = record.metadata();
        if self.sender.is_closed() || !self.enabled(metadata) {
            return;
        }

        let now = now();
        let args = format!("{}", record.args());
        let lvl = metadata.level();

        if lvl <= self.level {
            let _ = self
                .sender
                .send(LogMsg::Stdout(create_log_colored(record, &args, &now)));
        }

        if lvl <= self.file_level {
            let _ = self
                .sender
                .send(LogMsg::File(create_log(record, &args, &now)));
        }

        #[cfg(feature = "syslog")]
        if lvl <= self.syslog_level {
            let _ = self.sender.send(LogMsg::Syslog(lvl, args));
        }
    }

    fn flush(&self) {}
}

/// Each one of these tests must be run separately.
///
/// You can either run them one at a time or use some tool to do that (like [nextest](https://nexte.st/))
#[cfg(test)]
mod tests {
    use log::LevelFilter;
    use tempfile::NamedTempFile;
    use tokio::{fs::File, io::AsyncReadExt};

    use crate::{end, init};

    #[tokio::test(flavor = "multi_thread")]
    async fn logging1() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().as_os_str().to_str().unwrap();
        init(
            LevelFilter::Trace,
            Some(path.to_string()),
            LevelFilter::Trace,
            #[cfg(feature = "syslog")]
            LevelFilter::Trace,
        )
        .await
        .unwrap();
        log::trace!("logging1");
        log::debug!("logging1");
        log::info!("logging1");
        log::warn!("logging1");
        log::error!("logging1");
        end().await;
        log::info!("test!");

        // Test output
        let mut file = File::open(path).await.unwrap();
        let mut content = String::new();
        file.read_to_string(&mut content).await.unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 5);
        assert!(lines[0].starts_with("[TRACE]") && lines[0].ends_with("logging1"));
        assert!(lines[1].starts_with("[DEBUG]") && lines[0].ends_with("logging1"));
        assert!(lines[2].starts_with("[INFO]") && lines[0].ends_with("logging1"));
        assert!(lines[3].starts_with("[WARN]") && lines[0].ends_with("logging1"));
        assert!(lines[4].starts_with("[ERROR]") && lines[0].ends_with("logging1"));
        tmp.close().unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn logging2() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().as_os_str().to_str().unwrap();
        init(
            LevelFilter::Off,
            Some(path.to_string()),
            LevelFilter::Trace,
            #[cfg(feature = "syslog")]
            LevelFilter::Off,
        )
        .await
        .unwrap();
        log::trace!("logging2");
        log::debug!("logging2");
        log::info!("logging2");
        log::warn!("logging2");
        log::error!("logging2");
        end().await;
        log::info!("test");

        // Test output
        let mut file = File::open(path).await.unwrap();
        let mut content = String::new();
        file.read_to_string(&mut content).await.unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 5);
        assert!(lines[0].starts_with("[TRACE]") && lines[0].ends_with("logging2"));
        assert!(lines[1].starts_with("[DEBUG]") && lines[0].ends_with("logging2"));
        assert!(lines[2].starts_with("[INFO]") && lines[0].ends_with("logging2"));
        assert!(lines[3].starts_with("[WARN]") && lines[0].ends_with("logging2"));
        assert!(lines[4].starts_with("[ERROR]") && lines[0].ends_with("logging2"));
        tmp.close().unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn level1() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().as_os_str().to_str().unwrap();
        init(
            LevelFilter::Info,
            Some(path.to_string()),
            LevelFilter::Warn,
            #[cfg(feature = "syslog")]
            LevelFilter::Debug,
        )
        .await
        .unwrap();
        log::trace!("level1");
        log::debug!("level1");
        log::info!("level1");
        log::warn!("level1");
        log::error!("level1");
        end().await;

        // Test output
        let mut file = File::open(path).await.unwrap();
        let mut content = String::new();
        file.read_to_string(&mut content).await.unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("[WARN]") && lines[0].ends_with("level1"));
        assert!(lines[1].starts_with("[ERROR]") && lines[0].ends_with("level1"));
        tmp.close().unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn level2() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().as_os_str().to_str().unwrap();
        init(
            LevelFilter::Info,
            Some(path.to_string()),
            LevelFilter::Off,
            #[cfg(feature = "syslog")]
            LevelFilter::Warn,
        )
        .await
        .unwrap();
        log::trace!("level2");
        log::debug!("level2");
        log::info!("level2");
        log::warn!("level2");
        log::error!("level2");
        end().await;

        // Test output
        let mut file = File::open(path).await.unwrap();
        let mut content = String::new();
        file.read_to_string(&mut content).await.unwrap();
        assert_eq!(content.len(), 0);
        tmp.close().unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn level3() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().as_os_str().to_str().unwrap();
        init(
            LevelFilter::Info,
            None,
            LevelFilter::Info,
            #[cfg(feature = "syslog")]
            LevelFilter::Info,
        )
        .await
        .unwrap();
        log::trace!("level3");
        log::debug!("level3");
        log::info!("level3");
        log::warn!("level3");
        log::error!("level3");
        end().await;

        // Test output
        let mut file = File::open(path).await.unwrap();
        let mut content = String::new();
        file.read_to_string(&mut content).await.unwrap();
        assert_eq!(content.len(), 0);
        tmp.close().unwrap();
    }
}
