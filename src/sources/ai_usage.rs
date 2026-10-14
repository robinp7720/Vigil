//! Local, incremental usage accounting. No credentials, network requests, or prompt storage.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use gtk4::glib::DateTime;
use serde_json::Value;

use super::SourceSupervisor;
use crate::{
    AiProviderUsage, AiQuota, AiQuotaEstimate, AiUsageState, SourceHealth, SourceId, StateUpdate,
    SystemUpdate, TokenUsage,
};

const POLL_INTERVAL: Duration = Duration::from_secs(15);
const MAX_LINE_BYTES: u64 = 8 * 1024 * 1024;
const ESTIMATE_LOOKBACK_SECONDS: i64 = 3600;
const ESTIMATE_MIN_SPAN_SECONDS: i64 = 900;
const ESTIMATE_MAX_AGE_SECONDS: i64 = 900;
const ESTIMATE_MIN_PERCENT_CHANGE: u8 = 2;
// Concurrent requests can report the same reset with a one-second difference.
const RESET_TOLERANCE_SECONDS: u64 = 60;

type QuotaHistory = BTreeMap<(i64, u64, Option<i64>), u8>;

#[derive(Clone, Copy)]
enum Provider {
    Codex,
    Claude,
}

#[derive(Default)]
struct LogUsage {
    inode: u64,
    offset: u64,
    previous: Option<TokenUsage>,
    // Codex copied/forked records share timestamp + cumulative counters.
    codex: BTreeMap<(String, TokenUsage), TokenUsage>,
    // Claude emits multiple assistant blocks with the same message/request IDs.
    claude: BTreeMap<(String, String), TokenUsage>,
    last_event_at: Option<i64>,
    quotas: Option<(i64, Vec<AiQuota>)>,
    quota_history: QuotaHistory,
}

pub struct AiUsageReader {
    codex_home: PathBuf,
    claude_home: PathBuf,
    day_start: i64,
    logs: BTreeMap<PathBuf, LogUsage>,
}

impl Default for AiUsageReader {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self::new(
            configured_home("CODEX_HOME", home.join(".codex")),
            configured_home("CLAUDE_CONFIG_DIR", home.join(".claude")),
        )
    }
}

impl AiUsageReader {
    pub fn new(codex_home: PathBuf, claude_home: PathBuf) -> Self {
        Self {
            codex_home,
            claude_home,
            day_start: 0,
            logs: BTreeMap::new(),
        }
    }

    pub fn snapshot(&mut self, now: i64) -> Result<AiUsageState> {
        let local = DateTime::from_unix_local(now)?;
        let start =
            DateTime::from_local(local.year(), local.month(), local.day_of_month(), 0, 0, 0.0)?;
        let day_start = start.to_unix();
        let day_end = start.add_days(1)?.to_unix();
        if self.day_start != day_start {
            self.logs.clear();
            self.day_start = day_start;
        }
        let mut seen = BTreeSet::new();
        let codex = self.read_provider(Provider::Codex, day_start, day_end, now, &mut seen)?;
        let claude = self.read_provider(Provider::Claude, day_start, day_end, now, &mut seen)?;
        self.logs.retain(|path, _| seen.contains(path));
        Ok(AiUsageState {
            day: start.format("%F")?.to_string(),
            codex,
            claude,
        })
    }

