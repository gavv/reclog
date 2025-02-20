use chrono::{DateTime, Local, TimeDelta};
use clap::ValueEnum;
use rustix::system;
use std::fmt;
use std::time::Instant;

/// How to calculate timestamps.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq)]
#[clap(rename_all = "kebab_case")]
pub enum TimeSource {
    Wall,
    Elapsed,
    Delta,
}

/// Formats extras: header and timestamps.
pub struct Formatter {
    enable_header: bool,
    enable_time: bool,
    time_format: String,
    time_source: TimeSource,
    command: String,
    base_ts: Option<Instant>,
}

impl Formatter {
    pub fn new(
        enable_header: bool,
        enable_time: bool,
        time_format: &str,
        time_source: TimeSource,
        command: &[String],
    ) -> Self {
        Formatter {
            enable_header,
            enable_time,
            time_format: time_format.into(),
            time_source,
            command: command.join(" "),
            base_ts: None,
        }
    }

    /// True if header should be formatted.
    pub fn need_header(&self) -> bool {
        self.enable_header
    }

    /// Format header to string.
    pub fn format_header(&mut self, result: &mut String) -> fmt::Result {
        let date = Local::now().format("%F %T %z");
        let info = system::uname();

        result.push_str(&format!(
            "# HOST=[{}] OS=[{}_{}] TIME=[{}] CMD=[{}]\n",
            info.nodename().to_str().unwrap(),
            info.sysname().to_str().unwrap().to_lowercase(),
            info.machine().to_str().unwrap(),
            date,
            self.command
        ));

        self.enable_header = false;

        Ok(())
    }

    /// True if timestamp should be formatted.
    pub fn need_timestamp(&self) -> bool {
        self.enable_time
    }

    /// Format timestamp to string.
    pub fn format_timestamp(&mut self, result: &mut String) -> fmt::Result {
        match self.time_source {
            TimeSource::Wall => {
                let now = Local::now();
                now.format(&self.time_format).write_to(result)?;
            }
            TimeSource::Elapsed | TimeSource::Delta => {
                let now = Instant::now();
                if self.base_ts.is_none() {
                    self.base_ts = Some(now);
                }

                let delta = DateTime::UNIX_EPOCH
                    + TimeDelta::from_std(now - self.base_ts.unwrap()).unwrap();
                delta.format(&self.time_format).write_to(result)?;

                if self.time_source == TimeSource::Delta {
                    self.base_ts = Some(now);
                }
            }
        };

        Ok(())
    }
}
