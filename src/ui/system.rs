use crate::{
    ActionIntent, AppConfig, AudioState, BarSnapshot, ConnectivityState, ModuleName,
    PlaybackStatus, PowerProfile, SourceHealth, SourceId,
};

use super::control_center::{ControlCenterSpec, build_control_center_spec, keyboard_bar_labels};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SystemModuleId {
    Keyboard,
    Resources,
    AiUsage,
    Network,
    Bluetooth,
    Audio,
    Power,
    Notifications,
    Clock,
}

impl SystemModuleId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keyboard => "keyboard",
            Self::Resources => "resources",
            Self::AiUsage => "ai-usage",
            Self::Network => "network",
            Self::Bluetooth => "bluetooth",
            Self::Audio => "audio",
            Self::Power => "power",
            Self::Notifications => "notifications",
            Self::Clock => "clock",
        }
    }

    fn from_module_name(name: ModuleName) -> Option<Self> {
        match name {
            ModuleName::KeyboardLayout => Some(Self::Keyboard),
            ModuleName::Resources => Some(Self::Resources),
            ModuleName::AiUsage => Some(Self::AiUsage),
            ModuleName::Network => Some(Self::Network),
            ModuleName::Bluetooth => Some(Self::Bluetooth),
            ModuleName::BluetoothAudio => Some(Self::Audio),
            ModuleName::Power => Some(Self::Power),
            ModuleName::Notifications => Some(Self::Notifications),
            ModuleName::Clock => Some(Self::Clock),
            ModuleName::Workspaces
            | ModuleName::FocusedApp
            | ModuleName::FocusedTitle
            | ModuleName::Context
            | ModuleName::CriticalWarning => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemButtonSpec {
    pub id: SystemModuleId,
    pub icon_name: String,
    pub label: Option<String>,
    pub tooltip: String,
    pub classes: Vec<String>,
    /// What a secondary click does, when the module offers a quick toggle.
    pub secondary_action: Option<ActionIntent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemModuleSpec {
    pub button: SystemButtonSpec,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemCluster {
    modules: Vec<SystemModuleSpec>,
    control_center: ControlCenterSpec,
}

impl SystemCluster {
    pub fn modules(&self) -> &[SystemModuleSpec] {
        &self.modules
    }

    pub fn module(&self, id: SystemModuleId) -> &SystemButtonSpec {
        &self
            .modules
            .iter()
            .find(|module| module.button.id == id)
            .expect("system module")
            .button
    }

    pub fn control_center(&self) -> &ControlCenterSpec {
        &self.control_center
    }
}

pub fn build_system_cluster(snapshot: &BarSnapshot, config: &AppConfig) -> SystemCluster {
    let mut seen = std::collections::BTreeSet::new();
    let module_ids = config
        .modules
        .full
        .iter()
        .filter_map(|name| SystemModuleId::from_module_name(*name))
        .filter(|id| seen.insert(*id));

    let modules = module_ids
        .map(|id| SystemModuleSpec {
            button: build_button_spec(id, snapshot, config),
        })
        .collect();

    SystemCluster {
        modules,
        control_center: build_control_center_spec(snapshot),
    }
}

fn build_button_spec(
    id: SystemModuleId,
    snapshot: &BarSnapshot,
    config: &AppConfig,
) -> SystemButtonSpec {
    match id {
        SystemModuleId::Keyboard => keyboard_button(snapshot),
        SystemModuleId::Resources => resources_button(snapshot),
        SystemModuleId::AiUsage => ai_usage_button(snapshot),
        SystemModuleId::Network => network_button(snapshot),
        SystemModuleId::Bluetooth => bluetooth_button(snapshot, config),
        SystemModuleId::Audio => audio_button(snapshot, config),
        SystemModuleId::Power => power_button(snapshot, config),
        SystemModuleId::Notifications => notification_button(snapshot),
        SystemModuleId::Clock => clock_button(snapshot),
    }
}

fn keyboard_button(snapshot: &BarSnapshot) -> SystemButtonSpec {
    let (label, layout) = keyboard_bar_labels(&snapshot.system.keyboard_layout);
    let mut classes = base_classes(SystemModuleId::Keyboard);
    apply_health(
        &mut classes,
        snapshot.system.source_health.get(&SourceId::Compositor),
    );

    SystemButtonSpec {
        id: SystemModuleId::Keyboard,
        icon_name: "input-keyboard-symbolic".to_string(),
        label: Some(label),
        tooltip: with_health_note(
            format!("Keyboard layout: {layout}"),
            snapshot.system.source_health.get(&SourceId::Compositor),
        ),
        classes,
        secondary_action: None,
    }
}

pub(super) fn ai_usage_button(snapshot: &BarSnapshot) -> SystemButtonSpec {
    let usage = &snapshot.system.ai_usage;
    let health = snapshot.system.source_health.get(&SourceId::AiUsage);
    let mut classes = base_classes(SystemModuleId::AiUsage);
    if !usage.codex.available && !usage.claude.available {
        classes.push("inactive".to_string());
    }
    apply_health(&mut classes, health);
    SystemButtonSpec {
        id: SystemModuleId::AiUsage,
        icon_name: "applications-science-symbolic".to_string(),
        label: Some(super::ai_usage::label(usage, health)),
        tooltip: with_health_note(super::ai_usage::detail(usage, health), health),
        classes,
        secondary_action: None,
    }
}

fn resources_button(snapshot: &BarSnapshot) -> SystemButtonSpec {
    let cpu = snapshot.system.resources.cpu_percent;
    let memory = snapshot.system.resources.memory_percent;
    let mut classes = base_classes(SystemModuleId::Resources);
    if let Some(severity) = resource_severity(cpu, memory) {
        classes.push(severity.to_string());
    }
    apply_health(
        &mut classes,
        snapshot.system.source_health.get(&SourceId::Resources),
    );

    let label = match (cpu, memory) {
        (Some(cpu), Some(memory)) => Some(format!("{cpu}/{memory}")),
        (Some(cpu), None) => Some(format!("{cpu}/--")),
        (None, Some(memory)) => Some(format!("--/{memory}")),
        (None, None) => None,
    };

    SystemButtonSpec {
        id: SystemModuleId::Resources,
        icon_name: "utilities-system-monitor-symbolic".to_string(),
        label,
        tooltip: with_health_note(
            format!(
                "CPU {}\nMemory {}",
                percent_or_unavailable(cpu),
                percent_or_unavailable(memory)
            ),
            snapshot.system.source_health.get(&SourceId::Resources),
        ),
        classes,
        secondary_action: None,
    }
}

fn network_button(snapshot: &BarSnapshot) -> SystemButtonSpec {
    let health = snapshot.system.source_health.get(&SourceId::Network);
    let mut classes = base_classes(SystemModuleId::Network);
    apply_health(&mut classes, health);

    SystemButtonSpec {
        id: SystemModuleId::Network,
        icon_name: snapshot
            .system
            .network
            .icon_hint
            .clone()
            .unwrap_or_else(|| "network-idle-symbolic".to_string()),
        label: None,
        tooltip: with_health_note(network_tooltip(snapshot), health),
        classes,
        secondary_action: None,
    }
}

fn bluetooth_button(snapshot: &BarSnapshot, config: &AppConfig) -> SystemButtonSpec {
    let bluetooth = &snapshot.system.bluetooth;
    let health = snapshot.system.source_health.get(&SourceId::Bluetooth);
    let mut classes = base_classes(SystemModuleId::Bluetooth);
    let connected = bluetooth
        .devices
        .iter()
        .filter(|device| device.connected)
        .collect::<Vec<_>>();
    let mut icon_name = bluetooth_icon(bluetooth).to_string();

    let label = if !bluetooth.available || !bluetooth.powered {
        classes.push("inactive".to_string());
        None
    } else if bluetooth.pairing_prompt.is_some() {
        classes.push("needs-attention".to_string());
        Some("Pair".to_string())
    } else if let Some(operation) = bluetooth.devices.iter().find_map(|device| device.operation) {
        classes.push("busy".to_string());
        Some(bluetooth_operation_label(operation).to_string())
    } else {
        match connected.as_slice() {
            [] => None,
            [device] => {
                classes.push("connected".to_string());
                if !device.icon_name.is_empty() && device.icon_name != "bluetooth-symbolic" {
                    icon_name = device.icon_name.clone();
                }
                Some(bluetooth_device_label(device))
            }
            devices => {
                classes.push("connected".to_string());
                Some(devices.len().to_string())
            }
        }
    };
    if connected.iter().any(|device| {
        device
            .battery_percent
            .is_some_and(|percent| percent <= config.thresholds.battery_low_percent)
    }) {
        classes.push("warning".to_string());
    }
    apply_health(&mut classes, health);

    SystemButtonSpec {
        id: SystemModuleId::Bluetooth,
        icon_name,
        label,
        tooltip: with_health_note(bluetooth_tooltip(bluetooth), health),
        classes,
        secondary_action: bluetooth
            .available
            .then_some(ActionIntent::SetBluetoothPowered {
                powered: !bluetooth.powered,
            }),
    }
}

const BLUETOOTH_LABEL_CHARS: usize = 18;

fn bluetooth_device_label(device: &crate::BluetoothDeviceState) -> String {
    let name = if device.name.chars().count() > BLUETOOTH_LABEL_CHARS {
        let kept = device
            .name
            .chars()
            .take(BLUETOOTH_LABEL_CHARS - 1)
            .collect::<String>();
        format!("{}…", kept.trim_end())
    } else {
        device.name.clone()
    };
    match device.battery_percent {
        Some(percent) => format!("{name} · {percent}%"),
        None => name,
    }
}

fn bluetooth_operation_label(operation: crate::BluetoothDeviceOperation) -> &'static str {
    match operation {
        crate::BluetoothDeviceOperation::Connecting => "Connecting…",
        crate::BluetoothDeviceOperation::Disconnecting => "Disconnecting…",
        crate::BluetoothDeviceOperation::Pairing => "Pairing…",
        crate::BluetoothDeviceOperation::Forgetting => "Forgetting…",
    }
}

fn bluetooth_icon(bluetooth: &crate::BluetoothState) -> &'static str {
    if !bluetooth.available || !bluetooth.powered {
        "bluetooth-disabled-symbolic"
    } else if bluetooth.devices.iter().any(|device| device.connected) {
        "bluetooth-active-symbolic"
    } else {
        "bluetooth-symbolic"
    }
}

fn bluetooth_tooltip(bluetooth: &crate::BluetoothState) -> String {
    if !bluetooth.available {
        return "Bluetooth unavailable".to_string();
    }
    if !bluetooth.powered {
        return "Bluetooth off\nRight-click to turn on".to_string();
    }
    let mut lines = Vec::new();
    if let Some(prompt) = bluetooth.pairing_prompt.as_ref() {
        lines.push(format!("Pairing request from {}", prompt.device_name));
    }
    let connected = bluetooth
        .devices
        .iter()
        .filter(|device| device.connected)
        .collect::<Vec<_>>();
    if connected.is_empty() {
        lines.push("Bluetooth on · No devices connected".to_string());
    }
    for device in connected {
        lines.push(match device.battery_percent {
            Some(percent) => format!("{} · {percent}% battery", device.name),
            None => device.name.clone(),
        });
    }
    lines.push("Right-click to turn off".to_string());
    lines.join("\n")
}

/// Without its own bar item, Bluetooth state rides on the audio button.
fn audio_button(snapshot: &BarSnapshot, config: &AppConfig) -> SystemButtonSpec {
    let mut classes = base_classes(SystemModuleId::Audio);
    let carries_bluetooth = snapshot.system.bluetooth.available
        && !config.modules.full.contains(&ModuleName::Bluetooth);
    let bluetooth_off = carries_bluetooth && !snapshot.system.bluetooth.powered;
    if bluetooth_off {
        classes.push("inactive".to_string());
    }
    let health = if carries_bluetooth {
        module_health(snapshot, &[SourceId::Audio, SourceId::Bluetooth])
    } else {
        snapshot.system.source_health.get(&SourceId::Audio)
    };
    apply_health(&mut classes, health);

    SystemButtonSpec {
        id: SystemModuleId::Audio,
        icon_name: audio_icon(&snapshot.system.audio, bluetooth_off).to_string(),
        label: None,
        tooltip: with_health_note(audio_tooltip(snapshot, carries_bluetooth), health),
        classes,
        secondary_action: None,
    }
}

fn power_button(snapshot: &BarSnapshot, config: &AppConfig) -> SystemButtonSpec {
    let health = snapshot.system.source_health.get(&SourceId::Power);
    let mut classes = base_classes(SystemModuleId::Power);
    if snapshot.system.power.battery_present {
        if snapshot.system.power.charging {
            classes.push("charging".to_string());
        } else if let Some(severity) = battery_severity_class(snapshot, config) {
            classes.push(severity.to_string());
        }
    }
    apply_health(&mut classes, health);

    let percent = snapshot.system.power.battery_percent;
    let (icon_name, label) = if snapshot.system.power.battery_present {
        (
            power_icon(percent, snapshot.system.power.charging),
            percent.map(|value| format!("{value}%")),
        )
    } else {
        (power_profile_icon(&snapshot.system.power.profile), None)
    };
    SystemButtonSpec {
        id: SystemModuleId::Power,
        icon_name: icon_name.to_string(),
        label,
        tooltip: with_health_note(power_tooltip(snapshot), health),
        classes,
        secondary_action: None,
    }
}

fn clock_button(snapshot: &BarSnapshot) -> SystemButtonSpec {
    let health = module_health(
        snapshot,
        &[SourceId::Clock, SourceId::Calendar, SourceId::Timers],
    );
    let mut classes = base_classes(SystemModuleId::Clock);
    apply_health(&mut classes, health);

    SystemButtonSpec {
        id: SystemModuleId::Clock,
        icon_name: "preferences-system-time-symbolic".to_string(),
        label: Some(snapshot.system.clock.label.clone()),
        tooltip: with_health_note(format!("Clock: {}", snapshot.system.clock.label), health),
        classes,
        secondary_action: None,
    }
}

fn notification_button(snapshot: &BarSnapshot) -> SystemButtonSpec {
    let state = &snapshot.system.notifications;
    let mut classes = base_classes(SystemModuleId::Notifications);
    if state.unread_count > 0 {
        classes.push("has-unread".to_string());
    }
    if state.dnd {
        classes.push("is-dnd".to_string());
    }
    SystemButtonSpec {
        id: SystemModuleId::Notifications,
        icon_name: if state.dnd {
            "notifications-disabled-symbolic".to_string()
        } else if state.unread_count > 0 {
            "preferences-system-notifications-symbolic".to_string()
        } else {
            "notifications-symbolic".to_string()
        },
        label: (state.unread_count > 0).then(|| state.unread_count.to_string()),
        tooltip: if state.dnd {
            format!("Do Not Disturb · {} unread", state.unread_count)
        } else {
            format!("Notifications · {} unread", state.unread_count)
        },
        classes,
        secondary_action: None,
    }
}

fn resource_severity(cpu: Option<u8>, memory: Option<u8>) -> Option<&'static str> {
    let pressure = cpu.into_iter().chain(memory).max()?;
    if pressure >= 90 {
        Some("critical")
    } else if pressure >= 75 {
        Some("warning")
    } else {
        None
    }
}

fn audio_icon(audio: &AudioState, bluetooth_off: bool) -> &'static str {
    if bluetooth_off {
        "bluetooth-disabled-symbolic"
    } else if audio.muted {
        "audio-volume-muted-symbolic"
    } else {
        match audio.volume_percent.unwrap_or_default() {
            0 => "audio-volume-muted-symbolic",
            1..=33 => "audio-volume-low-symbolic",
            34..=66 => "audio-volume-medium-symbolic",
            _ => "audio-volume-high-symbolic",
        }
    }
}

