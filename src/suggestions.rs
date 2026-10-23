use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::{
    ActionIntent, AppConfig, BarSnapshot, ConnectivityState, DesktopContext, PlaybackStatus,
    PowerProfile, RoutineConfig, RoutineContextDetail,
};

const STATE_VERSION: u8 = 1;
const ROUTINE_WINDOW_MINUTES: u16 = 45;
const ROUTINE_DISMISS_SECONDS: i64 = 7 * 24 * 60 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SuggestionSource {
    Routine,
    Live,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SuggestionAction {
    Intent(ActionIntent),
    OpenControlCenter(DesktopContext),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub id: String,
    pub source: SuggestionSource,
    pub icon_name: String,
    pub title: String,
    pub reason: String,
    pub action_label: String,
    pub action: SuggestionAction,
    pub context: DesktopContext,
    pub priority: u8,
    generation: String,
    routine_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PersistedSuggestionState {
    #[serde(default = "state_version")]
    version: u8,
    #[serde(default)]
    observations: Vec<RoutineObservation>,
    #[serde(default)]
    feedback: BTreeMap<String, RoutineFeedback>,
}

impl Default for PersistedSuggestionState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            observations: Vec::new(),
            feedback: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct RoutineObservation {
    action: ActionIntent,
    action_key: String,
    observed_at: i64,
    day_key: i64,
    minute_of_day: u16,
    context: RoutineContext,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct RoutineFeedback {
    dismissals: u8,
    suppressed_until: Option<i64>,
    retired_at: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct RoutineContext {
    output: Option<String>,
    app_id: Option<String>,
    window_title: Option<String>,
    calendar_title: Option<String>,
    calendar_location: Option<String>,
    network_name: Option<String>,
    bluetooth_device: Option<String>,
    activity_path: Option<PathBuf>,
    media_title: Option<String>,
    on_battery: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LiveDismissal {
    generation: String,
}

#[derive(Debug)]
pub struct SuggestionEngine {
    state_path: Option<PathBuf>,
    persisted: PersistedSuggestionState,
    live_dismissals: BTreeMap<String, LiveDismissal>,
    visible: BTreeMap<String, Suggestion>,
}

impl Default for SuggestionEngine {
    fn default() -> Self {
        Self {
            state_path: None,
            persisted: PersistedSuggestionState {
                version: STATE_VERSION,
                ..PersistedSuggestionState::default()
            },
            live_dismissals: BTreeMap::new(),
            visible: BTreeMap::new(),
        }
    }
}

impl SuggestionEngine {
    pub fn load() -> Result<Self> {
        Self::load_at(suggestion_state_path()?)
    }

    fn load_at(state_path: PathBuf) -> Result<Self> {
        let persisted = match fs::read_to_string(&state_path) {
            Ok(contents) => match serde_json::from_str::<PersistedSuggestionState>(&contents) {
                Ok(state) if state.version == STATE_VERSION => state,
                Ok(state) => {
                    warn!(
                        "ignoring unsupported suggestion state version {} in {}",
                        state.version,
                        state_path.display()
                    );
                    PersistedSuggestionState::default()
                }
                Err(error) => {
                    warn!(
                        "ignoring corrupt suggestion state {}: {error}",
                        state_path.display()
                    );
                    PersistedSuggestionState::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                PersistedSuggestionState::default()
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read {}", state_path.display()));
            }
        };

        Ok(Self {
            state_path: Some(state_path),
            persisted: PersistedSuggestionState {
                version: STATE_VERSION,
                ..persisted
            },
            live_dismissals: BTreeMap::new(),
            visible: BTreeMap::new(),
        })
    }

    pub fn suggestions(
        &mut self,
        snapshot: &BarSnapshot,
        config: &AppConfig,
        now_epoch: i64,
    ) -> Vec<Suggestion> {
        if self.prune(&config.routines, now_epoch) {
            self.persist_or_warn();
        }

        let mut suggestions = if config.experience.suggestions {
            live_suggestions(snapshot, config, now_epoch)
        } else {
            Vec::new()
        };
        if config.experience.suggestions && config.routines.enabled {
            suggestions.extend(self.routine_suggestions(snapshot, &config.routines, now_epoch));
        }

        suggestions.retain(|suggestion| !self.is_dismissed(suggestion, now_epoch));
        suggestions.sort_by_key(|suggestion| {
            (
                suggestion.priority,
                suggestion.source,
                suggestion.title.clone(),
            )
        });
        suggestions.reverse();
        suggestions.truncate(3);
        self.visible = suggestions
            .iter()
            .cloned()
            .map(|suggestion| (suggestion.id.clone(), suggestion))
            .collect();
        suggestions
    }

    pub fn dismiss(&mut self, id: &str, now_epoch: i64) -> bool {
        let Some(suggestion) = self.visible.get(id).cloned() else {
            return false;
        };
        if let Some(key) = suggestion.routine_key {
            let feedback = self.persisted.feedback.entry(key).or_default();
            feedback.dismissals = feedback.dismissals.saturating_add(1);
            feedback.suppressed_until = Some(now_epoch.saturating_add(ROUTINE_DISMISS_SECONDS));
            if feedback.dismissals >= 3 {
                feedback.retired_at = Some(now_epoch);
            }
            self.persist_or_warn();
        } else {
            self.live_dismissals.insert(
                suggestion.id,
                LiveDismissal {
                    generation: suggestion.generation,
                },
            );
        }
        true
    }

    pub fn record_success(
        &mut self,
        origin: &str,
        action: &ActionIntent,
        snapshot: &BarSnapshot,
        config: &RoutineConfig,
        now_epoch: i64,
    ) -> bool {
        if !config.enabled || !routine_action_allowed(action) {
            return false;
        }

        if let Some(id) = origin.strip_prefix("suggestion-action|")
            && let Some(suggestion) = self.visible.get(id)
            && let Some(key) = suggestion.routine_key.as_ref()
        {
            self.persisted.feedback.remove(key);
        }

        let action_key = action_key(action);
        let (day_key, minute_of_day) = local_time_parts(now_epoch);
        self.persisted.observations.push(RoutineObservation {
            action: action.clone(),
            action_key: action_key.clone(),
            observed_at: now_epoch,
            day_key,
            minute_of_day,
            context: capture_context(snapshot, config.context),
        });
        if let Some(retired_at) = self
            .persisted
            .feedback
            .get(&action_key)
            .and_then(|feedback| feedback.retired_at)
        {
            let retrained_days = self
                .persisted
                .observations
                .iter()
                .filter(|observation| {
                    observation.action_key == action_key && observation.observed_at > retired_at
                })
                .map(|observation| observation.day_key)
                .collect::<BTreeSet<_>>()
                .len();
            if retrained_days >= usize::from(config.minimum_repetitions) {
                self.persisted.feedback.remove(&action_key);
            }
        }
        self.prune(config, now_epoch);
        self.persist_or_warn();
        true
    }

    pub fn clear_history(&mut self) -> Result<()> {
        let previous = std::mem::take(&mut self.persisted);
        if let Err(error) = self.persist() {
            self.persisted = previous;
            return Err(error);
        }
        self.visible
            .retain(|_, suggestion| !matches!(suggestion.source, SuggestionSource::Routine));
        Ok(())
    }

    pub fn observation_count(&self) -> usize {
        self.persisted.observations.len()
    }

    fn routine_suggestions(
        &self,
        snapshot: &BarSnapshot,
        config: &RoutineConfig,
        now_epoch: i64,
    ) -> Vec<Suggestion> {
        let (_, now_minute) = local_time_parts(now_epoch);
        let current_context = capture_context(snapshot, config.context);
        let mut by_action = BTreeMap::<String, Vec<&RoutineObservation>>::new();
        for observation in &self.persisted.observations {
            by_action
                .entry(observation.action_key.clone())
                .or_default()
                .push(observation);
        }

        by_action
            .into_iter()
            .filter_map(|(key, observations)| {
                let feedback = self.persisted.feedback.get(&key);
                let retired_at = feedback.and_then(|feedback| feedback.retired_at);
                let retrained = retired_at.is_some_and(|retired| {
                    observations
                        .iter()
                        .filter(|observation| observation.observed_at > retired)
                        .map(|observation| observation.day_key)
                        .collect::<BTreeSet<_>>()
                        .len()
                        >= usize::from(config.minimum_repetitions)
                });
                if !retrained
                    && feedback
                        .and_then(|feedback| feedback.suppressed_until)
                        .is_some_and(|until| now_epoch < until)
                {
                    return None;
                }
                let matching = observations
                    .into_iter()
                    .filter(|observation| {
                        retired_at.is_none_or(|retired| observation.observed_at > retired)
                            && minute_distance(observation.minute_of_day, now_minute)
                                <= ROUTINE_WINDOW_MINUTES
                            && context_matches(
                                &observation.context,
                                &current_context,
                                config.context,
                            )
                    })
                    .collect::<Vec<_>>();
                let distinct_days = matching
                    .iter()
                    .map(|observation| observation.day_key)
                    .collect::<BTreeSet<_>>()
                    .len();
                if distinct_days < usize::from(config.minimum_repetitions) {
                    return None;
                }

                let latest = matching
                    .into_iter()
                    .max_by_key(|observation| observation.observed_at)?;
                if !action_still_available(&latest.action, snapshot) {
                    return None;
                }
                let (title, action_label, icon_name, context) =
                    routine_presentation(&latest.action)?;
                let reason = routine_reason(&latest.context, now_minute);
                Some(Suggestion {
                    id: format!("routine-{:016x}", stable_hash(&key)),
                    source: SuggestionSource::Routine,
                    icon_name: icon_name.to_string(),
                    title,
                    reason,
                    action_label: action_label.to_string(),
                    action: SuggestionAction::Intent(latest.action.clone()),
                    context,
                    priority: 35,
                    generation: key.clone(),
                    routine_key: Some(key),
                })
            })
            .collect()
    }

    fn is_dismissed(&self, suggestion: &Suggestion, now_epoch: i64) -> bool {
        if let Some(key) = suggestion.routine_key.as_ref() {
            return self.persisted.feedback.get(key).is_some_and(|feedback| {
                feedback
                    .suppressed_until
                    .is_some_and(|until| now_epoch < until)
            });
        }

        self.live_dismissals
            .get(&suggestion.id)
            .is_some_and(|dismissal| dismissal.generation == suggestion.generation)
    }

    fn prune(&mut self, config: &RoutineConfig, now_epoch: i64) -> bool {
        let retention = i64::from(config.retention_days).saturating_mul(24 * 60 * 60);
        let oldest = now_epoch.saturating_sub(retention);
        let previous_observations = self.persisted.observations.len();
        self.persisted
            .observations
            .retain(|observation| observation.observed_at >= oldest);

        let active_keys = self
            .persisted
            .observations
            .iter()
            .map(|observation| observation.action_key.as_str())
            .collect::<BTreeSet<_>>();
        let previous_feedback = self.persisted.feedback.len();
        self.persisted
            .feedback
            .retain(|key, _| active_keys.contains(key.as_str()));

        previous_observations != self.persisted.observations.len()
            || previous_feedback != self.persisted.feedback.len()
    }

    fn persist_or_warn(&self) {
        if let Err(error) = self.persist() {
            warn!("failed to persist suggestion state: {error:#}");
        }
    }

    fn persist(&self) -> Result<()> {
        let Some(state_path) = self.state_path.as_ref() else {
            return Ok(());
        };
        let parent = state_path
            .parent()
            .ok_or_else(|| anyhow!("suggestion state path has no parent"))?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to protect {}", parent.display()))?;

        let temp_path = state_path.with_extension(format!(
            "json.tmp-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time")
                .as_nanos()
        ));
        let bytes = serde_json::to_vec(&self.persisted).context("serialize suggestion state")?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp_path)
            .with_context(|| format!("failed to create {}", temp_path.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("failed to write {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", temp_path.display()))?;
        fs::rename(&temp_path, state_path)
            .with_context(|| format!("failed to replace {}", state_path.display()))?;
        fs::set_permissions(state_path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to protect {}", state_path.display()))
    }
}

fn state_version() -> u8 {
    STATE_VERSION
}

fn suggestion_state_path() -> Result<PathBuf> {
    let root =
        dirs::state_dir().ok_or_else(|| anyhow!("failed to resolve Vigil state directory"))?;
    Ok(root.join("vigil").join("routines.json"))
}

fn live_suggestions(snapshot: &BarSnapshot, config: &AppConfig, now_epoch: i64) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();
    let power = &snapshot.system.power;
    if power.battery_present
        && !power.charging
        && power
            .battery_percent
            .is_some_and(|percent| percent <= config.thresholds.battery_low_percent)
        && power.profile != PowerProfile::PowerSaver
    {
        let percent = power.battery_percent.unwrap_or_default();
        suggestions.push(live_suggestion(
            "power-saver",
            format!("Stretch battery at {percent}%"),
            "Battery is low and Power Saver is available",
            "Use Power Saver",
            "power-profile-power-saver-symbolic",
            SuggestionAction::Intent(ActionIntent::SetPowerProfile {
                profile: PowerProfile::PowerSaver,
            }),
            DesktopContext::Power,
            90,
            format!("{percent}:{}", power.changed_at),
        ));
    }

    if snapshot
        .system
        .media
        .as_ref()
        .is_some_and(|media| media.status == PlaybackStatus::Playing)
        && snapshot.system.audio.muted
    {
        suggestions.push(live_suggestion(
            "unmute-media",
            "Media is playing silently",
            "Playback is active while system audio is muted",
            "Unmute",
            "audio-volume-muted-symbolic",
            SuggestionAction::Intent(ActionIntent::ToggleMute),
            DesktopContext::Audio,
            76,
            snapshot
                .system
                .media
                .as_ref()
                .map(|media| format!("{}:{}", media.player, media.changed_at))
                .unwrap_or_default(),
        ));
    }

    if let Some(event) = snapshot.system.calendar.as_ref() {
        let remaining = event.start_epoch.saturating_sub(now_epoch);
        let soon = i64::try_from(config.thresholds.calendar_soon_minutes)
            .unwrap_or(i64::MAX)
            .saturating_mul(60);
        if (-60..=soon).contains(&remaining) {
            suggestions.push(live_suggestion(
                "calendar-agenda",
                format!("Get ready for {}", event.title),
                "The next calendar event is about to begin",
                "Open agenda",
                "x-office-calendar-symbolic",
                SuggestionAction::OpenControlCenter(DesktopContext::Clock),
                DesktopContext::Clock,
                84,
                format!("{}:{}", event.id, event.start_epoch),
            ));
        }
    }

    if let Some(timer) = snapshot
        .system
        .timers
        .iter()
        .find(|timer| timer.completed)
        .or_else(|| {
            let soon = config.thresholds.timer_soon_minutes.saturating_mul(60);
            snapshot
                .system
                .timers
                .iter()
                .filter(|timer| !timer.completed && timer.remaining_seconds <= soon)
                .min_by_key(|timer| timer.remaining_seconds)
        })
    {
        let (title, action_label, action, priority) = if timer.completed {
            (
                format!("{} is complete", timer.label),
                "Clear",
                SuggestionAction::Intent(ActionIntent::CancelTimer {
                    id: timer.id.clone(),
                }),
                94,
            )
        } else {
            (
                format!("{} is nearly done", timer.label),
                "Pause",
                SuggestionAction::Intent(ActionIntent::PauseTimer {
                    id: timer.id.clone(),
                }),
                86,
            )
        };
        suggestions.push(live_suggestion(
            "timer",
            title,
            "A running timer needs attention",
            action_label,
            "alarm-symbolic",
            action,
            DesktopContext::Clock,
            priority,
            format!(
                "{}:{}:{}",
                timer.id, timer.completed, timer.remaining_seconds
            ),
        ));
    }

    let network = &snapshot.system.network;
    if network.wifi_available && network.wifi_enabled == Some(false) {
        suggestions.push(live_suggestion(
            "wifi-offline",
            "Wi-Fi is available but off",
            "Turn it on to recover wireless connectivity",
            "Turn on",
            "network-wireless-offline-symbolic",
            SuggestionAction::Intent(ActionIntent::SetWifiEnabled { enabled: true }),
            DesktopContext::Network,
            70,
            format!("{:?}:{:?}", network.connectivity, network.wifi_enabled),
        ));
    } else if network.wifi_available
        && network.wifi_enabled == Some(true)
        && network.connectivity == ConnectivityState::Disconnected
    {
        suggestions.push(live_suggestion(
            "network-recovery",
            "Network needs attention",
            "Wi-Fi is on, but no connection is active",
            "Review",
            "network-offline-symbolic",
            SuggestionAction::OpenControlCenter(DesktopContext::Network),
            DesktopContext::Network,
            68,
            format!("{:?}:{:?}", network.connectivity, network.label),
        ));
    }

    if snapshot.system.bluetooth.pairing_prompt.is_some() {
        suggestions.push(live_suggestion(
            "bluetooth-pairing",
            "Bluetooth pairing is waiting",
            "A nearby device needs an explicit response",
            "Review",
            "bluetooth-active-symbolic",
            SuggestionAction::OpenControlCenter(DesktopContext::Bluetooth),
            DesktopContext::Bluetooth,
            88,
            snapshot
                .system
                .bluetooth
                .pairing_prompt
                .as_ref()
                .map(|prompt| prompt.id.to_string())
                .unwrap_or_default(),
        ));
    }

    let pressure = snapshot
        .system
        .resources
        .cpu_percent
        .into_iter()
        .chain(snapshot.system.resources.memory_percent)
        .max();
    if pressure.is_some_and(|percent| percent >= 90) {
        suggestions.push(live_suggestion(
            "resource-pressure",
            "System pressure is high",
            "CPU or memory use crossed 90%",
            "Inspect",
            "utilities-system-monitor-symbolic",
            SuggestionAction::OpenControlCenter(DesktopContext::Resources),
            DesktopContext::Resources,
            72,
            pressure.unwrap_or_default().to_string(),
        ));
    }

    suggestions
}

#[allow(clippy::too_many_arguments)]
fn live_suggestion(
    id: &str,
    title: impl Into<String>,
    reason: impl Into<String>,
    action_label: impl Into<String>,
    icon_name: impl Into<String>,
    action: SuggestionAction,
    context: DesktopContext,
    priority: u8,
    generation: String,
) -> Suggestion {
    Suggestion {
        id: format!("live-{id}"),
        source: SuggestionSource::Live,
        icon_name: icon_name.into(),
        title: title.into(),
        reason: reason.into(),
        action_label: action_label.into(),
        action,
        context,
        priority,
        generation,
        routine_key: None,
    }
}

fn routine_action_allowed(action: &ActionIntent) -> bool {
    matches!(
        action,
        ActionIntent::SelectKeyboardLayout { .. }
            | ActionIntent::SetWifiEnabled { .. }
            | ActionIntent::SetBluetoothPowered { .. }
            | ActionIntent::ConnectBluetoothDevice { .. }
            | ActionIntent::SetAudioOutput { .. }
            | ActionIntent::SetPowerProfile { .. }
            | ActionIntent::StartTimer { .. }
            | ActionIntent::OpenCalendar
    )
}

fn action_still_available(action: &ActionIntent, snapshot: &BarSnapshot) -> bool {
    match action {
        ActionIntent::SelectKeyboardLayout { index } => {
            snapshot
                .system
                .keyboard_layout
                .layouts
                .iter()
                .any(|layout| layout.index == *index)
                && snapshot.system.keyboard_layout.current_index != Some(*index)
        }
        ActionIntent::SetWifiEnabled { enabled } => {
            snapshot.system.network.wifi_available
                && snapshot.system.network.wifi_enabled != Some(*enabled)
        }
        ActionIntent::SetBluetoothPowered { powered } => {
            snapshot.system.bluetooth.available && snapshot.system.bluetooth.powered != *powered
        }
        ActionIntent::ConnectBluetoothDevice { address } => snapshot
            .system
            .bluetooth
            .devices
            .iter()
            .any(|device| device.address == *address && device.paired && !device.connected),
        ActionIntent::SetAudioOutput { sink_name } => snapshot
            .system
            .audio
            .outputs
            .iter()
            .any(|output| output.name == *sink_name && !output.is_default),
        ActionIntent::SetPowerProfile { profile } => snapshot.system.power.profile != *profile,
        ActionIntent::StartTimer { .. } | ActionIntent::OpenCalendar => true,
        _ => false,
    }
}

fn routine_presentation(
    action: &ActionIntent,
) -> Option<(String, &'static str, &'static str, DesktopContext)> {
    match action {
        ActionIntent::SelectKeyboardLayout { .. } => Some((
            "Switch to your usual layout".to_string(),
            "Switch",
            "input-keyboard-symbolic",
            DesktopContext::Keyboard,
        )),
        ActionIntent::SetWifiEnabled { enabled } => Some((
            format!("Turn Wi-Fi {}", if *enabled { "on" } else { "off" }),
            if *enabled { "Turn on" } else { "Turn off" },
            "network-wireless-symbolic",
            DesktopContext::Network,
        )),
        ActionIntent::SetBluetoothPowered { powered } => Some((
            format!("Turn Bluetooth {}", if *powered { "on" } else { "off" }),
            if *powered { "Turn on" } else { "Turn off" },
            "bluetooth-symbolic",
            DesktopContext::Bluetooth,
        )),
        ActionIntent::ConnectBluetoothDevice { .. } => Some((
            "Reconnect your usual device".to_string(),
            "Connect",
            "bluetooth-active-symbolic",
            DesktopContext::Bluetooth,
        )),
        ActionIntent::SetAudioOutput { .. } => Some((
            "Use your usual audio output".to_string(),
            "Switch",
            "audio-speakers-symbolic",
            DesktopContext::Audio,
        )),
        ActionIntent::SetPowerProfile { profile } => Some((
            format!("Switch to {}", power_profile_name(profile)),
            "Switch",
            "power-profile-balanced-symbolic",
            DesktopContext::Power,
        )),
        ActionIntent::StartTimer {
            label,
            duration_seconds,
        } => Some((
            format!(
                "Start your usual {} timer ({})",
                label,
                duration_label(*duration_seconds)
            ),
            "Start",
            "alarm-symbolic",
            DesktopContext::Clock,
        )),
        ActionIntent::OpenCalendar => Some((
            "Open your calendar".to_string(),
            "Open",
            "x-office-calendar-symbolic",
            DesktopContext::Clock,
        )),
        _ => None,
    }
}

fn capture_context(snapshot: &BarSnapshot, detail: RoutineContextDetail) -> RoutineContext {
    let focused_output = snapshot
        .focused_output
        .as_ref()
        .and_then(|output| snapshot.outputs.get(output));
    let focused_window = focused_output.and_then(|output| output.focused_window.as_ref());
    let latest_activity = snapshot
        .activities
        .items
        .values()
        .max_by_key(|activity| activity.started_at);
    let on_battery = snapshot.system.power.battery_present && !snapshot.system.power.charging;

    match detail {
        RoutineContextDetail::TimingOnly => RoutineContext::default(),
        RoutineContextDetail::MetadataMinimized => RoutineContext {
            output: snapshot.focused_output.clone(),
            app_id: focused_window.and_then(|window| window.app_id.clone()),
            on_battery,
            ..RoutineContext::default()
        },
        RoutineContextDetail::Rich => RoutineContext {
            output: snapshot.focused_output.clone(),
            app_id: focused_window.and_then(|window| window.app_id.clone()),
            window_title: focused_window.map(|window| window.title.clone()),
            calendar_title: snapshot
                .system
                .calendar
                .as_ref()
                .map(|event| event.title.clone()),
            calendar_location: snapshot
                .system
                .calendar
                .as_ref()
                .and_then(|event| event.location.clone()),
            network_name: snapshot.system.network.label.clone(),
            bluetooth_device: snapshot.system.bluetooth.connected_device.clone(),
            activity_path: latest_activity.map(|activity| activity.cwd.clone()),
            media_title: snapshot
                .system
                .media
                .as_ref()
                .and_then(|media| media.title.clone()),
            on_battery,
        },
    }
}

fn context_matches(
    observed: &RoutineContext,
    current: &RoutineContext,
    detail: RoutineContextDetail,
) -> bool {
    match detail {
        RoutineContextDetail::TimingOnly => true,
        RoutineContextDetail::MetadataMinimized => {
            observed.on_battery == current.on_battery
                && option_matches(&observed.output, &current.output)
                && option_matches(&observed.app_id, &current.app_id)
        }
        RoutineContextDetail::Rich => {
            let anchors = [
                option_matches(&observed.output, &current.output),
                option_matches(&observed.app_id, &current.app_id),
                option_matches(&observed.window_title, &current.window_title),
                option_matches(&observed.calendar_title, &current.calendar_title),
                option_matches(&observed.calendar_location, &current.calendar_location),
                option_matches(&observed.network_name, &current.network_name),
                option_matches(&observed.bluetooth_device, &current.bluetooth_device),
                option_matches(&observed.activity_path, &current.activity_path),
                option_matches(&observed.media_title, &current.media_title),
            ];
            observed.on_battery == current.on_battery && anchors.into_iter().any(|matches| matches)
        }
    }
}

fn option_matches<T: PartialEq>(left: &Option<T>, right: &Option<T>) -> bool {
    left.is_some() && left == right
}

fn routine_reason(context: &RoutineContext, minute: u16) -> String {
    if let Some(title) = context.calendar_title.as_deref() {
        return format!("You often do this around “{title}”");
    }
    if let Some(title) = context.window_title.as_deref() {
        return format!("You often do this while “{title}” is focused");
    }
    if let Some(device) = context.bluetooth_device.as_deref() {
        return format!("You often do this with {device} connected");
    }
    if let Some(network) = context.network_name.as_deref() {
        return format!("You often do this on {network}");
    }
    let hour = minute / 60;
    let minute = minute % 60;
    format!("You often do this around {hour:02}:{minute:02}")
}

fn action_key(action: &ActionIntent) -> String {
    serde_json::to_string(action).unwrap_or_else(|_| format!("{action:?}"))
}

fn minute_distance(left: u16, right: u16) -> u16 {
    let direct = left.abs_diff(right);
    direct.min(24 * 60 - direct)
}

fn local_time_parts(epoch: i64) -> (i64, u16) {
    let seconds = libc::time_t::try_from(epoch).unwrap_or(libc::time_t::MAX);
    let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
    let result = unsafe { libc::localtime_r(&seconds, local.as_mut_ptr()) };
    if result.is_null() {
        return (
            epoch.div_euclid(24 * 60 * 60),
            u16::try_from(epoch.rem_euclid(24 * 60 * 60) / 60).unwrap_or_default(),
        );
    }
    let local = unsafe { local.assume_init() };
    let day_key = i64::from(local.tm_year)
        .saturating_mul(400)
        .saturating_add(i64::from(local.tm_yday));
    let minute = i64::from(local.tm_hour)
        .saturating_mul(60)
        .saturating_add(i64::from(local.tm_min));
    (day_key, u16::try_from(minute).unwrap_or_default())
}

fn stable_hash(text: &str) -> u64 {
    text.as_bytes()
        .iter()
        .fold(0xcbf29ce484222325, |hash, byte| {
            hash.wrapping_mul(0x100000001b3) ^ u64::from(*byte)
        })
}

fn power_profile_name(profile: &PowerProfile) -> &'static str {
    match profile {
        PowerProfile::Performance => "Performance",
        PowerProfile::Balanced => "Balanced",
        PowerProfile::PowerSaver => "Power Saver",
    }
}

fn duration_label(seconds: u64) -> String {
    if seconds.is_multiple_of(3_600) {
        format!("{}h", seconds / 3_600)
    } else if seconds.is_multiple_of(60) {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use crate::{
        ActionIntent, AppConfig, AudioState, BarSnapshot, CalendarEvent, MediaState,
        PlaybackStatus, PowerProfile, PowerState, RoutineContextDetail,
    };

    use super::{SuggestionAction, SuggestionEngine, SuggestionSource};

    #[test]
    fn live_suggestions_are_ranked_and_never_execute() {
        let mut engine = SuggestionEngine::default();
        let mut snapshot = BarSnapshot::default();
        snapshot.system.power = PowerState {
            battery_present: true,
            battery_percent: Some(12),
            charging: false,
            profile: PowerProfile::Balanced,
            changed_at: 7,
            ..PowerState::default()
        };
        snapshot.system.audio = AudioState {
            muted: true,
            ..AudioState::default()
        };
        snapshot.system.media = Some(MediaState {
            player: "player".to_string(),
            status: PlaybackStatus::Playing,
            title: Some("Track".to_string()),
            changed_at: 8,
            ..MediaState::default()
        });

        let suggestions = engine.suggestions(&snapshot, &AppConfig::default(), 1_800_000_000);

        assert_eq!(suggestions[0].id, "live-power-saver");
        assert!(matches!(
            suggestions[0].action,
            SuggestionAction::Intent(ActionIntent::SetPowerProfile {
                profile: PowerProfile::PowerSaver
            })
        ));
        assert_eq!(engine.observation_count(), 0);
    }

    #[test]
    fn live_dismissal_holds_only_for_the_same_generation() {
        let mut engine = SuggestionEngine::default();
        let mut snapshot = BarSnapshot::default();
        snapshot.system.calendar = Some(CalendarEvent {
            id: "meeting".to_string(),
            title: "Review".to_string(),
            start_epoch: 1_800_000_300,
            ..CalendarEvent::default()
        });
        let config = AppConfig::default();
        let initial = engine.suggestions(&snapshot, &config, 1_800_000_000);
        assert!(engine.dismiss(&initial[0].id, 1_800_000_000));
        assert!(
            engine
                .suggestions(&snapshot, &config, 1_800_000_010)
                .is_empty()
        );

        snapshot.system.calendar.as_mut().unwrap().start_epoch += 300;
        assert!(
            !engine
                .suggestions(&snapshot, &config, 1_800_000_010)
                .is_empty()
        );
    }

    #[test]
    fn routine_requires_three_distinct_days_in_matching_context() {
        let mut engine = SuggestionEngine::default();
        let mut config = AppConfig::default();
        config.routines.context = RoutineContextDetail::TimingOnly;
        let snapshot = BarSnapshot::default();
        let action = ActionIntent::StartTimer {
            label: "Focus".to_string(),
            duration_seconds: 25 * 60,
        };
        let day = 24 * 60 * 60;
        for offset in [0, day, day * 2] {
            assert!(engine.record_success(
                "control-center:clock:start-timer",
                &action,
                &snapshot,
                &config.routines,
                1_800_000_000 + offset,
            ));
        }

        let suggestions = engine.suggestions(&snapshot, &config, 1_800_000_000 + day * 3);
        assert!(suggestions.iter().any(|suggestion| {
            suggestion.source == SuggestionSource::Routine && suggestion.title.contains("Focus")
        }));
    }

    #[test]
    fn noisy_and_destructive_actions_are_not_learned() {
        let mut engine = SuggestionEngine::default();
        let snapshot = BarSnapshot::default();
        let config = AppConfig::default();

        assert!(!engine.record_success(
            "slider",
            &ActionIntent::SetVolumePercent { percent: 42 },
            &snapshot,
            &config.routines,
            1_800_000_000,
        ));
        assert!(!engine.record_success(
            "forget",
            &ActionIntent::ForgetBluetoothDevice {
                address: "AA:BB".to_string()
            },
            &snapshot,
            &config.routines,
            1_800_000_000,
        ));
        assert_eq!(engine.observation_count(), 0);
    }

    #[test]
    fn retention_prunes_old_observations() {
        let mut engine = SuggestionEngine::default();
        let mut config = AppConfig::default();
        config.routines.context = RoutineContextDetail::TimingOnly;
        config.routines.retention_days = 7;
        let snapshot = BarSnapshot::default();
        let action = ActionIntent::OpenCalendar;
        assert!(engine.record_success(
            "calendar",
            &action,
            &snapshot,
            &config.routines,
            1_800_000_000,
        ));

        engine.suggestions(&snapshot, &config, 1_800_000_000 + 8 * 24 * 60 * 60);
        assert_eq!(engine.observation_count(), 0);
    }

    #[test]
    fn three_dismissals_retire_a_routine_until_it_is_retrained() {
        let mut engine = SuggestionEngine::default();
        let mut config = AppConfig::default();
        config.routines.context = RoutineContextDetail::TimingOnly;
        let snapshot = BarSnapshot::default();
        let action = ActionIntent::OpenCalendar;
        let day = 24 * 60 * 60;
        let base = 1_800_000_000;
        for offset in [0, day, day * 2] {
            engine.record_success(
                "calendar",
                &action,
                &snapshot,
                &config.routines,
                base + offset,
            );
        }
        for week in [1_i64, 2, 3] {
            let now = base + day * 2 + week * 7 * day;
            let suggestions = engine.suggestions(&snapshot, &config, now);
            let routine = suggestions
                .iter()
                .find(|suggestion| suggestion.source == SuggestionSource::Routine)
                .expect("routine before retirement");
            assert!(engine.dismiss(&routine.id, now));
        }

        let retired_at = base + day * 2 + 22 * day;
        assert!(
            engine
                .suggestions(&snapshot, &config, retired_at)
                .iter()
                .all(|suggestion| suggestion.source != SuggestionSource::Routine)
        );

        for offset in [24_i64, 25, 26] {
            engine.record_success(
                "calendar",
                &action,
                &snapshot,
                &config.routines,
                base + offset * day,
            );
        }
        assert!(
            engine
                .suggestions(&snapshot, &config, base + 27 * day)
                .iter()
                .any(|suggestion| suggestion.source == SuggestionSource::Routine)
        );
    }

    #[test]
    fn persistence_is_user_only_and_corrupt_state_recovers() {
        let root = temp_dir("suggestions");
        let path = root.join("vigil").join("routines.json");
        let mut engine = SuggestionEngine::load_at(path.clone()).unwrap();
        let mut config = AppConfig::default();
        config.routines.context = RoutineContextDetail::TimingOnly;
        assert!(engine.record_success(
            "calendar",
            &ActionIntent::OpenCalendar,
            &BarSnapshot::default(),
            &config.routines,
            1_800_000_000,
        ));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::write(&path, b"{not-json").unwrap();
        let recovered = SuggestionEngine::load_at(path).unwrap();
        assert_eq!(recovered.observation_count(), 0);
    }

    #[test]
    fn config_controls_learning_and_legacy_flags_do_not_override_it() {
        for old_enabled in [false, true] {
            let root = temp_dir("legacy-learning");
            fs::create_dir_all(&root).unwrap();
            let path = root.join("routines.json");
            let mut engine = SuggestionEngine::load_at(path.clone()).unwrap();
            let mut config = AppConfig::default();
            config.routines.context = RoutineContextDetail::TimingOnly;
            let snapshot = BarSnapshot::default();
            for day in 0..3 {
                assert!(engine.record_success(
                    "test",
                    &ActionIntent::OpenCalendar,
                    &snapshot,
                    &config.routines,
                    1_800_000_000 + day * 86400
                ));
            }
            let mut json: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            json["learning_enabled"] = old_enabled.into();
            fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
            let mut engine = SuggestionEngine::load_at(path).unwrap();
            let now = 1_800_000_000 + 3 * 86400;
            assert!(
                engine
                    .suggestions(&snapshot, &config, now)
                    .iter()
                    .any(|s| s.source == SuggestionSource::Routine)
            );
            config.routines.enabled = false;
            assert!(!engine.record_success(
                "test",
                &ActionIntent::OpenCalendar,
                &snapshot,
                &config.routines,
                now
            ));
            assert!(
                !engine
                    .suggestions(&snapshot, &config, now)
                    .iter()
                    .any(|s| s.source == SuggestionSource::Routine)
            );
            assert_eq!(engine.observation_count(), 3);
            config.routines.enabled = true;
            let suggestions = engine.suggestions(&snapshot, &config, now);
            let routine = suggestions
                .iter()
                .find(|s| s.source == SuggestionSource::Routine)
                .unwrap();
            engine.dismiss(&routine.id, now);
            assert!(!engine.persisted.feedback.is_empty());
            engine.clear_history().unwrap();
            engine.clear_history().unwrap();
            assert_eq!(engine.observation_count(), 0);
            assert!(engine.persisted.feedback.is_empty());
            assert!(
                !engine
                    .visible
                    .values()
                    .any(|s| s.source == SuggestionSource::Routine)
            );
            let loaded = SuggestionEngine::load_at(root.join("routines.json")).unwrap();
            assert_eq!(loaded.observation_count(), 0);
            assert!(loaded.persisted.feedback.is_empty());
            assert!(engine.record_success(
                "test",
                &ActionIntent::OpenCalendar,
                &snapshot,
                &config.routines,
                now
            ));
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn failed_clear_preserves_history_and_live_suggestions() {
        let root = temp_dir("clear-failure");
        fs::create_dir_all(&root).unwrap();
        let mut engine = SuggestionEngine::default();
        let mut snapshot = BarSnapshot::default();
        snapshot.system.calendar = Some(CalendarEvent {
            title: "Meeting".into(),
            start_epoch: 1_800_000_300,
            ..CalendarEvent::default()
        });
        let config = AppConfig::default();
        engine.record_success(
            "test",
            &ActionIntent::OpenCalendar,
            &snapshot,
            &config.routines,
            1_800_000_000,
        );
        let live = engine.suggestions(&snapshot, &config, 1_800_000_000);
        assert!(!live.is_empty());
        let blocker = root.join("not-a-directory");
        fs::write(&blocker, b"block").unwrap();
        engine.state_path = Some(blocker.join("routines.json"));
        assert!(engine.clear_history().is_err());
        assert_eq!(engine.observation_count(), 1);
        assert_eq!(engine.visible.len(), live.len());
        engine.state_path = Some(root.join("routines.json"));
        engine.clear_history().unwrap();
        assert_eq!(engine.visible.len(), live.len());
        fs::remove_dir_all(root).unwrap();
    }

    fn temp_dir(label: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("vigil-{label}-{unique}"))
    }
}