    fn read_provider(
        &mut self,
        provider: Provider,
        start: i64,
        end: i64,
        now: i64,
        seen: &mut BTreeSet<PathBuf>,
    ) -> Result<AiProviderUsage> {
        let roots = match provider {
            Provider::Codex => vec![
                self.codex_home.join("sessions"),
                self.codex_home.join("archived_sessions"),
            ],
            Provider::Claude => vec![self.claude_home.join("projects")],
        };
        let mut state = AiProviderUsage::default();
        let mut files = Vec::new();
        for root in roots {
            if root.try_exists()? {
                state.available = true;
                // Include the previous hour across midnight without counting its tokens today.
                collect_logs(&root, start - ESTIMATE_LOOKBACK_SECONDS, &mut files)?;
            }
        }
        let mut codex_events = BTreeMap::new();
        let mut claude_messages = BTreeMap::new();
        let mut quota_history = QuotaHistory::new();
        for path in files {
            seen.insert(path.clone());
            let log = self.logs.entry(path.clone()).or_default();
            read_log(&path, log, provider, start, end)
                .with_context(|| format!("reading AI usage from {}", path.display()))?;
            state.last_event_at = state.last_event_at.max(log.last_event_at);
            if let Some((observed, quotas)) = &log.quotas
                && *observed <= now
                && state
                    .quota_observed_at
                    .is_none_or(|previous| *observed > previous)
            {
                state.quota_observed_at = Some(*observed);
                state.quotas = quotas.clone();
            }
            log.quota_history.retain(|(epoch, _, _), _| {
                *epoch >= now - ESTIMATE_LOOKBACK_SECONDS - ESTIMATE_MAX_AGE_SECONDS
            });
            for (key, percent) in &log.quota_history {
                let previous = quota_history.entry(*key).or_default();
                *previous = (*previous).max(*percent);
            }
            for (key, usage) in &log.codex {
                codex_events.entry(key.clone()).or_insert(*usage);
            }
            for (key, usage) in &log.claude {
                keep_largest(&mut claude_messages, key.clone(), *usage);
            }
        }
        for usage in codex_events.values().chain(claude_messages.values()) {
            state.tokens.add(*usage);
        }
        if let Some(observed) = state.quota_observed_at {
            for quota in &mut state.quotas {
                quota.estimate = estimate_quota(quota, observed, now, &quota_history);
            }
        }
        Ok(state)
    }
}

fn configured_home(variable: &str, fallback: PathBuf) -> PathBuf {
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or(fallback)
}

fn collect_logs(root: &Path, start: i64, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_logs(&entry.path(), start, files)?;
        } else if kind.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            && entry.metadata()?.mtime() >= start
        {
            files.push(entry.path());
        }
    }
    Ok(())
}

fn read_log(
    path: &Path,
    log: &mut LogUsage,
    provider: Provider,
    start: i64,
    end: i64,
) -> Result<()> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if log.inode != metadata.ino() || metadata.len() < log.offset {
        *log = LogUsage {
            inode: metadata.ino(),
            ..LogUsage::default()
        };
    }
    if metadata.len() == log.offset {
        return Ok(());
    }
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(log.offset))?;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader
            .by_ref()
            .take(MAX_LINE_BYTES)
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        if !line.ends_with(b"\n") {
            if read as u64 == MAX_LINE_BYTES {
                // Ignore oversized tool output without retaining it in memory.
                let skipped = reader.skip_until(b'\n')?;
                if skipped > 0 {
                    log.offset += read as u64 + skipped as u64;
                    continue;
                }
            }
            // A writer may be halfway through the last event; retry it next poll.
            break;
        }
        log.offset += read as u64;
        if !line.windows(7).any(|window| window == b"\"usage\"")
            && !line.windows(13).any(|window| window == b"\"token_count\"")
        {
            continue;
        }
        let Ok(event) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let Some(timestamp) = event.get("timestamp").and_then(Value::as_str) else {
            continue;
        };
        let Ok(date) = DateTime::from_iso8601(timestamp, None) else {
            continue;
        };
        let epoch = date.to_unix();
        match provider {
            Provider::Codex => record_codex(log, &event, timestamp, epoch, start, end),
            Provider::Claude if (start..end).contains(&epoch) => record_claude(log, &event, epoch),
            Provider::Claude => {}
        }
    }
    Ok(())
}