fn power_icon(percent: Option<u8>, charging: bool) -> &'static str {
    match (percent.unwrap_or(100), charging) {
        (_, true) => "battery-level-100-charging-symbolic",
        (0..=10, false) => "battery-level-10-symbolic",
        (11..=30, false) => "battery-level-30-symbolic",
        (31..=60, false) => "battery-level-50-symbolic",
        (61..=80, false) => "battery-level-80-symbolic",
        _ => "battery-level-100-symbolic",
    }
}

fn network_tooltip(snapshot: &BarSnapshot) -> String {
    match snapshot.system.network.connectivity {
        ConnectivityState::Connected => snapshot
            .system
            .network
            .label
            .as_ref()
            .map(|label| format!("Network: {label}"))
            .unwrap_or_else(|| "Network connected".to_string()),
        ConnectivityState::Connecting => "Network connecting".to_string(),
        ConnectivityState::Disconnected => "Network disconnected".to_string(),
        ConnectivityState::Unknown => "Network status unavailable".to_string(),
    }
}

fn audio_tooltip(snapshot: &BarSnapshot, carries_bluetooth: bool) -> String {
    let mut parts = vec![format!(
        "Volume {}",
        percent_or_unavailable(snapshot.system.audio.volume_percent)
    )];

    if snapshot.system.audio.muted {
        parts.push("Muted".to_string());
    }

    if carries_bluetooth {
        parts.push(if snapshot.system.bluetooth.powered {
            snapshot
                .system
                .bluetooth
                .connected_device
                .as_ref()
                .map(|device| format!("Bluetooth: {device}"))
                .unwrap_or_else(|| "Bluetooth on".to_string())
        } else {
            "Bluetooth off".to_string()
        });
    }

    if let Some(media) = snapshot.system.media.as_ref() {
        parts.push(media_summary(media));
    }

    parts.join("\n")
}

