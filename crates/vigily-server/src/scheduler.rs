//! Scheduled evaluations: the server runs the commands configured in `vigily.toml` on their cron
//! schedules.
//!
//! - Fire times are computed in the schedule's time zone (so `0 2 * * *` in `Europe/Berlin` stays at
//!   02:00 local across daylight-saving changes) and stored in UTC.
//! - A schedule never overlaps itself: a fire while the previous run is still going is recorded as
//!   skipped.
//! - After downtime, missed fires are either recorded as skipped (`missed = "skip"`) or the latest
//!   one is run once (`missed = "run_once"`).
//! - Each attempt gets an id, its exit status, and the last 64 KiB of stdout and stderr. The
//!   command runs in its own session; when it exits or times out, every process it left behind is
//!   stopped (its process group, and on Linux any process that still carries the attempt's
//!   `VIGILY_SCHEDULE_RUN`, which also catches processes that called `setsid`). Commands must not
//!   leave background processes running. Failed attempts are retried; when the last one fails, an
//!   alert is raised.
//! - Old runs and their output are pruned (`keep_runs`, `keep_days`, `keep_output`).
//! - Runs that were going when the server stopped are marked interrupted at the next start.
//! - The command gets `VIGILY_REPO`, `VIGILY_SCHEDULE`, `VIGILY_SCHEDULE_RUN` and
//!   `VIGILY_SCHEDULED_FOR`, so runs it logs through the SDK link back to the schedule run.
//!
//! Security: commands run as the server's user, with its permissions and environment plus the
//! schedule's `env`. They come only from the configuration file; the API cannot add, change or
//! start them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use chrono_tz::Tz;
use croner::Cron;
use tokio::io::AsyncReadExt;
use vigily_core::evals::{self, Alert, AlertKind, ScheduleRun, ScheduleRunStatus, Severity};
use vigily_core::now_micros;

use crate::config::{MissedPolicy, ScheduleConfig};
use crate::AppState;

/// Bytes of each output stream kept per run.
pub const LOG_TAIL: usize = 64 * 1024;

/// The first fire strictly after `after_us`, in UTC microseconds.
pub fn next_fire(cron: &Cron, tz: Tz, after_us: i64) -> Option<i64> {
    let t = Utc.timestamp_micros(after_us).single()?.with_timezone(&tz);
    cron.find_next_occurrence(&t, false)
        .ok()
        .map(|d| d.with_timezone(&Utc).timestamp_micros())
}

/// Fires in `(last, now]`, at most `limit`.
pub fn missed_fires(cron: &Cron, tz: Tz, last: i64, now: i64, limit: usize) -> Vec<i64> {
    let mut out = Vec::new();
    let mut t = last;
    while let Some(n) = next_fire(cron, tz, t) {
        if n > now || out.len() >= limit {
            break;
        }
        out.push(n);
        t = n;
    }
    out
}

/// The fires missed in `(last, now]`: how many (counting stops at `cap`) and the latest one.
/// The latest is found by searching back from `now` in growing windows, so it is right however long
/// the server was down.
pub fn missed_summary(
    cron: &Cron,
    tz: Tz,
    last: i64,
    now: i64,
    cap: usize,
) -> (usize, bool, Option<i64>) {
    let count = missed_fires(cron, tz, last, now, cap).len();
    let mut latest = None;
    for hours in [1i64, 24, 24 * 8, 24 * 32, 24 * 367, 24 * 367 * 10] {
        let from = (now - hours * 3_600_000_000).max(last);
        let mut t = from;
        while let Some(n) = next_fire(cron, tz, t) {
            if n > now {
                break;
            }
            latest = Some(n);
            t = n;
        }
        if latest.is_some() || from == last {
            break;
        }
    }
    (count, count >= cap, latest)
}