fn record_codex(
    log: &mut LogUsage,
    event: &Value,
    timestamp: &str,
    epoch: i64,
    start: i64,
    end: i64,
) {
    let payload = &event["payload"];
    if event["type"] != "event_msg" || payload["type"] != "token_count" {
        return;
    }
    let info = &payload["info"];
    if let Ok(total) = serde_json::from_value::<TokenUsage>(info["total_token_usage"].clone()) {
        // Repeated usage snapshots are not new requests. On a counter reset, only
        // the last request is known; do not count an inherited session baseline.
        let delta = match log.previous {
            Some(previous) if total.total_tokens >= previous.total_tokens => {
                total.difference(previous)
            }
            _ => serde_json::from_value::<TokenUsage>(info["last_token_usage"].clone())
                .unwrap_or_default(),
        };
        log.previous = Some(total);
        if (start..end).contains(&epoch) && delta.total_tokens > 0 {
            log.codex.insert((timestamp.to_owned(), total), delta);
            log.last_event_at = Some(epoch);
        }
    }
    let limits = &payload["rate_limits"];
    if (start - ESTIMATE_LOOKBACK_SECONDS..end).contains(&epoch)
        && limits.is_object()
        && limits["limit_id"].as_str().is_none_or(|id| id == "codex")
    {
        let quotas = ["primary", "secondary"]
            .into_iter()
            .filter_map(|key| {
                let value = &limits[key];
                Some(AiQuota {
                    used_percent: value["used_percent"].as_f64()?.clamp(0.0, 100.0).round() as u8,
                    window_minutes: value["window_minutes"].as_u64()?,
                    resets_at: value["resets_at"].as_i64(),
                    estimate: AiQuotaEstimate::default(),
                })
            })
            .collect::<Vec<_>>();
        for quota in &quotas {
            let previous = log
                .quota_history
                .entry((epoch, quota.window_minutes, quota.resets_at))
                .or_default();
            *previous = (*previous).max(quota.used_percent);
        }
        if log
            .quotas
            .as_ref()
            .is_none_or(|(previous, _)| epoch >= *previous)
        {
            log.quotas = Some((epoch, quotas));
        }
    }
}

fn same_reset(left: Option<i64>, right: Option<i64>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.abs_diff(right) <= RESET_TOLERANCE_SECONDS,
        (None, None) => true,
        _ => false,
    }
}

fn estimate_quota(
    quota: &AiQuota,
    observed: i64,
    now: i64,
    history: &QuotaHistory,
) -> AiQuotaEstimate {
    if quota.resets_at.is_some_and(|reset| reset <= now) {
        return AiQuotaEstimate::ResetDue;
    }
    if observed > now || now - observed > ESTIMATE_MAX_AGE_SECONDS {
        return AiQuotaEstimate::Stale;
    }
    if quota.used_percent >= 100 {
        return AiQuotaEstimate::Exhausted;
    }
    let mut samples = BTreeMap::<i64, u8>::new();
    for ((epoch, window, reset), percent) in history {
        if *window == quota.window_minutes
            && same_reset(*reset, quota.resets_at)
            && (observed - ESTIMATE_LOOKBACK_SECONDS..=observed).contains(epoch)
        {
            let previous = samples.entry(*epoch).or_default();
            *previous = (*previous).max(*percent);
        }
    }
    // The last observed report is the endpoint. Small out-of-order percentage
    // dips are noise; a larger drop starts a new baseline (reset/account change).
    samples.insert(observed, quota.used_percent);
    let mut baseline = None;
    let mut high_water = 0_u8;
    for (epoch, percent) in samples {
        if baseline.is_none() || percent.saturating_add(2) < high_water {
            baseline = Some((epoch, percent));
            high_water = percent;
        } else {
            high_water = high_water.max(percent);
        }
    }
    let Some((first_at, first_percent)) = baseline else {
        return AiQuotaEstimate::Collecting;
    };
    let span = observed - first_at;
    if span < ESTIMATE_MIN_SPAN_SECONDS {
        return AiQuotaEstimate::Collecting;
    }
    let delta = quota.used_percent.saturating_sub(first_percent);
    if delta == 0 {
        return AiQuotaEstimate::NoRecentBurn {
            sample_seconds: span as u64,
        };
    }
    if delta < ESTIMATE_MIN_PERCENT_CHANGE {
        return AiQuotaEstimate::Collecting;
    }
    let remaining = u64::from(100 - quota.used_percent) * span as u64;
    let seconds_from_report = remaining.div_ceil(u64::from(delta));
    let exhausts_at = observed.saturating_add(seconds_from_report as i64);
    AiQuotaEstimate::Available {
        remaining_seconds: exhausts_at.saturating_sub(now).max(0) as u64,
        exhausts_at,
        sample_seconds: span as u64,
        used_percent_delta: delta,
        lasts_until_reset: quota.resets_at.map(|reset| exhausts_at >= reset),
    }
}