fn power_tooltip(snapshot: &BarSnapshot) -> String {
    if !snapshot.system.power.battery_present {
        return format!(
            "Power profile: {}",
            power_profile_label(snapshot.system.power.profile.clone())
        );
    }
    let battery = match snapshot.system.power.battery_percent {
        Some(percent) if snapshot.system.power.charging => format!("Battery {percent}% (charging)"),
        Some(percent) => format!("Battery {percent}%"),
        None => "Battery unavailable".to_string(),
    };
    format!(
        "{battery}\nProfile: {}",
        power_profile_label(snapshot.system.power.profile.clone())
    )
}

fn media_summary(media: &crate::MediaState) -> String {
    let status = match media.status {
        PlaybackStatus::Playing => "Playing",
        PlaybackStatus::Paused => "Paused",
        PlaybackStatus::Stopped => "Stopped",
    };
    match (&media.title, &media.artist) {
        (Some(title), Some(artist)) => format!("{status}: {title} - {artist}"),
        (Some(title), None) => format!("{status}: {title}"),
        (None, Some(artist)) => format!("{status}: {artist}"),
        (None, None) => format!("{status}: {}", media.player),
    }
}

fn power_profile_label(profile: PowerProfile) -> &'static str {
    match profile {
        PowerProfile::Performance => "Performance",
        PowerProfile::Balanced => "Balanced",
        PowerProfile::PowerSaver => "Power Saver",
    }
}