fn parsed(cfg: &ScheduleConfig) -> (Cron, Tz) {
    let cron = Cron::new(&cfg.cron)
        .parse()
        .expect("validated in Config::load");
    let tz: Tz = cfg.timezone.parse().expect("validated in Config::load");
    (cron, tz)
}

/// Starts one task per enabled schedule, after marking runs left over from an earlier server
/// process as interrupted.
pub fn start(state: Arc<AppState>) {
    for cfg in state.config.schedule.iter().filter(|s| s.enabled).cloned() {
        let st = state.clone();
        tokio::spawn(async move {
            mark_interrupted(&st, &cfg.name).await;
            run_schedule(st, cfg).await
        });
    }
}

/// Runs recorded as running by an earlier process cannot still be going: mark them interrupted.
async fn mark_interrupted(state: &AppState, schedule: &str) {
    let Ok(runs) = evals::schedule_runs(&*state.records, schedule).await else {
        return;
    };
    for mut r in runs
        .into_iter()
        .filter(|r| r.status == ScheduleRunStatus::Running)
    {
        r.status = ScheduleRunStatus::Interrupted;
        r.ended_at = Some(now_micros());
        r.reason =
            Some("the server stopped while this run was going; its outcome is unknown".into());
        let _ = evals::put_schedule_run(&*state.records, &r).await;
    }
}

async fn save_last_fire(state: &AppState, name: &str, at: i64) {
    let s = evals::ScheduleState {
        last_fire: Some(at),
    };
    if let Err(e) = evals::put_schedule_state(&*state.records, name, &s).await {
        tracing::warn!(schedule = name, error = %e, "could not save schedule state");
    }
}

async fn run_schedule(state: Arc<AppState>, cfg: ScheduleConfig) {
    let (cron, tz) = parsed(&cfg);
    let running = Arc::new(AtomicBool::new(false));
    let now = now_micros();
    let last = evals::schedule_state(&*state.records, &cfg.name)
        .await
        .ok()
        .and_then(|s| s.last_fire);
    let mut last = match last {
        None => {
            save_last_fire(&state, &cfg.name, now).await;
            now
        }
        Some(last) => {
            let (count, capped, latest) = missed_summary(&cron, tz, last, now, 100_000);
            match latest {
                None => last,
                Some(latest) => {
                    match cfg.missed {
                        MissedPolicy::Skip => {
                            let r = ScheduleRun {
                                id: format!("{:020}-0", now_micros()),
                                schedule: cfg.name.clone(),
                                scheduled_for: latest,
                                started_at: now_micros(),
                                ended_at: Some(now_micros()),
                                attempt: 0,
                                status: ScheduleRunStatus::SkippedMissed,
                                exit_code: None,
                                command: cfg.command.clone(),
                                stdout: String::new(),
                                stderr: String::new(),
                                reason: Some(format!(
                                    "{}{count} scheduled {} missed while the server was not running; the policy is to skip them",
                                    if capped { "more than " } else { "" },
                                    if count == 1 && !capped { "run was" } else { "runs were" }
                                )),
                            };
                            let _ = evals::put_schedule_run(&*state.records, &r).await;
                        }
                        MissedPolicy::RunOnce => {
                            spawn_execute(state.clone(), cfg.clone(), latest, running.clone());
                        }
                    }
                    save_last_fire(&state, &cfg.name, latest).await;
                    latest
                }
            }
        }
    };
    loop {
        let Some(next) = next_fire(&cron, tz, last.max(now_micros())) else {
            tracing::warn!(schedule = %cfg.name, "the cron expression has no future fire time");
            return;
        };
        state.schedule_status.write().insert(cfg.name.clone(), next);
        let wait = (next - now_micros()).max(0) as u64;
        tokio::time::sleep(std::time::Duration::from_micros(wait)).await;
        last = next;
        save_last_fire(&state, &cfg.name, next).await;
        if running.load(Ordering::Acquire) {
            let r = ScheduleRun {
                id: format!("{:020}-0", now_micros()),
                schedule: cfg.name.clone(),
                scheduled_for: next,
                started_at: now_micros(),
                ended_at: Some(now_micros()),
                attempt: 0,
                status: ScheduleRunStatus::SkippedOverlap,
                exit_code: None,
                command: cfg.command.clone(),
                stdout: String::new(),
                stderr: String::new(),
                reason: Some("the previous run of this schedule was still going".into()),
            };
            let _ = evals::put_schedule_run(&*state.records, &r).await;
        } else {
            spawn_execute(state.clone(), cfg.clone(), next, running.clone());
        }
    }
}

