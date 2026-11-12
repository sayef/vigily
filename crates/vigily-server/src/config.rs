//! The server's configuration file, written by the operator (`vigily.toml`).
//!
//! ```toml
//! [[schedule]]
//! name = "nightly-intents"
//! description = "Score both intent classifiers on the support dataset"
//! cron = "0 2 * * *"                  # minute hour day-of-month month day-of-week
//! timezone = "Europe/Berlin"          # IANA name; fire times are stored in UTC
//! command = ["uv", "run", "python", "evals/nightly.py"]
//! workdir = "."                       # relative to this file
//! timeout = "30m"
//! retries = 1
//! retry_delay = "2m"
//! missed = "skip"                     # or "run_once": after downtime, run the latest missed fire
//! keep_runs = 200                     # runs kept, none older than keep_days (90), output for the
//! keep_output = 50                    # newest keep_output
//!
//! [alerts]
//! webhook = "https://hooks.example.com/vigily"
//!
//! [drift]
//! alpha = 0.01
//! min_effect = 0.02
//!
//! [tracing]
//! payloads = "truncated"              # "full", "truncated" (default) or "metadata"
//! max_field_bytes = 4096
//! currency = "EUR"
//! [tracing.prices."example-model"]    # per million tokens; there is no built-in price list
//! input = 2.0
//! output = 8.0
//! [tracing.projects.support-bot]      # a project (experiment) with its own payload policy
//! payloads = "metadata"
//! ```
//!
//! Commands come only from this file. The web app and the API can read schedules and their runs
//! but cannot create, change or start them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
pub use vigily_api::evals::MissedPolicy;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub schedule: Vec<ScheduleConfig>,
    #[serde(default)]
    pub alerts: AlertsConfig,
    #[serde(default)]
    pub drift: DriftSettings,
    #[serde(default)]
    pub tracing: vigily_core::traces::TracingConfig,
    /// The directory relative paths are resolved against: the file's directory.
    #[serde(skip)]
    pub base_dir: PathBuf,
    /// Where the configuration was read from, for display.
    #[serde(skip)]
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleConfig {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub cron: String,
    #[serde(default = "utc")]
    pub timezone: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub workdir: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_timeout")]
    pub timeout: String,
    #[serde(default)]
    pub retries: u32,
    #[serde(default = "default_retry_delay")]
    pub retry_delay: String,
    #[serde(default)]
    pub missed: MissedPolicy,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Runs kept for this schedule (newest first).
    #[serde(default = "keep_runs")]
    pub keep_runs: usize,
    /// Runs older than this many days are removed.
    #[serde(default = "keep_days")]
    pub keep_days: u32,
    /// Captured output is kept for this many of the newest runs.
    #[serde(default = "keep_output")]
    pub keep_output: usize,
}

fn keep_runs() -> usize {
    200
}
fn keep_days() -> u32 {
    90
}
fn keep_output() -> usize {
    50
}

fn utc() -> String {
    "UTC".into()
}
fn default_timeout() -> String {
    "1h".into()
}
fn default_retry_delay() -> String {
    "1m".into()
}
fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AlertsConfig {
    /// A URL that receives each new alert as a JSON `POST`.
    #[serde(default)]
    pub webhook: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DriftSettings {
    #[serde(default = "alpha")]
    pub alpha: f64,
    #[serde(default = "min_effect")]
    pub min_effect: f64,
}

fn alpha() -> f64 {
    0.01
}
fn min_effect() -> f64 {
    0.02
}

impl Default for DriftSettings {
    fn default() -> Self {
        Self {
            alpha: alpha(),
            min_effect: min_effect(),
        }
    }
}

/// Parses `90s`, `30m`, `2h`, `1d` or a number of seconds.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, unit) = match s.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
        Some((i, _)) => (&s[..i], &s[i..]),
        None => (s, "s"),
    };
    let n: f64 = num
        .trim()
        .parse()
        .map_err(|_| format!("{s:?} is not a duration such as 90s, 30m or 2h"))?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60.0,
        "h" => n * 3600.0,
        "d" => n * 86400.0,
        _ => return Err(format!("{s:?}: use s, m, h or d")),
    };
    if !(0.0..=30.0 * 86400.0).contains(&secs) {
        return Err(format!("{s:?} is out of range"));
    }
    Ok(Duration::from_secs_f64(secs))
}

impl ScheduleConfig {
    pub fn retention(&self) -> vigily_core::evals::Retention {
        vigily_core::evals::Retention {
            runs: self.keep_runs,
            days: self.keep_days,
            output: self.keep_output,
        }
    }

    pub fn timeout(&self) -> Duration {
        parse_duration(&self.timeout).unwrap_or(Duration::from_secs(3600))
    }