fn power_profile_icon(profile: &PowerProfile) -> &'static str {
    match profile {
        PowerProfile::Performance => "power-profile-performance-symbolic",
        PowerProfile::Balanced => "power-profile-balanced-symbolic",
        PowerProfile::PowerSaver => "power-profile-power-saver-symbolic",
    }
}

fn battery_severity_class(snapshot: &BarSnapshot, config: &AppConfig) -> Option<&'static str> {
    let percent = snapshot.system.power.battery_percent?;
    if percent <= config.thresholds.battery_critical_percent {
        Some("critical")
    } else if percent <= config.thresholds.battery_low_percent {
        Some("warning")
    } else {
        None
    }
}

fn module_health<'a>(snapshot: &'a BarSnapshot, ids: &[SourceId]) -> Option<&'a SourceHealth> {
    ids.iter()
        .filter_map(|id| snapshot.system.source_health.get(id))
        .find(|health| matches!(health, SourceHealth::Disconnected { .. }))
        .or_else(|| {
            ids.iter()
                .filter_map(|id| snapshot.system.source_health.get(id))
                .find(|health| matches!(health, SourceHealth::Stale { .. }))
        })
}

fn apply_health(classes: &mut Vec<String>, health: Option<&SourceHealth>) {
    match health {
        Some(SourceHealth::Stale { .. }) => classes.push("stale".to_string()),
        Some(SourceHealth::Disconnected { .. }) => classes.push("disconnected".to_string()),
        Some(SourceHealth::Healthy) | None => {}
    }
}

