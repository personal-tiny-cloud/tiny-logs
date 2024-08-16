# Simple logger for Tiny Cloud.

This is [Tiny Cloud](https://github.com/personal-tiny-cloud/tiny-cloud)'s logger.

tiny-logs uses [Tokio](https://tokio.rs/) as a backend to log files asynchronously to avoid blocking operations
(like writing to files or to stdout) during the execution of the program.

It is also able to log by using libc's [syslog](https://www.man7.org/linux/man-pages/man3/openlog.3.html)
(feature `syslog`). Since it is part of the Standard C Library, it should cover most platforms.
On Linux, logs made with `syslog()` will be saved to either syslogd or systemd-journald (depending on the system).

This feature is useful for low-spec hardware where logging to files might slow down the
system. For example, Raspberry Pis usually prefer using RAM logs for their system logs,
due to their slow writing speed. It might not be useful on systems with systemd, because the
standard output of every daemon is logged by default.

tiny-logs uses the [`log` crate](https://docs.rs/log/latest/log/) as a logging API.
Logging has to be done by using its macros.

# Usage

```rust
use log::LevelFilter;

// 1. Level filter for the terminal's output.
// 2. Path to a log file.
// 3. Level filter of the log file.
// 4. (feature 'syslog') Level filter of the system logger.
// Remember to show the error to the user if there's any.
tiny_logs::init(
    LevelFilter::Info,
    Some(path_to_logfile),
    LevelFilter::Warn,
    #[cfg(feature = "syslog")] LevelFilter::Error
).await.unwrap();

// -- Anywhere in the code --
log::info!("Some useful info");
log::error!("An error");
// --------------------------

// Remember to always close the logger at the end of the program
// to ensure that everything is written and closed correctly.
tiny_logs::end().await;
```

# Issues

If you find issues or bugs don't hesitate to open an issue, it would be really helpful.
Remember to always include logs and maybe an example to test it.

# License

This project is licensed under the [GNU General Public License 3.0](/LICENSE)