fn spawn_execute(state: Arc<AppState>, cfg: ScheduleConfig, at: i64, running: Arc<AtomicBool>) {
    running.store(true, Ordering::Release);
    tokio::spawn(async move {
        execute(&state, &cfg, at).await;
        running.store(false, Ordering::Release);
    });
}

/// Runs a schedule's command for the fire time `at`, with retries; raises an alert when every
/// attempt fails. Returns the attempts.
pub async fn execute(state: &AppState, cfg: &ScheduleConfig, at: i64) -> Vec<ScheduleRun> {
    execute_from(state, cfg, at, None).await
}

/// The record of a first attempt that starts now, before its command is spawned.
fn running_record(cfg: &ScheduleConfig, at: i64, attempt: u32) -> ScheduleRun {
    let started = now_micros();
    ScheduleRun {
        id: attempt_id(&cfg.name, started, attempt),
        schedule: cfg.name.clone(),
        scheduled_for: at,
        started_at: started,
        ended_at: None,
        attempt,
        status: ScheduleRunStatus::Running,
        exit_code: None,
        command: cfg.command.clone(),
        stdout: String::new(),
        stderr: String::new(),
        reason: None,
    }
}

/// Starts a schedule's command now, as the operator, in the background (with its retries, like a
/// scheduled run). Returns the first attempt, already recorded as running, so a client can follow
/// it by id.
pub async fn start_now(state: Arc<AppState>, cfg: ScheduleConfig) -> ScheduleRun {
    let at = now_micros();
    let first = running_record(&cfg, at, 1);
    let _ = evals::put_schedule_run(&*state.records, &first).await;
    let r = first.clone();
    tokio::spawn(async move {
        execute_from(&state, &cfg, at, Some(r)).await;
    });
    first
}

/// [`execute`], with the first attempt's record made (and stored) already.
async fn execute_from(
    state: &AppState,
    cfg: &ScheduleConfig,
    at: i64,
    mut first: Option<ScheduleRun>,
) -> Vec<ScheduleRun> {
    let mut attempts = Vec::new();
    for attempt in 1..=cfg.retries + 1 {
        let r = run_once(state, cfg, at, attempt, first.take()).await;
        let ok = r.status == ScheduleRunStatus::Succeeded;
        attempts.push(r);
        if ok {
            prune(state, cfg).await;
            return attempts;
        }
        if attempt <= cfg.retries {
            tokio::time::sleep(cfg.retry_delay()).await;
        }
    }
    prune(state, cfg).await;
    let last = attempts.last().expect("at least one attempt");
    let when = Utc
        .timestamp_micros(at)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_default();
    let what = match last.status {
        ScheduleRunStatus::TimedOut => format!("timed out after {}", cfg.timeout),
        _ => match last.exit_code {
            Some(c) => format!("exited with status {c}"),
            None => last.reason.clone().unwrap_or_else(|| "failed".into()),
        },
    };
    crate::monitor::notify(
        state,
        Alert {
            id: String::new(),
            kind: AlertKind::ScheduleFailed,
            severity: Severity::Critical,
            title: format!("Schedule {} failed", cfg.name),
            detail: format!(
                "The run for {when} {what} after {} {}. Its output is on the schedule's page.",
                attempts.len(),
                if attempts.len() == 1 {
                    "attempt"
                } else {
                    "attempts"
                }
            ),
            suite: None,
            run_id: None,
            schedule: Some(cfg.name.clone()),
            created_at: 0,
            acknowledged_at: None,
            dedupe: format!("schedule/{}/{at}", cfg.name),
            delivered: None,
        },
    )
    .await;
    attempts
}