fn record_claude(log: &mut LogUsage, event: &Value, epoch: i64) {
    if event["type"] != "assistant" {
        return;
    }
    let message = &event["message"];
    let Some(id) = message["id"].as_str() else {
        return;
    };
    let usage = &message["usage"];
    let (Some(input), Some(output)) = (
        usage["input_tokens"].as_u64(),
        usage["output_tokens"].as_u64(),
    ) else {
        return;
    };
    let cached = usage["cache_read_input_tokens"]
        .as_u64()
        .unwrap_or_default();
    let written = usage["cache_creation_input_tokens"]
        .as_u64()
        .unwrap_or_default();
    let input = input.saturating_add(cached).saturating_add(written);
    let tokens = TokenUsage {
        input_tokens: input,
        cached_input_tokens: cached,
        cache_write_input_tokens: written,
        output_tokens: output,
        total_tokens: input.saturating_add(output),
        ..TokenUsage::default()
    };
    let request = event["requestId"].as_str().unwrap_or_default();
    keep_largest(&mut log.claude, (id.to_owned(), request.to_owned()), tokens);
    log.last_event_at = log.last_event_at.max(Some(epoch));
}

fn keep_largest<K: Ord>(entries: &mut BTreeMap<K, TokenUsage>, key: K, usage: TokenUsage) {
    let previous = entries.entry(key).or_default();
    if usage.total_tokens >= previous.total_tokens {
        *previous = usage;
    }
}