fn with_health_note(base: String, health: Option<&SourceHealth>) -> String {
    match health {
        Some(SourceHealth::Stale { .. }) => format!("{base}\nStale"),
        Some(SourceHealth::Disconnected { message }) => format!("{base}\nDependency: {message}"),
        Some(SourceHealth::Healthy) | None => base,
    }
}

fn base_classes(id: SystemModuleId) -> Vec<String> {
    vec!["system-module".to_string(), id.as_str().to_string()]
}

fn percent_or_unavailable(value: Option<u8>) -> String {
    value
        .map(|percent| format!("{percent}%"))
        .unwrap_or_else(|| "unavailable".to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        ActionIntent, AppConfig, AudioState, BarSnapshot, BluetoothDeviceOperation,
        BluetoothDeviceState, BluetoothPairingPrompt, BluetoothPairingPromptKind, BluetoothState,
        ClockState, ConnectivityState, KeyboardLayoutOption, KeyboardLayoutState, ModuleName,
        NetworkState, PowerProfile, PowerState, ResourceState, SourceHealth, SourceId,
    };

    use super::{SystemModuleId, build_system_cluster};

    #[test]
    fn build_system_cluster_follows_configured_module_order() {
        let snapshot = snapshot();
        let mut config = AppConfig::default();
        config.modules.full = vec![ModuleName::Clock, ModuleName::Power, ModuleName::Network];

        let cluster = build_system_cluster(&snapshot, &config);

        assert_eq!(
            cluster
                .modules()
                .iter()
                .map(|module| module.button.id)
                .collect::<Vec<_>>(),
            vec![
                SystemModuleId::Clock,
                SystemModuleId::Power,
                SystemModuleId::Network,
            ]
        );
    }

    #[test]
    fn build_system_cluster_omits_modules_missing_from_config() {
        let snapshot = snapshot();
        let mut config = AppConfig::default();
        config.modules.full = vec![ModuleName::Clock, ModuleName::Network];

        let cluster = build_system_cluster(&snapshot, &config);

        assert!(
            !cluster
                .modules()
                .iter()
                .any(|module| module.button.id == SystemModuleId::Resources)
        );
    }

    #[test]
    fn build_system_cluster_deduplicates_repeated_module_names() {
        let snapshot = snapshot();
        let mut config = AppConfig::default();
        config.modules.full = vec![ModuleName::Clock, ModuleName::Clock, ModuleName::Network];

        let cluster = build_system_cluster(&snapshot, &config);

        assert_eq!(
            cluster
                .modules()
                .iter()
                .filter(|module| module.button.id == SystemModuleId::Clock)
                .count(),
            1
        );
    }

    #[test]
    fn build_system_cluster_ignores_non_system_module_names() {
        let snapshot = snapshot();
        let mut config = AppConfig::default();
        config.modules.full = vec![
            ModuleName::Workspaces,
            ModuleName::FocusedApp,
            ModuleName::FocusedTitle,
            ModuleName::Context,
            ModuleName::CriticalWarning,
            ModuleName::Clock,
        ];

        let cluster = build_system_cluster(&snapshot, &config);

        assert_eq!(
            cluster
                .modules()
                .iter()
                .map(|module| module.button.id)
                .collect::<Vec<_>>(),
            vec![SystemModuleId::Clock]
        );
    }

    #[test]
    fn keyboard_button_compacts_us_and_dvorak_layout_labels() {
        let mut snapshot = snapshot();
        snapshot.system.keyboard_layout = KeyboardLayoutState {
            current_index: Some(0),
            current_name: Some("English (US)".to_string()),
            layouts: vec![KeyboardLayoutOption {
                index: 0,
                name: "English (US)".to_string(),
                layout: Some("us".to_string()),
                variant: None,
            }],
        };

        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let keyboard = cluster.module(SystemModuleId::Keyboard);
        assert_eq!(keyboard.label.as_deref(), Some("US"));
        assert!(keyboard.tooltip.contains("English (US)"));

        snapshot.system.keyboard_layout = KeyboardLayoutState {
            current_index: Some(1),
            current_name: Some("English (Dvorak)".to_string()),
            layouts: vec![KeyboardLayoutOption {
                index: 1,
                name: "English (Dvorak)".to_string(),
                layout: Some("us".to_string()),
                variant: Some("dvorak".to_string()),
            }],
        };
        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let keyboard = cluster.module(SystemModuleId::Keyboard);
        assert_eq!(keyboard.label.as_deref(), Some("US-DV"));
        assert!(keyboard.tooltip.contains("English (Dvorak)"));

        snapshot.system.keyboard_layout = KeyboardLayoutState {
            current_index: Some(2),
            current_name: Some("German (KOY)".to_string()),
            layouts: vec![KeyboardLayoutOption {
                index: 2,
                name: "German (KOY)".to_string(),
                layout: Some("de".to_string()),
                variant: Some("koy".to_string()),
            }],
        };
        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let keyboard = cluster.module(SystemModuleId::Keyboard);
        assert_eq!(keyboard.label.as_deref(), Some("DE-KOY"));
        assert!(keyboard.tooltip.contains("German (KOY)"));
    }

    #[test]
    fn resources_button_marks_cpu_and_memory_pressure() {
        let mut snapshot = snapshot();
        snapshot.system.resources = ResourceState {
            cpu_percent: Some(91),
            memory_percent: Some(76),
        };

        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let resources = cluster.module(SystemModuleId::Resources);

        assert_eq!(resources.label.as_deref(), Some("91/76"));
        assert!(resources.classes.iter().any(|class| class == "critical"));
        assert!(resources.tooltip.contains("CPU 91%"));
        assert!(resources.tooltip.contains("Memory 76%"));
    }

    #[test]
    fn network_button_distinguishes_connected_and_disconnected_states() {
        let mut snapshot = snapshot();
        snapshot.system.network = NetworkState {
            connectivity: ConnectivityState::Connected,
            icon_hint: Some("network-wireless-signal-good-symbolic".to_string()),
            label: Some("Office Wi-Fi".to_string()),
            wifi_available: true,
            ethernet_available: true,
            wifi_enabled: Some(true),
            ..NetworkState::default()
        };

        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let network = cluster.module(SystemModuleId::Network);
        assert!(network.tooltip.contains("Office Wi-Fi"));
        assert!(!network.classes.iter().any(|class| class == "disconnected"));

        snapshot.system.source_health.insert(
            SourceId::Network,
            SourceHealth::Disconnected {
                message: "NetworkManager unavailable".to_string(),
            },
        );
        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let network = cluster.module(SystemModuleId::Network);
        assert!(network.classes.iter().any(|class| class == "disconnected"));
        assert!(network.tooltip.contains("NetworkManager unavailable"));
    }

    #[test]
    fn bluetooth_button_shows_powered_off_state_without_source_failure() {
        let mut snapshot = snapshot();
        snapshot.system.bluetooth = BluetoothState {
            available: true,
            powered: false,
            connected_device: None,
            audio_device: None,
            ..BluetoothState::default()
        };
        snapshot.system.audio = AudioState {
            volume_percent: Some(42),
            muted: false,
            outputs: Vec::new(),
        };
        let mut config = AppConfig::default();
        config.modules.full = vec![ModuleName::BluetoothAudio];

        let cluster = build_system_cluster(&snapshot, &config);
        let audio = cluster.module(SystemModuleId::Audio);

        assert!(audio.classes.iter().any(|class| class == "inactive"));
        assert!(!audio.classes.iter().any(|class| class == "disconnected"));
        assert_eq!(audio.icon_name, "bluetooth-disabled-symbolic");
        assert!(audio.tooltip.contains("Bluetooth off"));
        assert!(audio.tooltip.contains("42%"));
    }

    #[test]
    fn audio_button_leaves_bluetooth_to_its_own_item_when_both_are_shown() {
        let mut snapshot = snapshot();
        snapshot.system.bluetooth = BluetoothState {
            available: true,
            powered: false,
            ..BluetoothState::default()
        };
        snapshot.system.audio = AudioState {
            volume_percent: Some(42),
            muted: false,
            outputs: Vec::new(),
        };
        let mut config = AppConfig::default();
        config.modules.full = vec![ModuleName::Bluetooth, ModuleName::BluetoothAudio];

        let cluster = build_system_cluster(&snapshot, &config);
        let audio = cluster.module(SystemModuleId::Audio);

        assert!(!audio.classes.iter().any(|class| class == "inactive"));
        assert_eq!(audio.icon_name, "audio-volume-medium-symbolic");
        assert!(!audio.tooltip.contains("Bluetooth"));
    }

    #[test]
    fn bluetooth_status_button_shows_disabled_icon_when_powered_off() {
        let mut snapshot = snapshot();
        snapshot.system.bluetooth = BluetoothState {
            available: true,
            powered: false,
            ..BluetoothState::default()
        };

        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let bluetooth = cluster.module(SystemModuleId::Bluetooth);

        assert_eq!(bluetooth.icon_name, "bluetooth-disabled-symbolic");
        assert!(bluetooth.classes.iter().any(|class| class == "inactive"));
        assert!(bluetooth.tooltip.contains("off"));
    }

    #[test]
    fn bluetooth_status_button_shows_idle_icon_when_powered_with_no_devices() {
        let mut snapshot = snapshot();
        snapshot.system.bluetooth = BluetoothState {
            available: true,
            powered: true,
            ..BluetoothState::default()
        };

        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let bluetooth = cluster.module(SystemModuleId::Bluetooth);

        assert_eq!(bluetooth.icon_name, "bluetooth-symbolic");
        assert!(!bluetooth.classes.iter().any(|class| class == "inactive"));
    }

    #[test]
    fn bluetooth_status_button_shows_active_icon_and_device_name_when_connected() {
        let mut snapshot = snapshot();
        snapshot.system.bluetooth = BluetoothState {
            available: true,
            powered: true,
            connected_device: Some("Sony Headphones".to_string()),
            devices: vec![BluetoothDeviceState {
                name: "Sony Headphones".to_string(),
                icon_name: "bluetooth-symbolic".to_string(),
                connected: true,
                ..BluetoothDeviceState::default()
            }],
            ..BluetoothState::default()
        };

        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let bluetooth = cluster.module(SystemModuleId::Bluetooth);

        assert_eq!(bluetooth.icon_name, "bluetooth-active-symbolic");
        assert_eq!(bluetooth.label.as_deref(), Some("Sony Headphones"));
        assert!(bluetooth.classes.iter().any(|class| class == "connected"));
        assert!(bluetooth.tooltip.contains("Sony Headphones"));
    }

    #[test]
    fn bluetooth_status_button_names_one_device_with_its_icon_and_battery() {
        let mut snapshot = snapshot();
        snapshot.system.bluetooth = BluetoothState {
            available: true,
            powered: true,
            devices: vec![BluetoothDeviceState {
                name: "WH-1000XM5 Wireless Headphones".to_string(),
                icon_name: "audio-headphones-symbolic".to_string(),
                connected: true,
                battery_percent: Some(12),
                ..BluetoothDeviceState::default()
            }],
            ..BluetoothState::default()
        };

        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let bluetooth = cluster.module(SystemModuleId::Bluetooth);

        assert_eq!(bluetooth.icon_name, "audio-headphones-symbolic");
        assert_eq!(bluetooth.label.as_deref(), Some("WH-1000XM5 Wirele… · 12%"));
        assert!(bluetooth.classes.iter().any(|class| class == "warning"));
        assert!(
            bluetooth
                .tooltip
                .contains("WH-1000XM5 Wireless Headphones · 12% battery")
        );
    }

    #[test]
    fn bluetooth_status_button_counts_several_connected_devices() {
        let mut snapshot = snapshot();
        let device = BluetoothDeviceState {
            connected: true,
            ..BluetoothDeviceState::default()
        };
        snapshot.system.bluetooth = BluetoothState {
            available: true,
            powered: true,
            devices: vec![device.clone(), device],
            ..BluetoothState::default()
        };

        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let bluetooth = cluster.module(SystemModuleId::Bluetooth);

        assert_eq!(bluetooth.icon_name, "bluetooth-active-symbolic");
        assert_eq!(bluetooth.label.as_deref(), Some("2"));
    }

    #[test]
    fn bluetooth_status_button_flags_pairing_requests_and_busy_devices() {
        let mut snapshot = snapshot();
        snapshot.system.bluetooth = BluetoothState {
            available: true,
            powered: true,
            devices: vec![BluetoothDeviceState {
                operation: Some(BluetoothDeviceOperation::Pairing),
                ..BluetoothDeviceState::default()
            }],
            ..BluetoothState::default()
        };
        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let busy = cluster.module(SystemModuleId::Bluetooth);
        assert_eq!(busy.label.as_deref(), Some("Pairing…"));
        assert!(busy.classes.iter().any(|class| class == "busy"));

        snapshot.system.bluetooth.pairing_prompt = Some(BluetoothPairingPrompt {
            id: 1,
            address: "AA:BB:CC:DD:EE:FF".to_string(),
            device_name: "Keyboard".to_string(),
            kind: BluetoothPairingPromptKind::ConfirmPasskey { passkey: 123_456 },
        });
        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let prompt = cluster.module(SystemModuleId::Bluetooth);
        assert_eq!(prompt.label.as_deref(), Some("Pair"));
        assert!(
            prompt
                .classes
                .iter()
                .any(|class| class == "needs-attention")
        );
        assert!(prompt.tooltip.contains("Pairing request from Keyboard"));
    }

    #[test]
    fn bluetooth_status_button_secondary_click_toggles_power() {
        let mut snapshot = snapshot();
        snapshot.system.bluetooth = BluetoothState {
            available: true,
            powered: true,
            ..BluetoothState::default()
        };
        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        assert_eq!(
            cluster.module(SystemModuleId::Bluetooth).secondary_action,
            Some(ActionIntent::SetBluetoothPowered { powered: false })
        );

        snapshot.system.bluetooth.powered = false;
        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        assert_eq!(
            cluster.module(SystemModuleId::Bluetooth).secondary_action,
            Some(ActionIntent::SetBluetoothPowered { powered: true })
        );

        snapshot.system.bluetooth.available = false;
        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        assert_eq!(
            cluster.module(SystemModuleId::Bluetooth).secondary_action,
            None
        );
    }

    #[test]
    fn build_system_cluster_includes_bluetooth_by_default() {
        let cluster = build_system_cluster(&snapshot(), &AppConfig::default());
        assert!(
            cluster
                .modules()
                .iter()
                .any(|module| module.button.id == SystemModuleId::Bluetooth)
        );
    }

    #[test]
    fn audio_button_ignores_bluetooth_when_no_adapter_exists() {
        let mut snapshot = snapshot();
        snapshot.system.audio = AudioState {
            volume_percent: Some(42),
            muted: false,
            outputs: Vec::new(),
        };

        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let audio = cluster.module(SystemModuleId::Audio);

        assert!(!audio.classes.iter().any(|class| class == "inactive"));
        assert!(!audio.tooltip.contains("Bluetooth"));
    }

    #[test]
    fn power_button_marks_charging_low_and_critical_battery_states() {
        let mut snapshot = snapshot();
        snapshot.system.power = PowerState {
            battery_present: true,
            battery_percent: Some(88),
            charging: true,
            profile: PowerProfile::Balanced,
            changed_at: 0,
        };

        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let power = cluster.module(SystemModuleId::Power);
        assert_eq!(power.label.as_deref(), Some("88%"));
        assert!(power.classes.iter().any(|class| class == "charging"));

        snapshot.system.power.battery_percent = Some(15);
        snapshot.system.power.charging = false;
        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let power = cluster.module(SystemModuleId::Power);
        assert!(power.classes.iter().any(|class| class == "warning"));

        snapshot.system.power.battery_percent = Some(7);
        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let power = cluster.module(SystemModuleId::Power);
        assert!(power.classes.iter().any(|class| class == "critical"));
    }

    #[test]
    fn desktop_power_button_uses_profile_instead_of_fake_battery() {
        let mut snapshot = snapshot();
        snapshot.system.power.profile = PowerProfile::Performance;

        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let power = cluster.module(SystemModuleId::Power);

        assert_eq!(power.icon_name, "power-profile-performance-symbolic");
        assert_eq!(power.label, None);
        assert_eq!(power.tooltip, "Power profile: Performance");
        assert!(!power.classes.iter().any(|class| class == "charging"));
    }

    #[test]
    fn stale_health_adds_stale_class_to_the_affected_module() {
        let mut snapshot = snapshot();
        snapshot.system.source_health.insert(
            SourceId::Resources,
            SourceHealth::Stale { since_epoch: 123 },
        );

        let cluster = build_system_cluster(&snapshot, &AppConfig::default());
        let resources = cluster.module(SystemModuleId::Resources);

        assert!(resources.classes.iter().any(|class| class == "stale"));
        assert!(resources.tooltip.contains("Stale"));
    }

    fn snapshot() -> BarSnapshot {
        BarSnapshot {
            outputs: BTreeMap::new(),
            focused_output: None,
            system: crate::SystemState {
                clock: ClockState {
                    epoch_seconds: 1_800_000_000,
                    label: "12:00".to_string(),
                },
                network: NetworkState {
                    connectivity: ConnectivityState::Disconnected,
                    icon_hint: Some("network-offline-symbolic".to_string()),
                    label: None,
                    wifi_available: true,
                    ethernet_available: true,
                    wifi_enabled: Some(false),
                    ..NetworkState::default()
                },
                ..crate::SystemState::default()
            },
            ..BarSnapshot::default()
        }
    }
}