async fn prune(state: &AppState, cfg: &ScheduleConfig) {
    if let Err(e) =
        evals::prune_schedule_runs(&*state.records, &cfg.name, cfg.retention(), now_micros()).await
    {
        tracing::warn!(schedule = %cfg.name, error = %e, "could not prune old schedule runs");
    }
}

/// How long output readers may keep reading after the command exited or was stopped.
const READ_GRACE: std::time::Duration = std::time::Duration::from_secs(2);
/// How long processes get between SIGTERM and SIGKILL.
const KILL_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

type Tail = Arc<parking_lot::Mutex<Vec<u8>>>;

/// Keeps the last `LOG_TAIL` bytes of a stream in `buf`, which stays readable if the reader is
/// stopped before the stream ends.
async fn tail(mut r: impl tokio::io::AsyncRead + Unpin, buf: Tail) {
    let mut chunk = [0u8; 8192];
    loop {
        match r.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut b = buf.lock();
                b.extend_from_slice(&chunk[..n]);
                if b.len() > 2 * LOG_TAIL {
                    let cut = b.len() - LOG_TAIL;
                    b.drain(..cut);
                }
            }
        }
    }
}

fn tail_text(buf: &Tail) -> String {
    let b = buf.lock();
    let start = b.len().saturating_sub(LOG_TAIL);
    String::from_utf8_lossy(&b[start..]).into_owned()
}

/// Processes other than this one started by the given schedule attempt: their environment carries
/// both `VIGILY_SCHEDULE=<schedule>` and `VIGILY_SCHEDULE_RUN=<run_id>`.
#[cfg(target_os = "linux")]
fn marked_processes(schedule: &str, run_id: &str) -> Vec<i32> {
    let want_schedule = format!("VIGILY_SCHEDULE={schedule}");
    let want_run = format!("VIGILY_SCHEDULE_RUN={run_id}");
    let me = std::process::id() as i32;
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return vec![];
    };
    dir.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter(|pid| *pid != me)
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .map(|env| {
                    let vars: Vec<&[u8]> = env.split(|b| *b == 0).collect();
                    vars.contains(&want_schedule.as_bytes()) && vars.contains(&want_run.as_bytes())
                })
                .unwrap_or(false)
        })
        .collect()
}

#[cfg(all(unix, not(target_os = "linux")))]
fn marked_processes(_schedule: &str, _run_id: &str) -> Vec<i32> {
    vec![]
}

/// Sends `sig` to the command's process group and to every marked process; true if any was alive.
#[cfg(unix)]
fn signal_all(pgid: i32, schedule: &str, run_id: &str, sig: i32) -> bool {
    // SAFETY: kill and killpg only send signals; invalid ids return an error.
    let mut alive = unsafe { libc::killpg(pgid, sig) } == 0;
    for pid in marked_processes(schedule, run_id) {
        alive |= unsafe { libc::kill(pid, sig) } == 0;
    }
    alive
}

/// Stops everything the command started: SIGTERM, then SIGKILL after a grace period. The caller
/// keeps the group leader unreaped until this returns, so its id cannot be reused meanwhile.
#[cfg(unix)]
async fn stop_all(pgid: i32, schedule: String, run_id: String) {
    let (s, r) = (schedule.clone(), run_id.clone());
    let alive = tokio::task::spawn_blocking(move || signal_all(pgid, &s, &r, libc::SIGTERM))
        .await
        .unwrap_or(false);
    if !alive {
        return;
    }
    tokio::time::sleep(KILL_GRACE).await;
    let _ =
        tokio::task::spawn_blocking(move || signal_all(pgid, &schedule, &run_id, libc::SIGKILL))
            .await;
}