    pub fn retry_delay(&self) -> Duration {
        parse_duration(&self.retry_delay).unwrap_or(Duration::from_secs(60))
    }
}

impl Config {
    /// Reads and validates a configuration file.
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut c: Config =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        c.base_dir = path
            .parent()
            .map(Path::to_path_buf)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| PathBuf::from("."));
        c.path = Some(path.to_path_buf());
        c.validate()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(c)
    }

    pub fn validate(&self) -> Result<(), String> {
        let mut names = std::collections::HashSet::new();
        for s in &self.schedule {
            let ok = !s.name.is_empty()
                && s.name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
            if !ok {
                return Err(format!(
                    "schedule name {:?}: use letters, digits, '-', '_' or '.'",
                    s.name
                ));
            }
            if !names.insert(&s.name) {
                return Err(format!("schedule {:?} is defined twice", s.name));
            }
            let cron = croner::Cron::new(&s.cron)
                .parse()
                .map_err(|e| format!("schedule {:?}: cron {:?}: {e}", s.name, s.cron))?;
            // Expressions such as `0 2 30 2 *` (February 30) parse but never fire.
            let now = chrono::Utc::now();
            match cron.find_next_occurrence(&now, false) {
                Ok(next) if next - now <= chrono::Duration::days(5 * 366) => {}
                _ => {
                    return Err(format!(
                        "schedule {:?}: cron {:?} has no fire time in the next five years",
                        s.name, s.cron
                    ))
                }
            }
            s.timezone
                .parse::<chrono_tz::Tz>()
                .map_err(|_| format!("schedule {:?}: unknown time zone {:?} (use an IANA name such as Europe/Berlin)", s.name, s.timezone))?;
            if s.command.is_empty() || s.command[0].trim().is_empty() {
                return Err(format!("schedule {:?}: command is empty", s.name));
            }
            parse_duration(&s.timeout)
                .map_err(|e| format!("schedule {:?}: timeout {e}", s.name))?;
            parse_duration(&s.retry_delay)
                .map_err(|e| format!("schedule {:?}: retry_delay {e}", s.name))?;
            if s.keep_runs == 0 || s.keep_days == 0 {
                return Err(format!(
                    "schedule {:?}: keep_runs and keep_days must be at least 1",
                    s.name
                ));
            }
            if s.retries > 10 {
                return Err(format!("schedule {:?}: at most 10 retries", s.name));
            }
        }
        if let Some(w) = &self.alerts.webhook {
            if !(w.starts_with("https://") || w.starts_with("http://")) {
                return Err("alerts.webhook must be an http(s) URL".into());
            }
        }
        if !(0.0 < self.drift.alpha && self.drift.alpha < 1.0) {
            return Err("drift.alpha must be between 0 and 1".into());
        }
        self.tracing.validate()?;
        Ok(())
    }

    /// The working directory of a schedule's command.
    pub fn workdir(&self, s: &ScheduleConfig) -> PathBuf {
        match &s.workdir {
            Some(p) if p.is_absolute() => p.clone(),
            Some(p) => self.base_dir.join(p),
            None => self.base_dir.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_validates() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("vigily.toml");
        std::fs::write(
            &p,
            r#"
[[schedule]]
name = "nightly"
cron = "0 2 * * *"
timezone = "Europe/Berlin"
command = ["python", "evals/nightly.py"]
workdir = "repo"
timeout = "30m"
retries = 2
missed = "run_once"

[alerts]
webhook = "https://hooks.example.com/x"
"#,
        )
        .unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.schedule[0].missed, MissedPolicy::RunOnce);
        assert_eq!(c.schedule[0].timeout(), Duration::from_secs(1800));
        assert_eq!(c.workdir(&c.schedule[0]), dir.path().join("repo"));
        for bad in [
            "[[schedule]]\nname='a'\ncron='nope'\ncommand=['x']",
            "[[schedule]]\nname='a'\ncron='* * * * *'\ntimezone='Mars/Base'\ncommand=['x']",
            "[[schedule]]\nname='a b'\ncron='* * * * *'\ncommand=['x']",
            "[[schedule]]\nname='a'\ncron='* * * * *'\ncommand=[]",
            "[[schedule]]\nname='a'\ncron='* * * * *'\ncommand=['x']\nunknown=1",
            "[alerts]\nwebhook='ftp://x'",
            "[[schedule]]\nname='a'\ncron='0 2 30 2 *'\ncommand=['x']",
            "[[schedule]]\nname='a'\ncron='0 2 31 4 *'\ncommand=['x']",
        ] {
            std::fs::write(&p, bad).unwrap();
            assert!(Config::load(&p).is_err(), "{bad}");
        }
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
        assert!(parse_duration("5 weeks").is_err());
    }
}