impl TokenUsage {
    fn add(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.cached_input_tokens = self
            .cached_input_tokens
            .saturating_add(other.cached_input_tokens);
        self.cache_write_input_tokens = self
            .cache_write_input_tokens
            .saturating_add(other.cache_write_input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.reasoning_output_tokens = self
            .reasoning_output_tokens
            .saturating_add(other.reasoning_output_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
    }

    fn difference(self, previous: Self) -> Self {
        Self {
            input_tokens: self.input_tokens.saturating_sub(previous.input_tokens),
            cached_input_tokens: self
                .cached_input_tokens
                .saturating_sub(previous.cached_input_tokens),
            cache_write_input_tokens: self
                .cache_write_input_tokens
                .saturating_sub(previous.cache_write_input_tokens),
            output_tokens: self.output_tokens.saturating_sub(previous.output_tokens),
            reasoning_output_tokens: self
                .reasoning_output_tokens
                .saturating_sub(previous.reasoning_output_tokens),
            total_tokens: self.total_tokens.saturating_sub(previous.total_tokens),
        }
    }
}

pub fn read_ai_usage() -> Result<AiUsageState> {
    AiUsageReader::default().snapshot(current_epoch())
}

fn current_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn spawn_ai_usage_source(
    sender: Sender<StateUpdate>,
    cancelled: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    let mut reader = AiUsageReader::default();
    SourceSupervisor::spawn(cancelled.clone(), POLL_INTERVAL, move || {
        match reader.snapshot(current_epoch()) {
            Ok(usage) => {
                if sender
                    .send(StateUpdate::System(SystemUpdate::AiUsage(usage)))
                    .is_err()
                {
                    cancelled.store(true, Ordering::Relaxed);
                }
                Ok(true)
            }
            Err(error) => {
                let _ = sender.send(StateUpdate::Health {
                    source: SourceId::AiUsage,
                    health: SourceHealth::Disconnected {
                        message: error.to_string(),
                    },
                });
                Err(error)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn quota(percent: u8, reset: i64) -> AiQuota {
        AiQuota {
            used_percent: percent,
            window_minutes: 10080,
            resets_at: Some(reset),
            estimate: AiQuotaEstimate::default(),
        }
    }

    #[test]
    fn estimate_uses_recent_quota_burn_and_compares_exhaustion_with_reset() {
        let now = 100_000;
        let mut quota = quota(40, now + 86400);
        let history = [
            ((now - 3600, 10080, quota.resets_at), 20),
            ((now, 10080, quota.resets_at), 40),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            estimate_quota(&quota, now, now, &history),
            AiQuotaEstimate::Available {
                remaining_seconds: 10800,
                exhausts_at: now + 10800,
                sample_seconds: 3600,
                used_percent_delta: 20,
                lasts_until_reset: Some(false),
            }
        );
        assert!(matches!(
            estimate_quota(&quota, now, now + 300, &history),
            AiQuotaEstimate::Available {
                remaining_seconds: 10500,
                ..
            }
        ));
        quota.resets_at = Some(now + 1800);
        let history = [((now - 3600, 10080, quota.resets_at), 20)]
            .into_iter()
            .collect();
        assert!(matches!(
            estimate_quota(&quota, now, now, &history),
            AiQuotaEstimate::Available {
                lasts_until_reset: Some(true),
                ..
            }
        ));
    }

    #[test]
    fn estimate_does_not_mix_windows_resets_or_old_history() {
        let now = 100_000;
        let quota = quota(40, now + 86400);
        let mut history: QuotaHistory = [
            ((now - 7200, 10080, quota.resets_at), 1),
            ((now - 3600, 300, quota.resets_at), 1),
            ((now - 3600, 10080, Some(now + 7200)), 1),
            ((now - 1800, 10080, Some(now + 86401)), 30),
        ]
        .into_iter()
        .collect();
        assert!(matches!(
            estimate_quota(&quota, now, now, &history),
            AiQuotaEstimate::Available {
                sample_seconds: 1800,
                used_percent_delta: 10,
                remaining_seconds: 10800,
                ..
            }
        ));
        history.remove(&(now - 1800, 10080, Some(now + 86401)));
        assert_eq!(
            estimate_quota(&quota, now, now, &history),
            AiQuotaEstimate::Collecting
        );
    }

    #[test]
    fn estimate_handles_low_burn_stale_reports_and_exhausted_or_reset_quotas() {
        let now = 100_000;
        let mut quota = quota(40, now + 86400);
        for (age, first, expected) in [
            (300, 20, AiQuotaEstimate::Collecting),
            (3600, 39, AiQuotaEstimate::Collecting),
            (
                3600,
                40,
                AiQuotaEstimate::NoRecentBurn {
                    sample_seconds: 3600,
                },
            ),
        ] {
            let history = [((now - age, 10080, quota.resets_at), first)]
                .into_iter()
                .collect();
            assert_eq!(estimate_quota(&quota, now, now, &history), expected);
        }
        let history = QuotaHistory::new();
        assert_eq!(
            estimate_quota(&quota, now, now + 901, &history),
            AiQuotaEstimate::Stale
        );
        assert_eq!(
            estimate_quota(&quota, now + 1, now, &history),
            AiQuotaEstimate::Stale
        );
        quota.used_percent = 100;
        assert_eq!(
            estimate_quota(&quota, now, now, &history),
            AiQuotaEstimate::Exhausted
        );
        quota.resets_at = Some(now);
        assert_eq!(
            estimate_quota(&quota, now, now, &history),
            AiQuotaEstimate::ResetDue
        );
    }

    #[test]
    fn a_large_percentage_drop_restarts_the_estimate_baseline() {
        let now = 100_000;
        let quota = quota(20, now + 86400);
        let history = [
            ((now - 3600, 10080, quota.resets_at), 80),
            ((now - 1800, 10080, quota.resets_at), 10),
            ((now - 900, 10080, quota.resets_at), 15),
            ((now - 800, 10080, quota.resets_at), 14),
        ]
        .into_iter()
        .collect();
        assert!(matches!(
            estimate_quota(&quota, now, now, &history),
            AiQuotaEstimate::Available {
                sample_seconds: 1800,
                used_percent_delta: 10,
                remaining_seconds: 14400,
                ..
            }
        ));
    }

    #[test]
    fn estimates_reconstruct_after_restart_and_across_local_midnight() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("codex/sessions/session.jsonl");
        let (start, _) = today();
        let now = start + 120;
        for (epoch, percent, total) in [(start - 1800, 20, 100), (now, 28, 200)] {
            let mut event = codex(epoch, total, 100);
            event["payload"]["rate_limits"] = json!({"limit_id":"codex","primary":{"used_percent":percent,"window_minutes":10080,"resets_at":now+86400}});
            append(&path, &event);
        }
        fs::copy(&path, path.with_file_name("fork.jsonl")).unwrap();
        let first = reader(temp.path()).snapshot(now).unwrap();
        let second = reader(temp.path()).snapshot(now).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.codex.tokens.total_tokens, 100);
        assert!(matches!(
            first.codex.quotas[0].estimate,
            AiQuotaEstimate::Available {
                sample_seconds: 1920,
                used_percent_delta: 8,
                remaining_seconds: 17280,
                ..
            }
        ));
    }

    fn today() -> (i64, i64) {
        let now = DateTime::now_local().unwrap();
        let start = DateTime::from_local(now.year(), now.month(), now.day_of_month(), 0, 0, 0.0)
            .unwrap()
            .to_unix();
        (start, start + 3600)
    }

    fn stamp(epoch: i64) -> String {
        DateTime::from_unix_utc(epoch)
            .unwrap()
            .format_iso8601()
            .unwrap()
            .to_string()
    }

    fn codex(epoch: i64, total: u64, last: u64) -> Value {
        json!({"type":"event_msg", "timestamp":stamp(epoch), "payload":{
            "type":"token_count", "info":{
                "total_token_usage":{"input_tokens":total - total / 10, "output_tokens":total / 10, "total_tokens":total},
                "last_token_usage":{"input_tokens":last - last / 10, "output_tokens":last / 10, "total_tokens":last}
            }
        }})
    }

    fn append(path: &Path, event: &Value) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap(),
            "{event}"
        )
        .unwrap();
    }

    fn reader(root: &Path) -> AiUsageReader {
        AiUsageReader::new(root.join("codex"), root.join("claude"))
    }

    #[test]
    fn codex_counts_daily_deltas_without_duplicate_or_forked_history() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("codex/sessions/old-session.jsonl");
        let (start, now) = today();
        append(&path, &codex(start - 60, 1000, 1000));
        append(&path, &codex(now, 1500, 500));
        append(&path, &codex(now + 1, 1500, 500));
        append(&path, &codex(now + 2, 1800, 300));
        fs::copy(&path, path.with_file_name("fork.jsonl")).unwrap();
        let mut reader = reader(temp.path());
        let snapshot = reader.snapshot(now + 3).unwrap();
        assert_eq!(snapshot.codex.tokens.total_tokens, 800);
        assert_eq!(snapshot.codex.tokens.input_tokens, 720);
        assert_eq!(reader.snapshot(now + 4).unwrap(), snapshot);
        append(&path, &codex(now + 5, 2000, 200));
        assert_eq!(
            reader.snapshot(now + 6).unwrap().codex.tokens.total_tokens,
            1000
        );
    }

    #[test]
    fn partial_records_malformed_lines_and_file_replacement_recover() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("codex/sessions/session.jsonl");
        let (_, now) = today();
        append(&path, &codex(now, 100, 100));
        let next = codex(now + 1, 300, 200).to_string();
        let midpoint = next.len() / 2;
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "broken JSON\n{}", &next[..midpoint]).unwrap();
        let mut reader = reader(temp.path());
        assert_eq!(reader.snapshot(now).unwrap().codex.tokens.total_tokens, 100);
        let offset = reader.logs[&path].offset;
        assert_eq!(reader.snapshot(now).unwrap().codex.tokens.total_tokens, 100);
        assert_eq!(reader.logs[&path].offset, offset);
        writeln!(file, "{}", &next[midpoint..]).unwrap();
        assert_eq!(
            reader.snapshot(now + 2).unwrap().codex.tokens.total_tokens,
            300
        );
        let replacement = path.with_extension("replacement");
        append(&replacement, &codex(now + 3, 50, 50));
        fs::rename(replacement, &path).unwrap();
        assert_eq!(
            reader.snapshot(now + 4).unwrap().codex.tokens.total_tokens,
            50
        );
        fs::write(&path, "").unwrap();
        assert_eq!(
            reader.snapshot(now + 5).unwrap().codex.tokens.total_tokens,
            0
        );
    }

    #[test]
    fn counter_resets_use_last_request_and_ignore_other_limit_buckets() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("codex/sessions/session.jsonl");
        let (_, now) = today();
        let mut event = codex(now, 9000, 100);
        event["payload"]["rate_limits"] = json!({"limit_id":"codex", "primary":{"used_percent":42.0,"window_minutes":10080,"resets_at":now+1000}});
        append(&path, &event);
        append(&path, &codex(now + 1, 200, 200));
        append(
            &path,
            &json!({"type":"event_msg", "timestamp":stamp(now+2), "payload":{"type":"token_count","info":null,"rate_limits":{"limit_id":"other-model","primary":{"used_percent":99,"window_minutes":300}}}}),
        );
        let state = reader(temp.path()).snapshot(now + 3).unwrap();
        assert_eq!(state.codex.tokens.total_tokens, 300);
        assert_eq!(state.codex.quotas[0].used_percent, 42);
        assert_eq!(state.codex.quotas[0].window_minutes, 10080);
    }

    #[test]
    fn claude_deduplicates_streaming_blocks_and_counts_cache_once() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("claude/projects/project/session.jsonl");
        let (start, now) = today();
        let mut event = json!({"type":"assistant", "timestamp":stamp(now), "requestId":"request-1", "message":{"id":"message-1","usage":{"input_tokens":10,"cache_read_input_tokens":100,"cache_creation_input_tokens":20,"output_tokens":5}}});
        append(&path, &event);
        event["timestamp"] = json!(stamp(now + 1));
        event["message"]["usage"]["output_tokens"] = json!(15);
        append(&path, &event);
        append(&path, &event);
        let copy = path.parent().unwrap().join("subagents/agent.jsonl");
        append(&copy, &event);
        event["timestamp"] = json!(stamp(start - 1));
        event["message"]["id"] = json!("old-message");
        append(&path, &event);
        let state = reader(temp.path()).snapshot(now + 2).unwrap();
        assert_eq!(state.claude.tokens.total_tokens, 145);
        assert_eq!(state.claude.tokens.input_tokens, 130);
        assert_eq!(state.claude.tokens.cached_input_tokens, 100);
        assert_eq!(state.claude.tokens.cache_write_input_tokens, 20);
        assert_eq!(state.claude.tokens.output_tokens, 15);
    }

    #[test]
    fn local_midnight_resets_daily_usage_in_a_resumed_session() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("codex/sessions/session.jsonl");
        let (start, now) = today();
        append(&path, &codex(start - 1, 1000, 1000));
        let mut reader = reader(temp.path());
        assert_eq!(
            reader
                .snapshot(start - 1)
                .unwrap()
                .codex
                .tokens
                .total_tokens,
            1000
        );
        assert_eq!(reader.snapshot(now).unwrap().codex.tokens.total_tokens, 0);
        append(&path, &codex(now, 1100, 100));
        assert_eq!(
            reader.snapshot(now + 1).unwrap().codex.tokens.total_tokens,
            100
        );
    }

    #[test]
    fn missing_provider_differs_from_zero_activity_and_archives_are_included() {
        let temp = tempfile::tempdir().unwrap();
        let (_, now) = today();
        let mut reader = reader(temp.path());
        assert!(!reader.snapshot(now).unwrap().codex.available);
        fs::create_dir_all(temp.path().join("codex/sessions")).unwrap();
        let state = reader.snapshot(now).unwrap();
        assert!(state.codex.available);
        assert_eq!(state.codex.tokens.total_tokens, 0);
        assert!(!state.claude.available);
        append(
            &temp.path().join("codex/archived_sessions/session.jsonl"),
            &codex(now, 100, 100),
        );
        assert_eq!(reader.snapshot(now).unwrap().codex.tokens.total_tokens, 100);
    }
}