/// Whether the child `pid` has exited, without reaping it (`waitid` with `WNOWAIT`).
#[cfg(unix)]
fn has_exited(pid: i32) -> bool {
    // SAFETY: a zeroed siginfo_t is a valid out-parameter; waitid only writes to it.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let r = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
        )
    };
    // An error means there is no such child left to wait for.
    r != 0 || si_pid(&info) != 0
}

#[cfg(target_os = "linux")]
fn si_pid(i: &libc::siginfo_t) -> i32 {
    // SAFETY: waitid filled in the child fields when it reported a child.
    unsafe { i.si_pid() }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn si_pid(i: &libc::siginfo_t) -> i32 {
    i.si_pid
}

/// Waits until `pid` exits (true) or `timeout` passes (false), leaving it unreaped.
#[cfg(unix)]
async fn wait_unreaped(pid: i32, timeout: std::time::Duration) -> bool {
    let start = std::time::Instant::now();
    loop {
        if has_exited(pid) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// A unique attempt id that still sorts by start time: `{start µs:020}-{attempt}-{random hex}`.
fn attempt_id(schedule: &str, started: i64, attempt: u32) -> String {
    use std::hash::BuildHasher;
    // RandomState is keyed randomly per instance, so this differs between calls and processes.
    let h = std::collections::hash_map::RandomState::new().hash_one((
        schedule,
        started,
        attempt,
        std::process::id(),
    ));
    format!("{started:020}-{attempt}-{:08x}", h as u32)
}

async fn run_once(
    state: &AppState,
    cfg: &ScheduleConfig,
    at: i64,
    attempt: u32,
    recorded: Option<ScheduleRun>,
) -> ScheduleRun {
    let mut rec = match recorded {
        Some(r) => r,
        None => {
            let r = running_record(cfg, at, attempt);
            let _ = evals::put_schedule_run(&*state.records, &r).await;
            r
        }
    };
    let scheduled_for = Utc
        .timestamp_micros(at)
        .single()
        .map(|d| d.to_rfc3339())
        .unwrap_or_default();
    let mut cmd = tokio::process::Command::new(&cfg.command[0]);
    cmd.args(&cfg.command[1..])
        .current_dir(state.config.workdir(cfg))
        .envs(&cfg.env)
        .env("VIGILY_REPO", state.catalog.store().location())
        .env("VIGILY_SCHEDULE", &cfg.name)
        .env("VIGILY_SCHEDULE_RUN", &rec.id)
        .env("VIGILY_SCHEDULED_FOR", scheduled_for)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // Its own session (and so its own process group), so it can be stopped as a whole.
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            rec.status = ScheduleRunStatus::Failed;
            rec.reason = Some(format!("could not start {:?}: {e}", cfg.command[0]));
            rec.ended_at = Some(now_micros());
            let _ = evals::put_schedule_run(&*state.records, &rec).await;
            return rec;
        }
    };
    #[cfg(unix)]
    let pgid = child.id().map(|p| p as i32);
    let (out_buf, err_buf): (Tail, Tail) = Default::default();
    let mut out = tokio::spawn(tail(child.stdout.take().expect("piped"), out_buf.clone()));
    let mut err = tokio::spawn(tail(child.stderr.take().expect("piped"), err_buf.clone()));
    // The timeout covers the command itself; it does not wait for its output pipes to close.
    // The leader stays unreaped until everything it left behind is stopped, so neither its id nor
    // its process group id can be reused by an unrelated process in between.
    #[cfg(unix)]
    let exited = match pgid {
        Some(pid) => wait_unreaped(pid, cfg.timeout()).await,
        None => true,
    };
    #[cfg(not(unix))]
    let exited = tokio::time::timeout(cfg.timeout(), child.wait())
        .await
        .is_ok();
    if !exited {
        rec.status = ScheduleRunStatus::TimedOut;
        rec.reason = Some(format!("stopped after the {} timeout", cfg.timeout));
    }
    #[cfg(unix)]
    if let Some(pgid) = pgid {
        stop_all(pgid, cfg.name.clone(), rec.id.clone()).await;
    }
    #[cfg(not(unix))]
    if !exited {
        let _ = child.start_kill();
    }
    let status = child.wait().await;
    if exited {
        match status {
            Ok(status) => {
                rec.exit_code = status.code();
                rec.status = if status.success() {
                    ScheduleRunStatus::Succeeded
                } else {
                    ScheduleRunStatus::Failed
                };
                if status.code().is_none() {
                    rec.reason = Some("the command was killed by a signal".into());
                }
            }
            Err(e) => {
                rec.status = ScheduleRunStatus::Failed;
                rec.reason = Some(format!("could not wait for the command: {e}"));
            }
        }
    }
    // Give the readers a moment to drain what is left in the pipes.
    for h in [&mut out, &mut err] {
        if tokio::time::timeout(READ_GRACE, &mut *h).await.is_err() {
            h.abort();
        }
    }
    rec.stdout = tail_text(&out_buf);
    rec.stderr = tail_text(&err_buf);
    rec.ended_at = Some(now_micros());
    let _ = evals::put_schedule_run(&*state.records, &rec).await;
    rec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_ids_are_unique_and_sort_by_start() {
        let a = attempt_id("nightly", 42, 1);
        let b = attempt_id("weekly", 42, 1);
        assert_ne!(a, b);
        assert!(a.starts_with("00000000000000000042-1-"));
        assert_ne!(
            attempt_id("nightly", 42, 1),
            attempt_id("nightly", 42, 1),
            "random per call"
        );
    }

    #[test]
    fn fires_in_the_schedule_time_zone() {
        let cron = Cron::new("0 2 * * *").parse().unwrap();
        let tz: Tz = "Europe/Berlin".parse().unwrap();
        // 02:00 in Berlin is 00:00 UTC in summer (UTC+2) and 01:00 UTC in winter (UTC+1).
        let summer = Utc
            .with_ymd_and_hms(2026, 7, 10, 12, 0, 0)
            .unwrap()
            .timestamp_micros();
        let n = next_fire(&cron, tz, summer).unwrap();
        assert_eq!(
            Utc.timestamp_micros(n).unwrap().to_rfc3339(),
            "2026-07-11T00:00:00+00:00"
        );
        let winter = Utc
            .with_ymd_and_hms(2026, 1, 10, 12, 0, 0)
            .unwrap()
            .timestamp_micros();
        let n = next_fire(&cron, tz, winter).unwrap();
        assert_eq!(
            Utc.timestamp_micros(n).unwrap().to_rfc3339(),
            "2026-01-11T01:00:00+00:00"
        );
        let t = Utc
            .with_ymd_and_hms(2026, 3, 28, 12, 0, 0)
            .unwrap()
            .timestamp_micros();
        let last = Utc
            .with_ymd_and_hms(2026, 3, 20, 1, 0, 0)
            .unwrap()
            .timestamp_micros();
        let m = missed_fires(&cron, tz, last, t, 100);
        assert_eq!(m.len(), 8, "the 21st to the 28th");
        assert_eq!(
            missed_summary(&cron, tz, last, t, 100),
            (8, false, m.last().copied())
        );
        // A per-minute schedule down for 100 days: capped count, but the right latest fire.
        let every_minute = Cron::new("* * * * *").parse().unwrap();
        let down = t - 100 * 86_400_000_000;
        let (n, capped, latest) = missed_summary(&every_minute, tz, down, t, 1000);
        assert_eq!((n, capped), (1000, true));
        assert_eq!(latest, Some(t));
        assert_eq!(missed_fires(&cron, tz, t, t, 100).len(), 0);
    }
}
