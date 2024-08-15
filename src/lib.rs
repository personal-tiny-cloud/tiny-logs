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
//! This is the default logger of [Tiny Cloud](https://github.com/personal-tiny-cloud/tiny-cloud).
//! It uses [`tokio`] as a backend to log files asynchronously.
//!
//! It implements [`log::Log`] of the [`log`] crate.
//!
//! # Usage
//!
//! ```rust
//! # tokio_test::block_on(async {
//! # let tmp = tempfile::NamedTempFile::new().unwrap();
//! # let path_to_logfile = tmp.path().as_os_str().to_str().unwrap().to_string();
//! use log::LevelFilter;
//!
//! // Level filter for the terminal's output - path to a log file - level filter of the log file
//! // Remember to show the error to the user if there's any.
//! tiny_logs::init(LevelFilter::Info, Some(path_to_logfile), LevelFilter::Warn).await.unwrap();
//!
//! // -- Anywhere in the code --
//!
//! log::info!("Some useful info");
//! log::error!("An error");
//!
//! // --------------------------
//!
//! // Remember to always close the logger at the end of the program
//! // to ensure that everything is written and closed correctly.
//! tiny_logs::end().await;
//! # tmp.close().unwrap();
//! # });
//! ```

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

fn create_log(record: &Record) -> String {
    let level = match record.level() {
        Level::Trace => "[TRACE]",
        Level::Debug => "[DEBUG]",
        Level::Info => "[INFO]",
        Level::Warn => "[WARN]",
        Level::Error => "[ERROR]",
    };

    format!(
        "{level} [{now}]{submod}- {args}\n",
        now = now(),
        submod = {
            if let Some(s) = record.module_path() {
                format!(" {s} ")
            } else if let Some(s) = record.module_path_static() {
                format!(" {s} ")
            } else {
                " ".into()
            }
        },
        args = record.args()
    )
}

fn create_log_colored(record: &Record) -> String {
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
        now = now().if_supports_color(Stdout, |t| t.fg::<DimGray>()),
        submod = {
            if let Some(s) = record.module_path() {
                format!(" {} ", s.if_supports_color(Stdout, |t| t.bold()))
            } else if let Some(s) = record.module_path_static() {
                format!(" {} ", s.if_supports_color(Stdout, |t| t.bold()))
            } else {
                " ".into()
            }
        },
        args = record.args()
    )
}

enum LogMsg {
    Stdout(String),
    File(String),
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
                LogMsg::Close => recv.close(),
            }
        }
    } else {
        while let Some(log) = recv.recv().await {
            match log {
                LogMsg::Stdout(log) => write_log(&mut stdout, log).await,
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
        sender: send.clone(),
    });
    log::set_boxed_logger(logger)
        .map(|_| log::set_max_level(max(level, file_level)))
        .map_err(|e| format!("Failed to initialize logger: {e}"))?;

    let joinhandle = task::spawn(async move { writer(recv, file).await });

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
    }
}

/// Tiny Logger instance.
///
/// You don't have to use this struct directly. [`init`] initializes the logger on its own.
/// After initializing, use the [`log`] crate and its macros for logging.
pub struct TinyLogger {
    level: LevelFilter,
    file_level: LevelFilter,
    sender: UnboundedSender<LogMsg>,
}

impl log::Log for TinyLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        let lvl = metadata.level();
        lvl <= self.level || lvl <= self.file_level
    }

    fn log(&self, record: &Record) {
        let metadata = record.metadata();
        if self.sender.is_closed() || !self.enabled(metadata) {
            return;
        }

        let lvl = metadata.level();
        if lvl <= self.level {
            let _ = self.sender.send(LogMsg::Stdout(create_log_colored(record)));
        }
        if lvl <= self.file_level {
            let _ = self.sender.send(LogMsg::File(create_log(record)));
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
        )
        .await
        .unwrap();
        log::trace!("hello");
        log::debug!("hello");
        log::info!("hello");
        log::warn!("hello");
        log::error!("hello");
        end().await;
        log::info!("test");

        // Test output
        let mut file = File::open(path).await.unwrap();
        let mut content = String::new();
        file.read_to_string(&mut content).await.unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 5);
        assert!(lines[0].starts_with("[TRACE]") && lines[0].ends_with("hello"));
        assert!(lines[1].starts_with("[DEBUG]") && lines[0].ends_with("hello"));
        assert!(lines[2].starts_with("[INFO]") && lines[0].ends_with("hello"));
        assert!(lines[3].starts_with("[WARN]") && lines[0].ends_with("hello"));
        assert!(lines[4].starts_with("[ERROR]") && lines[0].ends_with("hello"));
        tmp.close().unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn logging2() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().as_os_str().to_str().unwrap();
        init(LevelFilter::Off, Some(path.to_string()), LevelFilter::Trace)
            .await
            .unwrap();
        log::trace!("hello");
        log::debug!("hello");
        log::info!("hello");
        log::warn!("hello");
        log::error!("hello");
        end().await;
        log::info!("test");

        // Test output
        let mut file = File::open(path).await.unwrap();
        let mut content = String::new();
        file.read_to_string(&mut content).await.unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 5);
        assert!(lines[0].starts_with("[TRACE]") && lines[0].ends_with("hello"));
        assert!(lines[1].starts_with("[DEBUG]") && lines[0].ends_with("hello"));
        assert!(lines[2].starts_with("[INFO]") && lines[0].ends_with("hello"));
        assert!(lines[3].starts_with("[WARN]") && lines[0].ends_with("hello"));
        assert!(lines[4].starts_with("[ERROR]") && lines[0].ends_with("hello"));
        tmp.close().unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn level1() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().as_os_str().to_str().unwrap();
        init(LevelFilter::Info, Some(path.to_string()), LevelFilter::Warn)
            .await
            .unwrap();
        log::trace!("Hi!");
        log::debug!("Hi!");
        log::info!("Hi!");
        log::warn!("Hi!");
        log::error!("Hi!");
        end().await;

        // Test output
        let mut file = File::open(path).await.unwrap();
        let mut content = String::new();
        file.read_to_string(&mut content).await.unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("[WARN]") && lines[0].ends_with("Hi!"));
        assert!(lines[1].starts_with("[ERROR]") && lines[0].ends_with("Hi!"));
        tmp.close().unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn level2() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().as_os_str().to_str().unwrap();
        init(LevelFilter::Info, Some(path.to_string()), LevelFilter::Off)
            .await
            .unwrap();
        log::trace!("Hi!");
        log::debug!("Hi!");
        log::info!("Hi!");
        log::warn!("Hi!");
        log::error!("Hi!");
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
        init(LevelFilter::Info, None, LevelFilter::Info)
            .await
            .unwrap();
        log::trace!("Hi!");
        log::debug!("Hi!");
        log::info!("Hi!");
        log::warn!("Hi!");
        log::error!("Hi!");
        end().await;

        // Test output
        let mut file = File::open(path).await.unwrap();
        let mut content = String::new();
        file.read_to_string(&mut content).await.unwrap();
        assert_eq!(content.len(), 0);
        tmp.close().unwrap();
    }
}
