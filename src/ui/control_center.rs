mod audio;
mod connectivity;
mod power;
mod storage;
mod time;

use audio::*;
use connectivity::*;
use power::*;
use time::*;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use tracing::warn;

use crate::{
    ActionCompletion, ActionIntent, ActionRequest, ActionResult, AudioOutputState, BarSnapshot,
    BluetoothDeviceOperation, BluetoothDeviceState, BluetoothPairingPrompt,
    BluetoothPairingPromptKind, BluetoothPairingResponse, CalendarAgenda, CalendarAgendaEvent,
    CalendarMonthRequest, ConnectivityState, DesktopContext, Direction, KeyboardLayoutOption,
    KeyboardLayoutState, MediaControlAction, Notification, NotificationControl, NotificationState,
    NotificationUrgency, PlaybackStatus, PowerProfile, SourceHealth, SourceId, Suggestion,
    SuggestionAction, SuggestionEngine,
};

use super::ai_usage::AiUsageView;
use super::artwork::{
    ArtworkRequest, prefer_artwork_candidate, result_is_current, spawn_artwork_loader,
};
use super::morph::MorphRect;
use super::morph_bin::{MorphBin, MorphOrigin, MorphShadow};
use super::stage;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ControlCenterFocus {
    Overview,
    Keyboard,
    Resources,
    Storage,
    AiUsage,
    Network,
    Bluetooth,
    Audio,
    Power,
    Clock,
    Notifications,
}

impl ControlCenterFocus {
    pub const ALL: [Self; 11] = [
        Self::Overview,
        Self::Keyboard,
        Self::Resources,
        Self::Storage,
        Self::AiUsage,
        Self::Network,
        Self::Bluetooth,
        Self::Audio,
        Self::Power,
        Self::Clock,
        Self::Notifications,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Keyboard => "keyboard",
            Self::Resources => "resources",
            Self::Storage => "storage",
            Self::AiUsage => "ai-usage",
            Self::Network => "network",
            Self::Bluetooth => "bluetooth",
            Self::Audio => "audio",
            Self::Power => "power",
            Self::Clock => "clock",
            Self::Notifications => "notifications",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Overview => "Quick Settings",
            Self::Keyboard => "Keyboard",
            Self::Resources => "System Resources",
            Self::Storage => "Storage",
            Self::AiUsage => "AI Usage",
            Self::Network => "Network",
            Self::Bluetooth => "Bluetooth",
            Self::Audio => "Audio",
            Self::Power => "Power",
            Self::Clock => "Time & Focus",
            Self::Notifications => "Notifications",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Self::Overview => "System overview",
            Self::Keyboard => "Input layout",
            Self::Resources => "CPU and memory",
            Self::Storage => "Local filesystem usage",
            Self::AiUsage => "Codex & Claude Code",
            Self::Network => "Connectivity",
            Self::Bluetooth => "Devices and discovery",
            Self::Audio => "Volume and playback",
            Self::Power => "Performance and energy",
            Self::Clock => "Calendar and timers",
            Self::Notifications => "Recent alerts and actions",
        }
    }

    pub fn luma_query(self) -> &'static str {
        match self {
            Self::Overview => "system",
            Self::Keyboard => "keyboard",
            Self::Resources => "system",
            Self::Storage => "system",
            Self::AiUsage => "system",
            Self::Network => "network",
            Self::Bluetooth => "bluetooth",
            Self::Audio => "audio",
            Self::Power => "power",
            Self::Clock => "calendar",
            Self::Notifications => "notifications",
        }
    }
}

impl From<DesktopContext> for ControlCenterFocus {
    fn from(context: DesktopContext) -> Self {
        match context {
            DesktopContext::Overview => Self::Overview,
            DesktopContext::Keyboard => Self::Keyboard,
            DesktopContext::Resources => Self::Resources,
            DesktopContext::Storage => Self::Storage,
            DesktopContext::Network => Self::Network,
            DesktopContext::Bluetooth => Self::Bluetooth,
            DesktopContext::Audio => Self::Audio,
            DesktopContext::Power => Self::Power,
            DesktopContext::Clock => Self::Clock,
            DesktopContext::Notifications => Self::Notifications,
        }
    }
}

impl From<ControlCenterFocus> for DesktopContext {
    fn from(focus: ControlCenterFocus) -> Self {
        match focus {
            ControlCenterFocus::Overview => Self::Overview,
            ControlCenterFocus::Keyboard => Self::Keyboard,
            ControlCenterFocus::Resources => Self::Resources,
            ControlCenterFocus::Storage => Self::Storage,
            // AI usage is an internal detail page; Luma's system context is unchanged.
            ControlCenterFocus::AiUsage => Self::Resources,
            ControlCenterFocus::Network => Self::Network,
            ControlCenterFocus::Bluetooth => Self::Bluetooth,
            ControlCenterFocus::Audio => Self::Audio,
            ControlCenterFocus::Power => Self::Power,
            ControlCenterFocus::Clock => Self::Clock,
            ControlCenterFocus::Notifications => Self::Notifications,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickControlSpec {
    pub icon_name: String,
    pub label: String,
    pub detail: String,
    pub enabled: bool,
    pub available: bool,
    pub toggle_available: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkTrafficSpec {
    pub interface: Option<String>,
    pub download_bytes_per_second: Option<u64>,
    pub upload_bytes_per_second: Option<u64>,
    pub download_history: Vec<u64>,
    pub upload_history: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioControlSpec {
    pub percent: Option<u8>,
    pub muted: bool,
    pub detail: String,
    pub outputs: Vec<AudioOutputControlSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioOutputControlSpec {
    pub name: String,
    pub label: String,
    pub detail: String,
    pub icon_name: String,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BluetoothDeviceControlSpec {
    pub address: String,
    pub name: String,
    pub icon_name: String,
    pub detail: String,
    pub paired: bool,
    pub connected: bool,
    pub operation: Option<BluetoothDeviceOperation>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BluetoothManagerSpec {
    pub available: bool,
    pub powered: bool,
    pub discovering: bool,
    pub connected_count: usize,
    pub devices: Vec<BluetoothDeviceControlSpec>,
    pub prompt: Option<BluetoothPairingPrompt>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerProfileOptionSpec {
    pub profile: PowerProfile,
    pub available: bool,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerStatSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub value: String,
    pub caption: Option<String>,
    pub level: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerPageSpec {
    pub hero_icon: &'static str,
    pub hero_eyebrow: &'static str,
    pub hero_title: String,
    pub hero_detail: String,
    pub profiles: Vec<PowerProfileOptionSpec>,
    pub profile_note: String,
    pub performance_degraded: bool,
    pub stats: Vec<PowerStatSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyboardLayoutControlSpec {
    pub index: u8,
    pub title: String,
    pub detail: String,
    pub raw_name: String,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyboardControlSpec {
    pub summary: String,
    pub current: String,
    pub detail: String,
    pub layouts: Vec<KeyboardLayoutControlSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrightnessControlSpec {
    pub device: String,
    pub percent: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaControlSpec {
    pub player: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub art_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimerControlSpec {
    pub id: String,
    pub label: String,
    pub remaining_seconds: u64,
    pub completed: bool,
    pub paused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalendarControlSpec {
    pub title: String,
    pub location: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlCenterSpec {
    pub network: QuickControlSpec,
    pub network_traffic: NetworkTrafficSpec,
    pub bluetooth: QuickControlSpec,
    pub bluetooth_manager: BluetoothManagerSpec,
    pub audio: AudioControlSpec,
    pub power: QuickControlSpec,
    pub power_page: PowerPageSpec,
    pub brightness: Option<BrightnessControlSpec>,
    pub keyboard: KeyboardControlSpec,
    pub cpu_percent: Option<u8>,
    pub memory_percent: Option<u8>,
    pub storage: crate::StorageState,
    pub storage_health: Option<SourceHealth>,
    pub ai_usage: crate::AiUsageState,
    pub ai_usage_health: Option<SourceHealth>,
    pub battery_present: bool,
    pub battery_percent: Option<u8>,
    pub charging: bool,
    pub clock: String,
    pub clock_epoch: i64,
    pub calendar: Option<CalendarControlSpec>,
    pub calendar_agenda: Option<CalendarAgenda>,
    pub calendar_agenda_error: Option<String>,
    pub timers: Vec<TimerControlSpec>,
    pub media: Option<MediaControlSpec>,
    pub notifications: NotificationState,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SliderDebounce {
    pending: Option<u8>,
}

impl SliderDebounce {
    pub fn schedule(&mut self, percent: u8) {
        self.pending = Some(percent.min(100));
    }

    pub fn take(&mut self) -> Option<u8> {
        self.pending.take()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ControlCenterErrors {
    messages: BTreeMap<ControlCenterFocus, String>,
}

impl ControlCenterErrors {
    pub fn record_failure(&mut self, focus: ControlCenterFocus, detail: &str) {
        self.messages.insert(focus, detail.to_string());
    }

    pub fn retry(&mut self, focus: ControlCenterFocus) {
        self.messages.remove(&focus);
    }

    pub fn close(&mut self) {
        self.messages.clear();
    }

    pub fn message(&self, focus: ControlCenterFocus) -> Option<&str> {
        self.messages.get(&focus).map(String::as_str)
    }
}

pub fn control_center_origin(focus: ControlCenterFocus, action: &str) -> String {
    format!("control-center:{}:{action}", focus.as_str())
}

pub fn focus_from_origin(origin: &str) -> Option<ControlCenterFocus> {
    let section = origin.strip_prefix("control-center:")?.split(':').next()?;
    ControlCenterFocus::ALL
        .into_iter()
        .find(|focus| focus.as_str() == section)
}

pub fn build_control_center_spec(snapshot: &BarSnapshot) -> ControlCenterSpec {
    let system = &snapshot.system;
    let network = network_control_spec(&system.network);
    let bluetooth_detail = system
        .bluetooth
        .connected_device
        .clone()
        .unwrap_or_else(|| {
            if system.bluetooth.powered {
                "On"
            } else {
                "Off"
            }
            .to_string()
        });
    let mut audio_outputs = system
        .audio
        .outputs
        .iter()
        .map(audio_output_control_spec)
        .collect::<Vec<_>>();
    audio_outputs.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.name.cmp(&right.name))
    });
    let audio_detail = audio_outputs
        .iter()
        .find(|output| output.selected)
        .map(|output| output.label.clone())
        .unwrap_or_else(|| "No audio output".to_string());
    let media = system.media.as_ref().map(|media| MediaControlSpec {
        player: media.player.clone(),
        title: media.title.clone().unwrap_or_else(|| media.player.clone()),
        artist: media.artist.clone().unwrap_or_else(|| media.player.clone()),
        playing: media.status == PlaybackStatus::Playing,
        art_url: media.art_url.clone(),
    });

    ControlCenterSpec {
        network,
        network_traffic: NetworkTrafficSpec {
            interface: system.network.interface.clone(),
            download_bytes_per_second: system.network.download_bytes_per_second,
            upload_bytes_per_second: system.network.upload_bytes_per_second,
            download_history: system.network.download_history.clone(),
            upload_history: system.network.upload_history.clone(),
        },
        bluetooth: QuickControlSpec {
            icon_name: "bluetooth-symbolic".to_string(),
            label: "Bluetooth".to_string(),
            detail: bluetooth_detail,
            enabled: system.bluetooth.powered,
            available: system.bluetooth.available,
            toggle_available: system.bluetooth.available,
        },
        bluetooth_manager: BluetoothManagerSpec {
            available: system.bluetooth.available,
            powered: system.bluetooth.powered,
            discovering: system.bluetooth.discovering,
            connected_count: system
                .bluetooth
                .devices
                .iter()
                .filter(|device| device.connected)
                .count(),
            devices: system
                .bluetooth
                .devices
                .iter()
                .map(|device| BluetoothDeviceControlSpec {
                    address: device.address.clone(),
                    name: device.name.clone(),
                    icon_name: device.icon_name.clone(),
                    detail: bluetooth_device_detail(device),
                    paired: device.paired,
                    connected: device.connected,
                    operation: device.operation,
                    error: device.error.clone(),
                })
                .collect(),
            prompt: system.bluetooth.pairing_prompt.clone(),
            error: system.bluetooth.error.clone(),
        },
        audio: AudioControlSpec {
            percent: system.audio.volume_percent.map(|value| value.min(100)),
            muted: system.audio.muted,
            detail: audio_detail,
            outputs: audio_outputs,
        },
        power: QuickControlSpec {
            icon_name: power_profile_icon(&system.power.profile).to_string(),
            label: "Power".to_string(),
            detail: power_profile_label(&system.power.profile).to_string(),
            enabled: system.power.profile != PowerProfile::Balanced,
            available: true,
            toggle_available: false,
        },
        power_page: power_page_spec(&system.power),
        brightness: system
            .brightness
            .device
            .as_ref()
            .zip(system.brightness.percent)
            .map(|(device, percent)| BrightnessControlSpec {
                device: device.clone(),
                percent: percent.min(100),
            }),
        keyboard: keyboard_control_spec(&system.keyboard_layout),
        cpu_percent: system.resources.cpu_percent,
        memory_percent: system.resources.memory_percent,
        storage: system.storage.clone(),
        storage_health: system.source_health.get(&SourceId::Storage).cloned(),
        ai_usage: system.ai_usage.clone(),
        ai_usage_health: system.source_health.get(&SourceId::AiUsage).cloned(),
        battery_present: system.power.battery_present,
        battery_percent: system.power.battery_percent,
        charging: system.power.charging,
        clock: system.clock.label.clone(),
        clock_epoch: system.clock.epoch_seconds,
        calendar: system.calendar.as_ref().map(|event| CalendarControlSpec {
            title: event.title.clone(),
            location: event.location.clone(),
        }),
        calendar_agenda: system.calendar_agenda.clone(),
        calendar_agenda_error: match system.source_health.get(&SourceId::CalendarAgenda) {
            Some(SourceHealth::Disconnected { message }) => Some(message.clone()),
            Some(SourceHealth::Stale { .. }) => Some("Calendar agenda is stale".to_string()),
            _ => None,
        },
        timers: system
            .timers
            .iter()
            .map(|timer| TimerControlSpec {
                id: timer.id.clone(),
                label: timer.label.clone(),
                remaining_seconds: timer.remaining_seconds,
                completed: timer.completed,
                paused: !timer.completed && timer.target_epoch.is_none(),
            })
            .collect(),
        media,
        notifications: system.notifications.clone(),
    }
}

fn keyboard_control_spec(state: &KeyboardLayoutState) -> KeyboardControlSpec {
    let layouts = state
        .layouts
        .iter()
        .map(|option| {
            let (title, detail, _) = keyboard_labels(option);
            KeyboardLayoutControlSpec {
                index: option.index,
                title,
                detail,
                raw_name: option.name.clone(),
                selected: state.current_index == Some(option.index),
            }
        })
        .collect::<Vec<_>>();
    let current = state
        .current_index
        .and_then(|index| state.layouts.iter().find(|option| option.index == index));
    let (summary, current_label, detail) = if let Some(option) = current {
        let (title, variant, compact) = keyboard_labels(option);
        (
            compact,
            format!("{title} — {variant}"),
            state
                .current_name
                .clone()
                .unwrap_or_else(|| option.name.clone()),
        )
    } else if let Some(name) = state.current_name.as_deref() {
        let fallback = KeyboardLayoutOption {
            name: name.to_string(),
            ..KeyboardLayoutOption::default()
        };
        let (title, variant, compact) = keyboard_labels(&fallback);
        (compact, format!("{title} — {variant}"), name.to_string())
    } else {
        (
            "--".to_string(),
            "Unavailable".to_string(),
            "No active layout".to_string(),
        )
    };

    KeyboardControlSpec {
        summary,
        current: current_label,
        detail,
        layouts,
    }
}

fn keyboard_labels(option: &KeyboardLayoutOption) -> (String, String, String) {
    let raw = option.name.to_ascii_lowercase();
    let layout = option.layout.as_deref().map(str::to_ascii_lowercase);
    let variant = option.variant.as_deref().map(str::to_ascii_lowercase);
    let title = match layout.as_deref() {
        Some("us") => "US".to_string(),
        Some("de") => "DE".to_string(),
        Some(value) if !value.is_empty() => value.to_ascii_uppercase(),
        _ if raw.contains("english") || raw == "us" => "US".to_string(),
        _ if raw.contains("german") || raw == "de" => "DE".to_string(),
        _ => option
            .name
            .chars()
            .take(2)
            .collect::<String>()
            .to_uppercase(),
    };
    let detail = match variant.as_deref() {
        Some("dvorak") => "Dvorak".to_string(),
        Some("koy") => "KOY".to_string(),
        Some(value) if !value.is_empty() => value.to_string(),
        _ if raw.contains("dvorak") => "Dvorak".to_string(),
        _ if raw.contains("koy") => "KOY".to_string(),
        _ => "Standard".to_string(),
    };
    let suffix = match detail.as_str() {
        "Standard" => None,
        "Dvorak" => Some("DV"),
        "KOY" => Some("KOY"),
        value => Some(value),
    };
    let compact = suffix.map_or_else(|| title.clone(), |suffix| format!("{title}-{suffix}"));
    (title, detail, compact)
}

pub(super) fn keyboard_bar_labels(state: &KeyboardLayoutState) -> (String, String) {
    let spec = keyboard_control_spec(state);
    (spec.summary, format!("{} · {}", spec.current, spec.detail))
}

#[derive(Clone)]
struct NavigationUi {
    page: Rc<Cell<ControlCenterFocus>>,
    stack: gtk::Stack,
    back_button: gtk::Button,
    title: gtk::Label,
    subtitle: gtk::Label,
    footer_label: gtk::Label,
    footer: gtk::Button,
    error_slot: gtk::Box,
    error_label: gtk::Label,
    errors: Rc<RefCell<ControlCenterErrors>>,
    pending: Rc<RefCell<BTreeMap<ControlCenterFocus, usize>>>,
    page_changed: PageChanged,
    page_morph: MorphBin,
    heading_morph: MorphBin,
    return_origin: Rc<Cell<Option<MorphRect>>>,
}

type PageChanged = Rc<RefCell<Option<Rc<dyn Fn(ControlCenterFocus)>>>>;

impl NavigationUi {
    fn navigate(&self, page: ControlCenterFocus, backwards: bool) {
        self.navigate_with(page, backwards, None);
    }

    /// Expands the page out of the activated tile (window coordinates).
    fn navigate_from(&self, page: ControlCenterFocus, origin: MorphRect) {
        self.navigate_with(page, false, Some(origin));
    }

    /// Switches immediately to `page` without an inner crossfade; used when opening or
    /// switching pages from a topbar click so the entire popup grows as one unit.
    fn snap_to(&self, page: ControlCenterFocus) {
        self.page.set(page);
        self.return_origin.set(None);
        self.stack.set_visible_child_name(page.as_str());
        self.page_morph.cancel();
        self.heading_morph.cancel();
        self.title.set_label(page.title());
        self.subtitle.set_label(page.subtitle());
        self.back_button
            .set_visible(page != ControlCenterFocus::Overview);
        self.footer_label
            .set_label(&format!("Open {} in Luma", page.title()));
        self.footer.set_visible(page != ControlCenterFocus::AiUsage);
        self.render_error();
        if let Some(callback) = self.page_changed.borrow().as_ref() {
            callback(page);
        }
    }

    fn navigate_with(&self, page: ControlCenterFocus, backwards: bool, origin: Option<MorphRect>) {
        let previous = self.page.get();
        self.page.set(page);
        let show = || self.stack.set_visible_child_name(page.as_str());
        let _ = (backwards, origin);
        if previous == page || !self.page_morph.is_mapped() {
            self.return_origin.set(None);
            show();
            self.page_morph.cancel();
            self.heading_morph.cancel();
            self.title.set_label(page.title());
            self.subtitle.set_label(page.subtitle());
        } else {
            self.return_origin.set(None);
            self.page_morph.crossfade(show);
            self.heading_morph.crossfade(|| {
                self.title.set_label(page.title());
                self.subtitle.set_label(page.subtitle());
            });
        }
        self.back_button
            .set_visible(page != ControlCenterFocus::Overview);
        self.footer_label
            .set_label(&format!("Open {} in Luma", page.title()));
        self.footer.set_visible(page != ControlCenterFocus::AiUsage);
        self.render_error();
        if let Some(callback) = self.page_changed.borrow().as_ref() {
            callback(page);
        }
    }

    fn render_error(&self) {
        if let Some(message) = self.errors.borrow().message(self.page.get()) {
            self.error_label.set_label(message);
            self.error_slot.set_visible(true);
            self.error_slot.remove_css_class("is-pending");
            self.error_slot.add_css_class("has-error");
        } else if self
            .pending
            .borrow()
            .get(&self.page.get())
            .is_some_and(|count| *count > 0)
        {
            self.error_label.set_label("Applying change…");
            self.error_slot.set_visible(true);
            self.error_slot.remove_css_class("has-error");
            self.error_slot.add_css_class("is-pending");
        } else {
            self.error_label.set_label("");
            self.error_slot.set_visible(false);
            self.error_slot.remove_css_class("has-error");
            self.error_slot.remove_css_class("is-pending");
        }
    }

    fn begin_action(&self, focus: ControlCenterFocus) {
        *self.pending.borrow_mut().entry(focus).or_default() += 1;
        self.render_error();
    }

    fn finish_action(&self, focus: ControlCenterFocus) {
        let mut pending = self.pending.borrow_mut();
        if let Some(count) = pending.get_mut(&focus) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                pending.remove(&focus);
            }
        }
        drop(pending);
        self.render_error();
    }
}

#[derive(Clone)]
struct ActionHandle {
    sender: Sender<ActionRequest>,
    errors: Rc<RefCell<ControlCenterErrors>>,
    navigation: NavigationUi,
    focus: ControlCenterFocus,
}

impl ActionHandle {
    fn send(&self, action: &str, intent: ActionIntent) {
        self.errors.borrow_mut().retry(self.focus);
        self.navigation.begin_action(self.focus);
        if self
            .sender
            .send(ActionRequest {
                origin: control_center_origin(self.focus, action),
                intent,
            })
            .is_err()
        {
            self.navigation.finish_action(self.focus);
        }
    }
}

struct ToggleTile {
    root: gtk::Box,
    icon: gtk::Image,
    title: gtk::Label,
    detail: gtk::Label,
    toggle: gtk::Switch,
}

struct ActionTile {
    root: gtk::Box,
    icon: gtk::Image,
    title: gtk::Label,
    detail: gtk::Label,
    action: gtk::Button,
}

struct TileBody {
    button: gtk::Button,
    icon: gtk::Image,
    title: gtk::Label,
    detail: gtk::Label,
}

const METRIC_SEGMENTS: usize = 10;
const NETWORK_GRAPH_SAMPLES: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlCenterMotionPhase {
    Hidden,
    Entering,
    Visible,
    Exiting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlCenterMotionEvent {
    Present,
    Dismiss,
    Entered,
    Exited,
}

impl ControlCenterMotionPhase {
    fn transition(self, event: ControlCenterMotionEvent) -> Self {
        match (self, event) {
            (Self::Hidden | Self::Exiting, ControlCenterMotionEvent::Present) => Self::Entering,
            (Self::Entering | Self::Visible, ControlCenterMotionEvent::Dismiss) => Self::Exiting,
            (Self::Entering, ControlCenterMotionEvent::Entered) => Self::Visible,
            (Self::Exiting, ControlCenterMotionEvent::Exited) => Self::Hidden,
            _ => self,
        }
    }

    fn is_presented(self) -> bool {
        matches!(self, Self::Entering | Self::Visible)
    }
}

struct ControlCenterMotion {
    window: glib::WeakRef<gtk::ApplicationWindow>,
    root: glib::WeakRef<gtk::Box>,
    panel: glib::WeakRef<MorphBin>,
    phase: Cell<ControlCenterMotionPhase>,
    origin: Cell<MorphOrigin>,
}

impl ControlCenterMotion {
    fn new(window: &gtk::ApplicationWindow, root: &gtk::Box, panel: &MorphBin) -> Self {
        Self {
            window: window.downgrade(),
            root: root.downgrade(),
            panel: panel.downgrade(),
            phase: Cell::new(ControlCenterMotionPhase::Hidden),
            origin: Cell::new(MorphOrigin::Scaled(0.92)),
        }
    }

    fn present(self: &Rc<Self>, origin: MorphOrigin) {
        let current = self.phase.get();
        let retargeting_visible =
            current == ControlCenterMotionPhase::Visible && self.origin.get() != origin;
        let next = if retargeting_visible {
            ControlCenterMotionPhase::Entering
        } else {
            current.transition(ControlCenterMotionEvent::Present)
        };
        if next == current {
            return;
        }
        let (Some(window), Some(root), Some(panel)) = (
            self.window.upgrade(),
            self.root.upgrade(),
            self.panel.upgrade(),
        ) else {
            return;
        };
        root.set_sensitive(true);
        self.origin.set(origin);
        if !animations_enabled() {
            panel.cancel();
            self.phase.set(ControlCenterMotionPhase::Visible);
            window.present();
            return;
        }
        if retargeting_visible {
            panel.cancel();
        }
        self.phase.set(next);
        window.present();
        panel.expand_from(origin, false, || {});
        let motion = Rc::downgrade(self);
        panel.on_settled(move || {
            if let Some(motion) = motion.upgrade() {
                motion.phase.set(
                    motion
                        .phase
                        .get()
                        .transition(ControlCenterMotionEvent::Entered),
                );
            }
        });
    }

    fn dismiss(self: &Rc<Self>) {
        self.dismiss_then(|| {});
    }

    /// Collapses back into the origin, then runs `after` once the panel is hidden.
    fn dismiss_then(self: &Rc<Self>, after: impl FnOnce() + 'static) {
        let current = self.phase.get();
        let next = current.transition(ControlCenterMotionEvent::Dismiss);
        let (Some(_window), Some(root), Some(panel)) = (
            self.window.upgrade(),
            self.root.upgrade(),
            self.panel.upgrade(),
        ) else {
            after();
            return;
        };
        if next == current {
            after();
            return;
        }
        root.set_sensitive(false);
        if !animations_enabled() {
            self.hide_now();
            after();
            return;
        }
        self.phase.set(next);
        panel.collapse_into(self.origin.get(), false, || {});
        let motion = Rc::downgrade(self);
        panel.on_settled(move || {
            if let Some(motion) = motion.upgrade() {
                motion.phase.set(
                    motion
                        .phase
                        .get()
                        .transition(ControlCenterMotionEvent::Exited),
                );
                if let Some(root) = motion.root.upgrade() {
                    root.set_sensitive(true);
                }
                if let Some(window) = motion.window.upgrade() {
                    window.set_visible(false);
                }
            }
            after();
        });
    }

    /// Hides without animation; used by the session lock and output changes.
    fn hide_now(&self) {
        if let Some(panel) = self.panel.upgrade() {
            panel.cancel();
        }
        self.phase.set(ControlCenterMotionPhase::Hidden);
        if let Some(root) = self.root.upgrade() {
            root.set_sensitive(true);
        }
        if let Some(window) = self.window.upgrade() {
            window.set_visible(false);
        }
    }

    fn is_presented(&self) -> bool {
        self.phase.get().is_presented()
    }

    fn destroy(&self) {
        self.hide_now();
        if let Some(window) = self.window.upgrade() {
            window.close();
        }
    }
}

/// Converts a monitor-space origin into `panel_morph` local coordinates, falling back to a
/// centered scale when no button origin is available.
fn panel_origin(origin: Option<MorphRect>, monitor_width: i32, panel_width: i32) -> MorphOrigin {
    origin.map_or(MorphOrigin::Scaled(0.92), |rect| {
        MorphOrigin::Local(stage::monitor_to_panel_local(
            rect,
            monitor_width,
            panel_width,
        ))
    })
}

fn animations_enabled() -> bool {
    gtk::Settings::default().is_some_and(|settings| settings.is_gtk_enable_animations())
}

fn current_epoch_seconds() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

struct MetricGauge {
    root: gtk::Box,
    value: gtk::Label,
    segments: Vec<gtk::Box>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetricLevel {
    Normal,
    Warning,
    Critical,
    Unavailable,
}

#[derive(Debug, PartialEq, Eq)]
struct MetricVisual {
    active_segments: usize,
    level: MetricLevel,
    label: String,
}

pub struct ControlCenterView {
    window: gtk::ApplicationWindow,
    stage_box: gtk::Box,
    panel_morph: MorphBin,
    panel_scroll: gtk::ScrolledWindow,
    motion: Rc<ControlCenterMotion>,
    current: Rc<RefCell<ControlCenterSpec>>,
    navigation: NavigationUi,
    suggestion_list: gtk::Box,
    suggestion_specs: RefCell<Vec<Suggestion>>,
    suggestion_rows: RefCell<BTreeMap<String, gtk::Box>>,
    suggestion_section: gtk::Box,
    suggestion_engine: Rc<RefCell<SuggestionEngine>>,
    suggestion_dirty: Rc<super::surface::ContextDirty>,
    network_tile: ToggleTile,
    bluetooth_tile: ToggleTile,
    audio_tile: ToggleTile,
    power_tile: ActionTile,
    quick_grid: gtk::Grid,
    quick_layout: Cell<Option<(bool, bool)>>,
    volume_sliders: Vec<VolumeSliderWidgets>,
    audio_output_list: gtk::Box,
    audio_output_specs: RefCell<Vec<AudioOutputControlSpec>>,
    brightness_sliders: Vec<SliderWidgets>,
    brightness_device: Rc<RefCell<Option<String>>>,
    media_widgets: Vec<MediaWidgets>,
    artwork: ArtworkController,
    keyboard_summary: gtk::Label,
    resources_summary: gtk::Label,
    storage: storage::StorageView,
    storage_summaries: Vec<gtk::Label>,
    ai_usage: AiUsageView,
    ai_usage_summary: gtk::Label,
    battery_summary_button: gtk::Button,
    battery_summary: gtk::Label,
    time_summary: gtk::Label,
    network_page: NetworkPage,
    bluetooth_page: BluetoothPage,
    audio_state: gtk::Label,
    audio_detail: gtk::Label,
    power_page: PowerPage,
    keyboard_state: gtk::Label,
    keyboard_detail: gtk::Label,
    keyboard_layout_list: gtk::Box,
    keyboard_layout_specs: RefCell<Vec<KeyboardLayoutControlSpec>>,
    cpu_gauge: MetricGauge,
    memory_gauge: MetricGauge,
    time_page: TimePage,
    timer_widgets: RefCell<TimerWidgets>,
    clock_label: gtk::Label,
    errors: Rc<RefCell<ControlCenterErrors>>,
    suppress_controls: Rc<Cell<bool>>,
    bluetooth_discovery_active: Rc<Cell<bool>>,
    timer_sender: Sender<ActionRequest>,
    notification_list: gtk::Box,
    notification_specs: RefCell<Vec<Notification>>,
    notification_dnd: gtk::Switch,
    notification_control: Option<NotificationControl>,
}

impl ControlCenterView {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        application: &gtk::Application,
        monitor: &gtk::gdk::Monitor,
        output_name: &str,
        top_margin: i32,
        right_margin: i32,
        spec: &ControlCenterSpec,
        action_sender: Sender<ActionRequest>,
        calendar_sender: Option<Sender<CalendarMonthRequest>>,
        suggestion_engine: Rc<RefCell<SuggestionEngine>>,
        suggestion_dirty: Rc<Cell<bool>>,
        notification_control: Option<NotificationControl>,
    ) -> Self {
        Self::new_with_waker(
            application,
            monitor,
            output_name,
            top_margin,
            right_margin,
            spec,
            action_sender,
            calendar_sender,
            suggestion_engine,
            Rc::new(super::surface::ContextDirty::from_flag(suggestion_dirty)),
            notification_control,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn new_with_waker(
        application: &gtk::Application,
        monitor: &gtk::gdk::Monitor,
        output_name: &str,
        top_margin: i32,
        right_margin: i32,
        spec: &ControlCenterSpec,
        action_sender: Sender<ActionRequest>,
        calendar_sender: Option<Sender<CalendarMonthRequest>>,
        suggestion_engine: Rc<RefCell<SuggestionEngine>>,
        suggestion_dirty: Rc<super::surface::ContextDirty>,
        notification_control: Option<NotificationControl>,
    ) -> Self {
        let window = gtk::ApplicationWindow::builder()
            .application(application)
            .title("Vigil Quick Settings")
            .build();
        window.set_decorated(false);
        window.set_default_size(512, -1);
        window.add_css_class("control-center-window");
        window.init_layer_shell();
        window.set_namespace(Some("vigil-control-center"));
        window.set_layer(Layer::Overlay);
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Right, true);
        window.set_anchor(Edge::Bottom, true);
        window.set_margin(Edge::Top, top_margin);
        window.set_margin(Edge::Right, right_margin);
        window.set_margin(Edge::Bottom, top_margin);
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::OnDemand);
        window.set_monitor(Some(monitor));

        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.add_css_class("control-center-root");
        root.set_size_request(480, -1);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        header.add_css_class("control-header");
        let back_button = icon_button("go-previous-symbolic", "Back to overview");
        back_button.add_css_class("control-back");
        let heading_text = gtk::Box::new(gtk::Orientation::Vertical, 3);
        heading_text.set_hexpand(true);
        let title = gtk::Label::new(Some(ControlCenterFocus::Overview.title()));
        title.add_css_class("control-center-title");
        title.set_xalign(0.0);
        let subtitle = gtk::Label::new(Some(ControlCenterFocus::Overview.subtitle()));
        subtitle.add_css_class("supporting-text");
        subtitle.set_xalign(0.0);
        heading_text.append(&title);
        heading_text.append(&subtitle);
        let clock = gtk::Label::new(Some(&spec.clock));
        clock.add_css_class("control-center-clock");
        let close_button = gtk::Button::with_label("×");
        close_button.set_tooltip_text(Some("Close quick settings"));
        close_button.set_has_frame(false);
        close_button.add_css_class("control-close");
        header.append(&back_button);
        let heading_morph = MorphBin::new();
        heading_morph.set_hexpand(true);
        heading_morph.set_child(Some(&heading_text));
        header.append(&heading_morph);
        header.append(&clock);
        header.append(&close_button);
        root.append(&header);

        let stack = gtk::Stack::new();
        stack.add_css_class("control-stack");
        stack.set_transition_type(gtk::StackTransitionType::None);
        let page_morph = MorphBin::new();
        page_morph.set_follow(false, true);
        page_morph.set_radius(stage::style().card_radius as f32);
        page_morph.set_child(Some(&stack));
        stack.set_hhomogeneous(true);
        stack.set_vhomogeneous(false);

        let errors = Rc::new(RefCell::new(ControlCenterErrors::default()));
        let error_slot = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        error_slot.add_css_class("control-error-slot");
        error_slot.set_size_request(-1, 32);
        error_slot.set_visible(false);
        let error_icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
        let error_label = gtk::Label::new(None);
        error_label.set_xalign(0.0);
        error_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        error_label.set_hexpand(true);
        error_slot.append(&error_icon);
        error_slot.append(&error_label);

        let footer = gtk::Button::new();
        footer.add_css_class("control-footer");
        footer.set_has_frame(false);
        let footer_content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        footer_content.set_halign(gtk::Align::Fill);
        let footer_icon = gtk::Image::from_icon_name("system-search-symbolic");
        footer_icon.add_css_class("control-footer-icon");
        let footer_label = gtk::Label::new(Some("Open Quick Settings in Luma"));
        footer_label.set_xalign(0.0);
        footer_label.set_hexpand(true);
        let footer_arrow = gtk::Image::from_icon_name("go-next-symbolic");
        footer_arrow.add_css_class("control-footer-arrow");
        footer_content.append(&footer_icon);
        footer_content.append(&footer_label);
        footer_content.append(&footer_arrow);
        footer.set_child(Some(&footer_content));

        let navigation = NavigationUi {
            page: Rc::new(Cell::new(ControlCenterFocus::Overview)),
            stack: stack.clone(),
            back_button: back_button.clone(),
            title,
            subtitle,
            footer_label,
            footer: footer.clone(),
            error_slot: error_slot.clone(),
            error_label,
            errors: errors.clone(),
            pending: Rc::new(RefCell::new(BTreeMap::new())),
            page_changed: Rc::new(RefCell::new(None)),
            page_morph: page_morph.clone(),
            heading_morph: heading_morph.clone(),
            return_origin: Rc::new(Cell::new(None)),
        };

        let current = Rc::new(RefCell::new(spec.clone()));
        let suppress_controls = Rc::new(Cell::new(false));
        let bluetooth_discovery_active = Rc::new(Cell::new(false));
        let discovery_sender = action_sender.clone();
        let discovery_current = current.clone();
        let discovery_active = bluetooth_discovery_active.clone();
        let brightness_device = Rc::new(RefCell::new(
            spec.brightness.as_ref().map(|value| value.device.clone()),
        ));

        let overview = gtk::Box::new(gtk::Orientation::Vertical, 12);
        overview.add_css_class("control-page");
        let suggestion_section = gtk::Box::new(gtk::Orientation::Vertical, 7);
        suggestion_section.add_css_class("suggestion-section");
        suggestion_section.set_visible(false);
        suggestion_section.append(&section_eyebrow("SUGGESTED NOW"));
        let suggestion_list = gtk::Box::new(gtk::Orientation::Vertical, 7);
        suggestion_list.add_css_class("suggestion-list");
        suggestion_section.append(&suggestion_list);
        overview.append(&suggestion_section);
        let quick_grid = gtk::Grid::new();
        quick_grid.add_css_class("control-grid");
        quick_grid.set_row_spacing(8);
        quick_grid.set_column_spacing(8);
        quick_grid.set_column_homogeneous(true);

        let network_tile = toggle_tile(
            "network-wireless-symbolic",
            "Wi-Fi",
            ControlCenterFocus::Network,
            &navigation,
        );
        let bluetooth_tile = toggle_tile(
            "bluetooth-symbolic",
            "Bluetooth",
            ControlCenterFocus::Bluetooth,
            &navigation,
        );
        let audio_tile = toggle_tile(
            "audio-volume-high-symbolic",
            "Audio",
            ControlCenterFocus::Audio,
            &navigation,
        );
        let power_tile = action_tile(
            "power-profile-balanced-symbolic",
            "Power",
            ControlCenterFocus::Power,
            &navigation,
        );
        overview.append(&section_eyebrow("CONTROLS"));
        overview.append(&quick_grid);

        let sliders = gtk::Box::new(gtk::Orientation::Vertical, 6);
        sliders.add_css_class("control-slider-group");
        let overview_volume = volume_slider_row();
        let overview_brightness = slider_row("display-brightness-symbolic", "Brightness");
        sliders.append(&overview_volume.slider.row);
        sliders.append(&overview_brightness.row);
        overview.append(&section_eyebrow("LEVELS"));
        overview.append(&sliders);

        let overview_media = media_card(
            action_sender.clone(),
            errors.clone(),
            navigation.clone(),
            current.clone(),
        );
        overview.append(&overview_media.root);

        let summaries = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        summaries.add_css_class("summary-grid");
        summaries.set_homogeneous(true);
        let (keyboard_summary_button, keyboard_summary) =
            summary_tile("input-keyboard-symbolic", "Keyboard");
        let (resources_summary_button, resources_summary) =
            summary_tile("utilities-system-monitor-symbolic", "CPU / RAM");
        let (battery_summary_button, battery_summary) =
            summary_tile("battery-good-symbolic", "Battery");
        let (time_summary_button, time_summary) =
            summary_tile("preferences-system-time-symbolic", "Focus");
        keyboard_summary_button.add_css_class("keyboard");
        resources_summary_button.add_css_class("resources");
        battery_summary_button.add_css_class("power");
        time_summary_button.add_css_class("clock");
        connect_navigation(
            &keyboard_summary_button,
            ControlCenterFocus::Keyboard,
            &navigation,
        );
        connect_navigation(
            &resources_summary_button,
            ControlCenterFocus::Resources,
            &navigation,
        );
        connect_navigation(
            &battery_summary_button,
            ControlCenterFocus::Power,
            &navigation,
        );
        connect_navigation(&time_summary_button, ControlCenterFocus::Clock, &navigation);
        summaries.append(&keyboard_summary_button);
        summaries.append(&resources_summary_button);
        summaries.append(&battery_summary_button);
        summaries.append(&time_summary_button);
        overview.append(&section_eyebrow("AT A GLANCE"));
        overview.append(&summaries);
        let (storage_button, storage_summary) = summary_tile("drive-harddisk-symbolic", "Storage");
        storage_summary.set_max_width_chars(40);
        connect_navigation(&storage_button, ControlCenterFocus::Storage, &navigation);
        overview.append(&storage_button);
        overview.append(&section_eyebrow("DESKTOP"));
        overview.append(&desktop_mode_row(ActionHandle {
            sender: action_sender.clone(),
            errors: errors.clone(),
            navigation: navigation.clone(),
            focus: ControlCenterFocus::Overview,
        }));
        stack.add_named(&overview, Some(ControlCenterFocus::Overview.as_str()));

        let network_page = network_page();
        stack.add_named(
            &network_page.connectivity.root,
            Some(ControlCenterFocus::Network.as_str()),
        );

        let bluetooth_page = bluetooth_page();
        stack.add_named(
            &bluetooth_page.root,
            Some(ControlCenterFocus::Bluetooth.as_str()),
        );

        let audio_page = gtk::Box::new(gtk::Orientation::Vertical, 12);
        audio_page.add_css_class("control-page");
        let (audio_hero, audio_state, audio_detail) =
            detail_hero("audio-volume-high-symbolic", "Audio");
        audio_page.append(&audio_hero);
        let detail_volume = volume_slider_row();
        audio_page.append(&detail_volume.slider.row);
        audio_page.append(&section_eyebrow("OUTPUT"));
        let audio_output_list = gtk::Box::new(gtk::Orientation::Vertical, 6);
        audio_output_list.add_css_class("audio-output-list");
        audio_output_list.set_vexpand(false);
        audio_page.append(&audio_output_list);
        let detail_media = media_card(
            action_sender.clone(),
            errors.clone(),
            navigation.clone(),
            current.clone(),
        );
        audio_page.append(&detail_media.root);
        stack.add_named(&audio_page, Some(ControlCenterFocus::Audio.as_str()));

        let power_page = power_page();
        let detail_brightness = slider_row("display-brightness-symbolic", "Brightness");
        power_page.root.append(&detail_brightness.row);
        stack.add_named(&power_page.root, Some(ControlCenterFocus::Power.as_str()));

        let keyboard_page = gtk::Box::new(gtk::Orientation::Vertical, 12);
        keyboard_page.add_css_class("control-page");
        let (keyboard_hero, keyboard_state, keyboard_detail) =
            detail_hero("input-keyboard-symbolic", "Current layout");
        keyboard_page.append(&keyboard_hero);
        keyboard_page.append(&section_eyebrow("AVAILABLE LAYOUTS"));
        let keyboard_layout_list = gtk::Box::new(gtk::Orientation::Vertical, 6);
        keyboard_layout_list.add_css_class("keyboard-layout-list");
        keyboard_page.append(&keyboard_layout_list);
        stack.add_named(&keyboard_page, Some(ControlCenterFocus::Keyboard.as_str()));

        let resources_page = gtk::Box::new(gtk::Orientation::Vertical, 12);
        resources_page.add_css_class("control-page");
        let resources_intro = section_intro(
            "utilities-system-monitor-symbolic",
            "Live system pressure",
            "A quick read of processor and memory use.",
        );
        resources_page.append(&resources_intro);
        let cpu_gauge = metric_row("CPU");
        let memory_gauge = metric_row("Memory");
        resources_page.append(&cpu_gauge.root);
        resources_page.append(&memory_gauge.root);
        let (storage_button, resources_storage_summary) =
            summary_tile("drive-harddisk-symbolic", "Storage");
        resources_storage_summary.set_max_width_chars(40);
        connect_navigation(&storage_button, ControlCenterFocus::Storage, &navigation);
        resources_page.append(&storage_button);
        let storage = storage::StorageView::new();
        stack.add_named(&storage.root, Some(ControlCenterFocus::Storage.as_str()));
        let (ai_usage_button, ai_usage_summary) =
            summary_tile("applications-science-symbolic", "AI Usage");
        connect_navigation(&ai_usage_button, ControlCenterFocus::AiUsage, &navigation);
        resources_page.append(&ai_usage_button);
        stack.add_named(
            &resources_page,
            Some(ControlCenterFocus::Resources.as_str()),
        );

        let ai_usage = AiUsageView::new();
        ai_usage.update(&spec.ai_usage, spec.ai_usage_health.as_ref());
        let ai_scroll = gtk::ScrolledWindow::new();
        ai_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        ai_scroll.set_propagate_natural_height(true);
        ai_scroll.set_max_content_height(700);
        ai_scroll.set_child(Some(&ai_usage.root));
        stack.add_named(&ai_scroll, Some(ControlCenterFocus::AiUsage.as_str()));

        let time_page = build_time_page(
            spec,
            action_sender.clone(),
            errors.clone(),
            navigation.clone(),
            calendar_sender,
        );
        stack.add_named(&time_page.root, Some(ControlCenterFocus::Clock.as_str()));
        let notifications_page = gtk::Box::new(gtk::Orientation::Vertical, 10);
        notifications_page.add_css_class("control-page");
        notifications_page.add_css_class("notifications-page");
        let notification_controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        notification_controls.add_css_class("notification-controls");
        let dnd_label = gtk::Label::new(Some("Do Not Disturb"));
        dnd_label.set_xalign(0.0);
        dnd_label.set_hexpand(true);
        let notification_dnd = gtk::Switch::new();
        notification_dnd.set_valign(gtk::Align::Center);
        let clear_notifications = gtk::Button::with_label("Clear all");
        clear_notifications.add_css_class("compact-action");
        notification_controls.append(&dnd_label);
        notification_controls.append(&notification_dnd);
        notification_controls.append(&clear_notifications);
        notifications_page.append(&notification_controls);
        let notification_scroll = gtk::ScrolledWindow::new();
        notification_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        notification_scroll.set_min_content_height(280);
        notification_scroll.set_max_content_height(620);
        let notification_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        notification_list.add_css_class("notification-list");
        notification_scroll.set_child(Some(&notification_list));
        notifications_page.append(&notification_scroll);
        stack.add_named(
            &notifications_page,
            Some(ControlCenterFocus::Notifications.as_str()),
        );
        root.append(&page_morph);
        root.append(&error_slot);
        root.append(&footer);

        let panel_morph = MorphBin::new();
        panel_morph.set_halign(gtk::Align::End);
        panel_morph.set_valign(gtk::Align::Start);
        panel_morph.set_margin_top(stage::panel_top_in_stage());
        panel_morph.set_margin_start(stage::style().panel_margin);
        panel_morph.set_margin_end(stage::style().panel_margin);
        panel_morph.set_margin_bottom(stage::style().panel_bottom_margin);
        panel_morph.set_radius(stage::style().panel_radius as f32);
        panel_morph.set_shadow(stage::style().shadows.then_some(MorphShadow {
            dy: 18.0,
            blur: 48.0,
            alpha: 0.44,
        }));

        // Bound the panel on shorter outputs without clipping its footer or input shape.
        let panel_scroll = gtk::ScrolledWindow::new();
        panel_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        panel_scroll.set_propagate_natural_height(true);
        panel_scroll
            .set_max_content_height(stage::panel_available_height(monitor.geometry().height()));
        panel_scroll.set_child(Some(&root));
        panel_morph.set_child(Some(&panel_scroll));
        let stage_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        stage_box.set_size_request((monitor.geometry().width() - right_margin * 2).max(568), -1);
        stage_box.append(&panel_morph);
        window.set_child(Some(&stage_box));

        let time_page_for_navigation = time_page.clone();
        let notification_control_for_navigation = notification_control.clone();
        *navigation.page_changed.borrow_mut() = Some(Rc::new(move |page| {
            sync_bluetooth_discovery(
                &discovery_sender,
                &discovery_current,
                &discovery_active,
                page == ControlCenterFocus::Bluetooth,
            );
            if page == ControlCenterFocus::Clock {
                reset_time_page(&time_page_for_navigation);
            }
            if page == ControlCenterFocus::Notifications
                && let Some(control) = &notification_control_for_navigation
            {
                let _ = control.mark_read();
            }
        }));
        {
            let panel = panel_morph.downgrade();
            stage::track_input(&window, move || {
                panel
                    .upgrade()
                    .and_then(|panel| {
                        let root = panel.root()?;
                        let shape = panel.shape_in(&root)?;
                        // Never take pointer focus outside the resting panel (e.g. over the bar).
                        let bounds = panel.compute_bounds(&root)?;
                        shape.intersection(MorphRect::new(
                            bounds.x(),
                            bounds.y(),
                            bounds.width(),
                            bounds.height(),
                        ))
                    })
                    .into_iter()
                    .collect()
            });
        }

        let motion = Rc::new(ControlCenterMotion::new(&window, &root, &panel_morph));

        {
            let notification_control = notification_control.clone();
            let suppress_controls = suppress_controls.clone();
            notification_dnd.connect_active_notify(move |switch| {
                if suppress_controls.get() {
                    return;
                }
                if let Some(control) = &notification_control {
                    let _ = control.set_dnd(switch.is_active());
                }
            });
        }
        {
            let notification_control = notification_control.clone();
            clear_notifications.connect_clicked(move |_| {
                if let Some(control) = &notification_control {
                    let _ = control.clear();
                }
            });
        }

        let nav_for_back = navigation.clone();
        back_button.connect_clicked(move |_| {
            nav_for_back.navigate(ControlCenterFocus::Overview, true);
        });
        let motion_for_close_button = motion.clone();
        close_button.connect_clicked(move |_| {
            motion_for_close_button.dismiss();
        });
        let nav_for_footer = navigation.clone();
        let sender_for_footer = action_sender.clone();
        let motion_for_footer = motion.clone();
        let output_for_footer = output_name.to_string();
        footer.connect_clicked(move |_| {
            let focus = nav_for_footer.page.get();
            let sender = sender_for_footer.clone();
            let output = output_for_footer.clone();
            motion_for_footer.dismiss_then(move || {
                let _ = sender.send(ActionRequest {
                    origin: control_center_origin(focus, "open-luma"),
                    intent: ActionIntent::OpenContextQuery {
                        context: focus.into(),
                        output,
                    },
                });
            });
        });

        connect_toggle_controls(
            [&network_tile.toggle, &network_page.connectivity.toggle],
            suppress_controls.clone(),
            current.clone(),
            ActionHandle {
                sender: action_sender.clone(),
                errors: errors.clone(),
                navigation: navigation.clone(),
                focus: ControlCenterFocus::Network,
            },
            |spec, active| {
                (spec.network.toggle_available && spec.network.enabled != active)
                    .then_some(ActionIntent::SetWifiEnabled { enabled: active })
            },
        );
        connect_toggle_controls(
            [&bluetooth_tile.toggle, &bluetooth_page.toggle],
            suppress_controls.clone(),
            current.clone(),
            ActionHandle {
                sender: action_sender.clone(),
                errors: errors.clone(),
                navigation: navigation.clone(),
                focus: ControlCenterFocus::Bluetooth,
            },
            |spec, active| {
                (spec.bluetooth.toggle_available && spec.bluetooth.enabled != active)
                    .then_some(ActionIntent::SetBluetoothPowered { powered: active })
            },
        );
        let audio_action = ActionHandle {
            sender: action_sender.clone(),
            errors: errors.clone(),
            navigation: navigation.clone(),
            focus: ControlCenterFocus::Audio,
        };
        connect_toggle_controls(
            [&audio_tile.toggle],
            suppress_controls.clone(),
            current.clone(),
            audio_action.clone(),
            |spec, active| (spec.audio.muted == active).then_some(ActionIntent::ToggleMute),
        );
        for volume in [&overview_volume, &detail_volume] {
            let handle = audio_action.clone();
            volume.button.connect_clicked(move |_| {
                handle.send("toggle-mute", ActionIntent::ToggleMute);
            });
        }

        let power_action = ActionHandle {
            sender: action_sender.clone(),
            errors: errors.clone(),
            navigation: navigation.clone(),
            focus: ControlCenterFocus::Power,
        };
        {
            let handle = power_action.clone();
            power_tile.action.connect_clicked(move |_| {
                handle.send(
                    "cycle-profile",
                    ActionIntent::CyclePowerProfile {
                        direction: Direction::Next,
                    },
                );
            });
        }
        for (profile, button) in &power_page.profile_buttons {
            let handle = power_action.clone();
            let profile = profile.clone();
            button.connect_clicked(move |button| {
                if !button.has_css_class("selected") {
                    handle.send(
                        "set-profile",
                        ActionIntent::SetPowerProfile {
                            profile: profile.clone(),
                        },
                    );
                }
            });
        }

        let volume_handle = ActionHandle {
            sender: action_sender.clone(),
            errors: errors.clone(),
            navigation: navigation.clone(),
            focus: ControlCenterFocus::Audio,
        };
        for volume in [&overview_volume, &detail_volume] {
            install_percent_debounce(
                &volume.slider.scale,
                volume.slider.value.clone(),
                suppress_controls.clone(),
                volume_handle.clone(),
                "set-volume",
                |percent| ActionIntent::SetVolumePercent { percent },
            );
        }

        let brightness_handle = ActionHandle {
            sender: action_sender.clone(),
            errors: errors.clone(),
            navigation: navigation.clone(),
            focus: ControlCenterFocus::Power,
        };
        for brightness in [&overview_brightness, &detail_brightness] {
            let device = brightness_device.clone();
            install_percent_debounce(
                &brightness.scale,
                brightness.value.clone(),
                suppress_controls.clone(),
                brightness_handle.clone(),
                "set-brightness",
                move |percent| ActionIntent::SetBrightnessPercent {
                    device: device.borrow().clone().unwrap_or_default(),
                    percent,
                },
            );
        }

        let errors_for_close = errors.clone();
        let nav_for_close = navigation.clone();
        let current_for_close = current.clone();
        let discovery_for_close = bluetooth_discovery_active.clone();
        let sender_for_close = action_sender.clone();
        window.connect_visible_notify(move |window| {
            if !window.is_visible() {
                errors_for_close.borrow_mut().close();
                nav_for_close.pending.borrow_mut().clear();
                nav_for_close.render_error();
                sync_bluetooth_discovery(
                    &sender_for_close,
                    &current_for_close,
                    &discovery_for_close,
                    false,
                );
            }
        });

        let escape = gtk::EventControllerKey::new();
        let motion_for_escape = motion.clone();
        escape.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                motion_for_escape.dismiss();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.add_controller(escape);

        // The compositor can leave pointer focus on the stage after it opens under a still
        // pointer; a press outside the panel then dismisses it, like clicking the opener again.
        let away = gtk::GestureClick::new();
        away.set_button(1);
        let motion_for_away = motion.clone();
        let panel_for_away = panel_morph.downgrade();
        away.connect_pressed(move |gesture, _, x, y| {
            let Some(panel) = panel_for_away.upgrade() else {
                return;
            };
            let inside = gesture
                .widget()
                .and_then(|widget| panel.compute_bounds(&widget))
                .is_some_and(|bounds| {
                    bounds.contains_point(&gtk::graphene::Point::new(x as f32, y as f32))
                });
            if !inside {
                motion_for_away.dismiss();
            }
        });
        window.add_controller(away);

        let media_widgets = vec![overview_media, detail_media];
        let artwork = ArtworkController::new(&window, &media_widgets);
        let view = Self {
            window,
            stage_box,
            panel_morph,
            panel_scroll,
            motion,
            current,
            navigation,
            suggestion_list,
            suggestion_specs: RefCell::new(Vec::new()),
            suggestion_rows: RefCell::new(BTreeMap::new()),
            suggestion_section,
            suggestion_engine,
            suggestion_dirty,
            network_tile,
            bluetooth_tile,
            audio_tile,
            power_tile,
            quick_grid,
            quick_layout: Cell::new(None),
            volume_sliders: vec![overview_volume, detail_volume],
            audio_output_list,
            audio_output_specs: RefCell::new(Vec::new()),
            brightness_sliders: vec![overview_brightness, detail_brightness],
            brightness_device,
            media_widgets,
            artwork,
            keyboard_summary,
            resources_summary,
            storage,
            storage_summaries: vec![storage_summary, resources_storage_summary],
            ai_usage,
            ai_usage_summary,
            battery_summary_button,
            battery_summary,
            time_summary,
            network_page,
            bluetooth_page,
            audio_state,
            audio_detail,
            power_page,
            keyboard_state,
            keyboard_detail,
            keyboard_layout_list,
            keyboard_layout_specs: RefCell::new(Vec::new()),
            cpu_gauge,
            memory_gauge,
            time_page,
            timer_widgets: RefCell::new(BTreeMap::new()),
            clock_label: clock,
            errors,
            suppress_controls,
            bluetooth_discovery_active,
            timer_sender: action_sender,
            notification_list,
            notification_specs: RefCell::new(Vec::new()),
            notification_dnd,
            notification_control,
        };
        view.navigation
            .navigate(ControlCenterFocus::Overview, false);
        view.update(spec);
        view
    }

    pub fn window(&self) -> &gtk::ApplicationWindow {
        &self.window
    }

    pub fn set_monitor(&self, monitor: &gtk::gdk::Monitor) {
        self.window.set_monitor(Some(monitor));
        self.panel_scroll
            .set_max_content_height(stage::panel_available_height(monitor.geometry().height()));
    }

    pub fn present(&self) {
        self.present_from(None);
    }

    /// Opens the panel growing out of `origin` (monitor-logical coordinates).
    pub fn present_from(&self, origin: Option<MorphRect>) {
        let monitor_width = self
            .window
            .monitor()
            .map_or(0, |monitor| monitor.geometry().width());
        let (_, measured_outer_width, _, _) =
            self.panel_morph.measure(gtk::Orientation::Horizontal, -1);
        let allocated_panel_width = self.panel_morph.width();
        let panel_width = if allocated_panel_width > 0 {
            allocated_panel_width
        } else {
            (measured_outer_width - stage::style().panel_margin * 2).max(480)
        };
        let stage_panel_width =
            measured_outer_width.max(panel_width + stage::style().panel_margin * 2);
        let needed = stage::control_center_stage_width(stage_panel_width, origin, monitor_width);
        let visible = self.window.is_visible();
        let current_request = self.stage_box.width_request();
        let request = stage::stage_width_request(current_request, needed, visible);
        if request != current_request {
            self.stage_box.set_size_request(request, -1);
            if !visible {
                // Otherwise a hidden window keeps a wider earlier size and offsets origins.
                self.window.set_default_size(request, -1);
            }
        }
        self.motion
            .present(panel_origin(origin, monitor_width, panel_width));
        sync_bluetooth_discovery(
            &self.timer_sender,
            &self.current,
            &self.bluetooth_discovery_active,
            self.current_page() == ControlCenterFocus::Bluetooth,
        );
    }

    /// Hides immediately without a morph (session lock, IPC close).
    pub fn dismiss_now(&self) {
        sync_bluetooth_discovery(
            &self.timer_sender,
            &self.current,
            &self.bluetooth_discovery_active,
            false,
        );
        self.motion.hide_now();
    }

    pub fn dismiss(&self) {
        sync_bluetooth_discovery(
            &self.timer_sender,
            &self.current,
            &self.bluetooth_discovery_active,
            false,
        );
        self.motion.dismiss();
    }

    pub fn is_visible(&self) -> bool {
        self.motion.is_presented()
    }

    pub fn destroy(&self) {
        sync_bluetooth_discovery(
            &self.timer_sender,
            &self.current,
            &self.bluetooth_discovery_active,
            false,
        );
        self.motion.destroy();
    }

    pub fn show_page(&self, focus: ControlCenterFocus) {
        self.navigation.snap_to(focus);
    }

    pub fn current_page(&self) -> ControlCenterFocus {
        self.navigation.page.get()
    }

    pub fn media_player(&self) -> Option<String> {
        self.current
            .borrow()
            .media
            .as_ref()
            .map(|media| media.player.clone())
    }

    pub fn update(&self, spec: &ControlCenterSpec) {
        *self.current.borrow_mut() = spec.clone();
        sync_bluetooth_discovery(
            &self.timer_sender,
            &self.current,
            &self.bluetooth_discovery_active,
            self.is_visible() && self.current_page() == ControlCenterFocus::Bluetooth,
        );
        self.suppress_controls.set(true);

        update_toggle_tile(&self.network_tile, &spec.network);
        update_toggle_tile(&self.bluetooth_tile, &spec.bluetooth);
        let quick_layout = (spec.network.available, spec.bluetooth.available);
        if self.quick_layout.get() != Some(quick_layout) {
            reflow_quick_grid(
                &self.quick_grid,
                [
                    (&self.network_tile.root, spec.network.available),
                    (&self.audio_tile.root, true),
                    (&self.bluetooth_tile.root, spec.bluetooth.available),
                    (&self.power_tile.root, true),
                ],
            );
            self.quick_layout.set(Some(quick_layout));
        }
        let audio_available = !spec.audio.outputs.is_empty() && spec.audio.percent.is_some();
        self.audio_tile.detail.set_label(
            &spec
                .audio
                .percent
                .map(|percent| {
                    format!(
                        "{} · {percent}%",
                        if spec.audio.muted {
                            "Muted"
                        } else {
                            &spec.audio.detail
                        }
                    )
                })
                .unwrap_or_else(|| spec.audio.detail.clone()),
        );
        set_enabled_class(&self.audio_tile.root, audio_available && !spec.audio.muted);
        self.audio_tile.toggle.set_active(!spec.audio.muted);
        self.audio_tile.toggle.set_sensitive(audio_available);
        update_action_tile(&self.power_tile, &spec.power);

        let volume = spec.audio.percent.unwrap_or_default();
        let volume_label = spec
            .audio
            .percent
            .map(|percent| format!("{percent}%"))
            .unwrap_or_else(|| "--".to_string());
        for widgets in &self.volume_sliders {
            widgets.slider.scale.set_value(f64::from(volume));
            widgets.slider.scale.set_sensitive(audio_available);
            widgets.slider.value.set_label(&volume_label);
            widgets
                .button
                .set_icon_name(volume_icon_name(spec.audio.muted, spec.audio.percent));
            widgets
                .button
                .set_tooltip_text(Some(if spec.audio.muted { "Unmute" } else { "Mute" }));
            widgets.button.set_sensitive(audio_available);
            if spec.audio.muted {
                widgets.button.add_css_class("muted");
            } else {
                widgets.button.remove_css_class("muted");
            }
        }

        if let Some(brightness) = spec.brightness.as_ref() {
            *self.brightness_device.borrow_mut() = Some(brightness.device.clone());
            for widgets in &self.brightness_sliders {
                widgets.row.set_visible(true);
                widgets.scale.set_value(f64::from(brightness.percent));
                widgets.value.set_label(&format!("{}%", brightness.percent));
            }
        } else {
            *self.brightness_device.borrow_mut() = None;
            for widgets in &self.brightness_sliders {
                widgets.row.set_visible(false);
            }
        }

        for media in &self.media_widgets {
            update_media(media, spec.media.as_ref());
        }
        self.artwork.show(spec.media.as_ref(), &self.media_widgets);

        self.keyboard_summary.set_label(&spec.keyboard.summary);
        self.storage
            .update(&spec.storage, spec.storage_health.as_ref());
        let storage_summary = storage::summary(&spec.storage, spec.storage_health.as_ref());
        for label in &self.storage_summaries {
            label.set_label(&storage_summary);
            label.set_tooltip_text(Some(&storage_summary));
        }
        self.ai_usage
            .update(&spec.ai_usage, spec.ai_usage_health.as_ref());
        self.ai_usage_summary.set_label(&super::ai_usage::label(
            &spec.ai_usage,
            spec.ai_usage_health.as_ref(),
        ));
        self.resources_summary.set_label(&format!(
            "{} / {}",
            percent_label(spec.cpu_percent),
            percent_label(spec.memory_percent)
        ));
        self.battery_summary_button
            .set_visible(spec.battery_present);
        if spec.battery_present {
            self.battery_summary.set_label(
                &spec
                    .battery_percent
                    .map(|percent| format!("{percent}%"))
                    .unwrap_or_else(|| "--".to_string()),
            );
        }
        self.time_summary.set_label(
            &spec
                .timers
                .first()
                .map(timer_summary)
                .or_else(|| spec.calendar.as_ref().map(|event| event.title.clone()))
                .unwrap_or_else(|| spec.clock.clone()),
        );

        update_connectivity_page(&self.network_page.connectivity, &spec.network);
        update_network_traffic(&self.network_page, &spec.network_traffic);
        update_bluetooth_page(
            &self.bluetooth_page,
            &spec.bluetooth_manager,
            self.timer_sender.clone(),
        );
        self.audio_state
            .set_label(if spec.audio.outputs.is_empty() {
                "Unavailable"
            } else if spec.audio.muted {
                "Muted"
            } else {
                "Sound on"
            });
        self.audio_detail.set_label(&spec.audio.detail);
        reconcile_audio_outputs(
            &self.audio_output_list,
            &mut self.audio_output_specs.borrow_mut(),
            &spec.audio.outputs,
            self.timer_sender.clone(),
            self.errors.clone(),
            self.navigation.clone(),
        );

        update_power_page(&self.power_page, &spec.power_page);
        self.keyboard_state.set_label(&spec.keyboard.current);
        self.keyboard_detail.set_label(&spec.keyboard.detail);
        reconcile_keyboard_layouts(
            &self.keyboard_layout_list,
            &mut self.keyboard_layout_specs.borrow_mut(),
            &spec.keyboard.layouts,
            self.timer_sender.clone(),
            self.errors.clone(),
            self.navigation.clone(),
        );
        update_metric(&self.cpu_gauge, spec.cpu_percent);
        update_metric(&self.memory_gauge, spec.memory_percent);

        update_time_page(&self.time_page, spec);
        reconcile_timers(
            &self.time_page.timer_list,
            &self.time_page.timer_empty,
            &mut self.timer_widgets.borrow_mut(),
            &spec.timers,
            self.timer_sender.clone(),
            self.errors.clone(),
            self.navigation.clone(),
        );
        self.clock_label.set_label(&spec.clock);
        self.notification_dnd.set_active(spec.notifications.dnd);
        self.update_notifications(&spec.notifications);
        let unavailable_page = match self.navigation.page.get() {
            ControlCenterFocus::Network => !spec.network.available,
            ControlCenterFocus::Bluetooth => !spec.bluetooth.available,
            _ => false,
        };
        if unavailable_page {
            self.navigation.navigate(ControlCenterFocus::Overview, true);
        }
        self.suppress_controls.set(false);
    }

    fn update_notifications(&self, state: &NotificationState) {
        if self.notification_specs.borrow().as_slice() == state.items.as_slice() {
            return;
        }
        while let Some(child) = self.notification_list.first_child() {
            self.notification_list.remove(&child);
        }
        *self.notification_specs.borrow_mut() = state.items.clone();
        if state.items.iter().all(|item| item.duplicate_count == 0) {
            let empty = gtk::Label::new(Some("You’re all caught up"));
            empty.add_css_class("notification-empty");
            empty.set_margin_top(28);
            empty.set_margin_bottom(28);
            self.notification_list.append(&empty);
            return;
        }
        for notification in &state.items {
            if notification.duplicate_count == 0 {
                continue;
            }
            self.notification_list.append(&notification_row(
                notification,
                self.notification_control.as_ref(),
                NotificationPresentation::Inbox,
            ));
        }
    }

    pub fn update_suggestions(&self, suggestions: &[Suggestion]) {
        self.suggestion_section.set_visible(!suggestions.is_empty());
        if self.suggestion_specs.borrow().as_slice() == suggestions {
            return;
        }
        while let Some(child) = self.suggestion_list.first_child() {
            self.suggestion_list.remove(&child);
        }
        self.suggestion_rows.borrow_mut().clear();
        *self.suggestion_specs.borrow_mut() = suggestions.to_vec();
        self.suggestion_list.set_visible(!suggestions.is_empty());

        for suggestion in suggestions {
            let row = suggestion_row(
                suggestion,
                self.timer_sender.clone(),
                self.navigation.clone(),
                self.suggestion_engine.clone(),
                self.suggestion_dirty.clone(),
            );
            self.suggestion_rows
                .borrow_mut()
                .insert(suggestion.id.clone(), row.clone());
            self.suggestion_list.append(&row);
        }
    }

    pub fn handle_completion(&self, completion: &ActionCompletion) -> bool {
        if let Some(id) = completion.origin.strip_prefix("suggestion-action|")
            && let Some(row) = self.suggestion_rows.borrow().get(id)
        {
            row.remove_css_class("is-pending");
            row.add_css_class(match completion.result {
                ActionResult::Completed => "is-success",
                ActionResult::Failed { .. } => "is-failure",
            });
            let row = row.clone();
            glib::timeout_add_local_once(Duration::from_millis(720), move || {
                row.remove_css_class("is-success");
                row.remove_css_class("is-failure");
            });
            return true;
        }
        let Some(focus) = focus_from_origin(&completion.origin) else {
            return false;
        };
        match &completion.result {
            ActionResult::Completed => self.errors.borrow_mut().retry(focus),
            ActionResult::Failed { detail, .. } => {
                self.errors.borrow_mut().record_failure(focus, detail);
                if completion.origin.ends_with(":open-luma") {
                    self.present();
                }
            }
        }
        self.navigation.finish_action(focus);
        true
    }
}

impl Drop for ControlCenterView {
    fn drop(&mut self) {
        self.motion.destroy();
    }
}

fn toggle_tile(
    icon_name: &str,
    title: &str,
    focus: ControlCenterFocus,
    navigation: &NavigationUi,
) -> ToggleTile {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    root.add_css_class("quick-tile");
    root.add_css_class(focus.as_str());
    let body = tile_body(icon_name, title);
    connect_navigation(&body.button, focus, navigation);
    let toggle = gtk::Switch::new();
    toggle.set_valign(gtk::Align::Center);
    root.append(&body.button);
    root.append(&toggle);
    ToggleTile {
        root,
        icon: body.icon,
        title: body.title,
        detail: body.detail,
        toggle,
    }
}

fn action_tile(
    icon_name: &str,
    title: &str,
    focus: ControlCenterFocus,
    navigation: &NavigationUi,
) -> ActionTile {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    root.add_css_class("quick-tile");
    root.add_css_class(focus.as_str());
    let body = tile_body(icon_name, title);
    connect_navigation(&body.button, focus, navigation);
    let action = icon_button("view-refresh-symbolic", "Cycle power profile");
    action.add_css_class("tile-action");
    root.append(&body.button);
    root.append(&action);
    ActionTile {
        root,
        icon: body.icon,
        title: body.title,
        detail: body.detail,
        action,
    }
}

fn tile_body(icon_name: &str, title: &str) -> TileBody {
    let button = gtk::Button::new();
    button.add_css_class("quick-tile-body");
    button.set_has_frame(false);
    button.set_hexpand(true);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.add_css_class("quick-tile-icon");
    row.append(&icon);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
    text.set_hexpand(true);
    let title = gtk::Label::new(Some(title));
    title.set_xalign(0.0);
    let detail = gtk::Label::new(None);
    detail.add_css_class("supporting-text");
    detail.set_xalign(0.0);
    detail.set_max_width_chars(20);
    detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&title);
    text.append(&detail);
    row.append(&text);
    button.set_child(Some(&row));
    TileBody {
        button,
        icon,
        title,
        detail,
    }
}

fn connect_navigation(button: &gtk::Button, page: ControlCenterFocus, navigation: &NavigationUi) {
    let navigation = navigation.clone();
    button.connect_clicked(move |button| {
        let origin = button.root().and_then(|root| button.compute_bounds(&root));
        match origin {
            Some(bounds) => navigation.navigate_from(
                page,
                MorphRect::new(bounds.x(), bounds.y(), bounds.width(), bounds.height()),
            ),
            None => navigation.navigate(page, false),
        }
    });
}

fn section_eyebrow(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("section-eyebrow");
    label.set_xalign(0.0);
    label
}

/// Desktop-wide modes that otherwise only exist as scripts: monitor layouts
/// and the night light (Hyprland only) and regenerating the wallpaper theme.
fn desktop_mode_row(actions: ActionHandle) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.add_css_class("summary-grid");
    row.add_css_class("desktop-mode-row");
    row.set_homogeneous(true);
    let hyprland = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some();
    let mut tiles = Vec::new();
    if hyprland {
        tiles.push((
            "video-display-symbolic",
            "Everyday",
            "Layout",
            "display-default",
            ActionIntent::SetDisplayMode {
                mode: "default".to_string(),
            },
        ));
        tiles.push((
            "input-gaming-symbolic",
            "Game",
            "Layout",
            "display-game",
            ActionIntent::SetDisplayMode {
                mode: "game".to_string(),
            },
        ));
        tiles.push((
            "night-light-symbolic",
            "Night light",
            "Toggle",
            "night-light",
            ActionIntent::ToggleNightLight,
        ));
    }
    tiles.push((
        "preferences-desktop-wallpaper-symbolic",
        "Theme",
        "Regenerate",
        "reapply-theme",
        ActionIntent::ReapplyTheme,
    ));
    for (icon_name, title, value, action, intent) in tiles {
        let (button, value_label) = summary_tile(icon_name, title);
        value_label.set_label(value);
        let actions = actions.clone();
        button.connect_clicked(move |_| actions.send(action, intent.clone()));
        row.append(&button);
    }
    row
}

fn summary_tile(icon_name: &str, title: &str) -> (gtk::Button, gtk::Label) {
    let button = gtk::Button::new();
    button.add_css_class("summary-tile");
    button.set_has_frame(false);
    button.set_hexpand(true);
    let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.add_css_class("summary-icon");
    let title = gtk::Label::new(Some(title));
    title.add_css_class("summary-label");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    header.append(&icon);
    header.append(&title);
    let value = gtk::Label::new(None);
    value.add_css_class("summary-value");
    value.set_xalign(0.0);
    value.set_max_width_chars(12);
    value.set_ellipsize(gtk::pango::EllipsizeMode::End);
    column.append(&header);
    column.append(&value);
    button.set_child(Some(&column));
    (button, value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NotificationPresentation {
    Toast { stack_width: u32 },
    Inbox,
}

pub(super) fn notification_row(
    notification: &Notification,
    control: Option<&NotificationControl>,
    presentation: NotificationPresentation,
) -> gtk::Box {
    let compact = matches!(presentation, NotificationPresentation::Toast { .. });
    let attachment = notification_attachment(notification, presentation);
    let has_avatar = attachment
        .as_ref()
        .is_some_and(|attachment| attachment.kind == NotificationImagePresentation::Avatar);
    let has_media = attachment.as_ref().is_some_and(|attachment| {
        matches!(
            attachment.kind,
            NotificationImagePresentation::WideMedia | NotificationImagePresentation::PortraitMedia
        )
    });
    let expanded = has_media
        || notification.progress.is_some()
        || (notification.actionable && !notification.actions.is_empty());

    let row = gtk::Box::new(gtk::Orientation::Vertical, 7);
    row.add_css_class("notification-card");
    row.add_css_class(match notification.urgency {
        NotificationUrgency::Low => "urgency-low",
        NotificationUrgency::Normal => "urgency-normal",
        NotificationUrgency::Critical => "urgency-critical",
    });
    if compact {
        row.add_css_class("notification-toast");
        if let Some(control) = control.cloned() {
            let hover = gtk::EventControllerMotion::new();
            let enter_control = control.clone();
            let id = notification.id;
            hover.connect_enter(move |_, _, _| {
                let _ = enter_control.set_hovered(id, true);
            });
            hover.connect_leave(move |_| {
                let _ = control.set_hovered(id, false);
            });
            row.add_controller(hover);
        }
    } else {
        row.add_css_class("notification-inbox-card");
    }
    if expanded {
        row.add_css_class("notification-expanded");
    }
    if has_media {
        row.add_css_class("has-media");
    }
    if notification.progress.is_some() {
        row.add_css_class("has-progress");
    }
    if notification.actionable && !notification.actions.is_empty() {
        row.add_css_class("has-actions");
    }
    row.set_accessible_role(gtk::AccessibleRole::Group);
    row.update_property(&[gtk::accessible::Property::Label(&format!(
        "{}: {}",
        notification.app_name, notification.summary
    ))]);

    let copy = gtk::Box::new(gtk::Orientation::Vertical, 4);
    copy.add_css_class("notification-copy");
    copy.set_hexpand(true);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    if !has_avatar {
        let icon = notification_icon(notification);
        icon.set_pixel_size(if compact { 22 } else { 28 });
        header.append(&icon);
    }
    let app = gtk::Label::new(Some(&notification.app_name));
    app.add_css_class("notification-app");
    app.set_xalign(0.0);
    app.set_hexpand(true);
    app.set_ellipsize(gtk::pango::EllipsizeMode::End);
    header.append(&app);
    if notification.duplicate_count > 1 {
        let duplicate = gtk::Label::new(Some(&format!("×{}", notification.duplicate_count)));
        duplicate.add_css_class("notification-duplicate");
        duplicate.set_tooltip_text(Some(&format!(
            "{} similar notifications",
            notification.duplicate_count
        )));
        header.append(&duplicate);
    }
    if matches!(presentation, NotificationPresentation::Inbox) {
        let age = gtk::Label::new(Some(&notification_age_label(
            notification.received_at,
            current_epoch_seconds(),
        )));
        age.add_css_class("notification-age");
        header.append(&age);
    }
    let dismiss = icon_button("window-close-symbolic", "Dismiss notification");
    dismiss.add_css_class("notification-dismiss");
    if let Some(control) = control.cloned() {
        let id = notification.id;
        dismiss.connect_clicked(move |_| {
            let _ = control.dismiss(id);
        });
    } else {
        dismiss.set_sensitive(false);
    }
    header.append(&dismiss);
    copy.append(&header);

    let summary = gtk::Label::new(Some(&notification.summary));
    summary.add_css_class("notification-summary");
    summary.set_xalign(0.0);
    summary.set_wrap(true);
    summary.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    summary.set_lines(if compact { 2 } else { 3 });
    summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
    copy.append(&summary);

    if !notification.body.is_empty() {
        let body = gtk::Label::new(None);
        body.add_css_class("notification-body");
        body.set_xalign(0.0);
        body.set_wrap(true);
        body.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        body.set_lines(if compact {
            if expanded { 3 } else { 2 }
        } else {
            8
        });
        body.set_ellipsize(gtk::pango::EllipsizeMode::End);
        if gtk::pango::parse_markup(&notification.body, '\0').is_ok() {
            body.set_markup(&notification.body);
            body.connect_activate_link(|_, uri| {
                if matches!(uri.split(':').next(), Some("http" | "https" | "mailto")) {
                    let _ = gtk::gio::AppInfo::launch_default_for_uri(
                        uri,
                        None::<&gtk::gio::AppLaunchContext>,
                    );
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
        } else {
            body.set_label(&notification.body);
        }
        copy.append(&body);
    }

    if has_avatar {
        let main = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        main.add_css_class("notification-main");
        let avatar_frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        avatar_frame.add_css_class("notification-avatar-frame");
        avatar_frame.set_halign(gtk::Align::Start);
        avatar_frame.set_valign(gtk::Align::Start);
        avatar_frame.set_overflow(gtk::Overflow::Hidden);
        let avatar_size = if compact { 40 } else { 48 };
        avatar_frame.set_size_request(avatar_size, avatar_size);
        if let Some(attachment) = attachment.as_ref() {
            attachment.picture.add_css_class("notification-avatar");
            avatar_frame.append(&attachment.picture);
        }
        main.append(&avatar_frame);
        main.append(&copy);
        row.append(&main);
    } else {
        row.append(&copy);
    }

    if let Some(attachment) = attachment.filter(|attachment| {
        matches!(
            attachment.kind,
            NotificationImagePresentation::WideMedia | NotificationImagePresentation::PortraitMedia
        )
    }) {
        attachment.picture.add_css_class("notification-image");
        row.append(&attachment.picture);
    }
    if let Some(progress) = notification.progress {
        let progress_bar = gtk::ProgressBar::new();
        progress_bar.set_fraction(f64::from(progress) / 100.0);
        progress_bar.set_text(Some(&format!("{progress}%")));
        progress_bar.set_show_text(true);
        row.append(&progress_bar);
    }

    if notification.actionable && !notification.actions.is_empty() {
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        actions.add_css_class("notification-actions");
        actions.set_homogeneous(true);
        for action in notification.actions.iter().take(3) {
            let button = gtk::Button::new();
            button.add_css_class("notification-action");
            button.set_tooltip_text(Some(&action.label));
            let label = gtk::Label::new(Some(&action.label));
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(16);
            button.set_child(Some(&label));
            if let Some(control) = control.cloned() {
                let id = notification.id;
                let key = action.key.clone();
                button.connect_clicked(move |_| {
                    let _ = control.invoke(id, key.clone());
                });
            } else {
                button.set_sensitive(false);
            }
            actions.append(&button);
        }
        row.append(&actions);
    }
    row
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NotificationImagePresentation {
    Avatar,
    WideMedia,
    PortraitMedia,
}

struct NotificationAttachment {
    picture: gtk::Picture,
    kind: NotificationImagePresentation,
}

fn notification_attachment(
    notification: &Notification,
    presentation: NotificationPresentation,
) -> Option<NotificationAttachment> {
    let path = notification
        .image_path
        .as_ref()
        .filter(|path| path.is_file())?;
    let image = image::open(path).ok()?;
    let kind = notification_image_presentation(
        i32::try_from(image.width()).ok()?,
        i32::try_from(image.height()).ok()?,
    );
    if matches!(presentation, NotificationPresentation::Toast { .. })
        && kind == NotificationImagePresentation::PortraitMedia
    {
        return None;
    }
    let (width, height, cover) = match (presentation, kind) {
        (NotificationPresentation::Toast { .. }, NotificationImagePresentation::Avatar) => {
            (40, 40, true)
        }
        (NotificationPresentation::Inbox, NotificationImagePresentation::Avatar) => (48, 48, true),
        (
            NotificationPresentation::Toast { stack_width },
            NotificationImagePresentation::WideMedia,
        ) => {
            let media_width = stack_width.saturating_sub(28).max(1);
            (media_width, 120, true)
        }
        (NotificationPresentation::Inbox, NotificationImagePresentation::WideMedia) => {
            (410, 180, true)
        }
        (NotificationPresentation::Inbox, NotificationImagePresentation::PortraitMedia) => {
            (410, 180, false)
        }
        (NotificationPresentation::Toast { .. }, NotificationImagePresentation::PortraitMedia) => {
            return None;
        }
    };
    Some(NotificationAttachment {
        picture: scaled_notification_picture(image, width, height, cover),
        kind,
    })
}

fn notification_image_presentation(width: i32, height: i32) -> NotificationImagePresentation {
    if width <= 0 || height <= 0 {
        return NotificationImagePresentation::PortraitMedia;
    }
    let shorter = i64::from(width.min(height));
    let longer = i64::from(width.max(height));
    if shorter * 5 >= longer * 4 {
        NotificationImagePresentation::Avatar
    } else if i64::from(width) * 10 >= i64::from(height) * 13 {
        NotificationImagePresentation::WideMedia
    } else {
        NotificationImagePresentation::PortraitMedia
    }
}

fn scaled_notification_picture(
    image: image::DynamicImage,
    width: u32,
    height: u32,
    cover: bool,
) -> gtk::Picture {
    let rendered = if cover {
        image.resize_to_fill(width, height, image::imageops::FilterType::Lanczos3)
    } else {
        image.thumbnail(width, height)
    };
    let rgba = rendered.into_rgba8();
    let texture_width = i32::try_from(rgba.width()).unwrap_or(i32::MAX);
    let texture_height = i32::try_from(rgba.height()).unwrap_or(i32::MAX);
    let stride = usize::try_from(rgba.width()).unwrap_or_default() * 4;
    let bytes = gtk::glib::Bytes::from_owned(rgba.into_raw());
    let texture = gtk::gdk::MemoryTexture::new(
        texture_width,
        texture_height,
        gtk::gdk::MemoryFormat::R8g8b8a8,
        &bytes,
        stride,
    );
    gtk::Picture::for_paintable(&texture)
}

fn notification_age_label(received_at: i64, now_epoch: i64) -> String {
    let elapsed = now_epoch.saturating_sub(received_at).max(0);
    match elapsed {
        0..=59 => "now".to_string(),
        60..=3_599 => format!("{}m", elapsed / 60),
        3_600..=86_399 => format!("{}h", elapsed / 3_600),
        _ => format!("{}d", elapsed / 86_400),
    }
}

fn notification_icon(notification: &Notification) -> gtk::Image {
    if let Some(icon) = notification.app_icon.as_deref() {
        let path = icon
            .strip_prefix("file://")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(icon));
        if path.is_absolute() && path.is_file() {
            return gtk::Image::from_file(path);
        }
        if !icon.contains('/') {
            return gtk::Image::from_icon_name(icon);
        }
    }
    gtk::Image::from_icon_name("preferences-system-notifications-symbolic")
}

fn suggestion_row(
    suggestion: &Suggestion,
    action_sender: Sender<ActionRequest>,
    navigation: NavigationUi,
    suggestion_engine: Rc<RefCell<SuggestionEngine>>,
    suggestion_dirty: Rc<super::surface::ContextDirty>,
) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("suggestion-card");
    row.update_property(&[
        gtk::accessible::Property::Label(&suggestion.title),
        gtk::accessible::Property::Description(&suggestion.reason),
    ]);

    let icon = gtk::Image::from_icon_name(&suggestion.icon_name);
    icon.add_css_class("suggestion-card-icon");
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
    copy.set_hexpand(true);
    let title = gtk::Label::new(Some(&suggestion.title));
    title.add_css_class("suggestion-card-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let reason = gtk::Label::new(Some(&suggestion.reason));
    reason.add_css_class("supporting-text");
    reason.set_xalign(0.0);
    reason.set_ellipsize(gtk::pango::EllipsizeMode::End);
    copy.append(&title);
    copy.append(&reason);

    let action = gtk::Button::with_label(&suggestion.action_label);
    action.add_css_class("suggestion-card-action");
    action.set_tooltip_text(Some(&suggestion.reason));
    let dismiss = icon_button("window-close-symbolic", "Dismiss suggestion");
    dismiss.add_css_class("suggestion-card-dismiss");

    {
        let suggestion = suggestion.clone();
        let row = row.clone();
        action.connect_clicked(move |_| match suggestion.action.clone() {
            SuggestionAction::Intent(intent) => {
                row.add_css_class("is-pending");
                let _ = action_sender.send(ActionRequest {
                    origin: format!("suggestion-action|{}", suggestion.id),
                    intent,
                });
            }
            SuggestionAction::OpenControlCenter(context) => {
                navigation.navigate(context.into(), false);
            }
        });
    }
    {
        let id = suggestion.id.clone();
        dismiss.connect_clicked(move |_| {
            if suggestion_engine
                .borrow_mut()
                .dismiss(&id, current_epoch_seconds())
            {
                suggestion_dirty.set(true);
            }
        });
    }

    row.append(&icon);
    row.append(&copy);
    row.append(&action);
    row.append(&dismiss);
    row
}

fn detail_hero(icon_name: &str, title: &str) -> (gtk::Box, gtk::Label, gtk::Label) {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    root.add_css_class("detail-hero");
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.add_css_class("detail-hero-icon");
    root.append(&icon);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
    text.set_hexpand(true);
    let eyebrow = gtk::Label::new(Some(title));
    eyebrow.add_css_class("detail-eyebrow");
    eyebrow.set_xalign(0.0);
    let state = gtk::Label::new(None);
    state.add_css_class("detail-hero-title");
    state.set_xalign(0.0);
    state.set_max_width_chars(40);
    state.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let detail = gtk::Label::new(None);
    detail.add_css_class("supporting-text");
    detail.set_xalign(0.0);
    detail.set_max_width_chars(40);
    detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&eyebrow);
    text.append(&state);
    text.append(&detail);
    root.append(&text);
    (root, state, detail)
}

fn section_intro(icon_name: &str, title: &str, detail: &str) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    root.add_css_class("detail-hero");
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.add_css_class("detail-hero-icon");
    root.append(&icon);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
    let title = gtk::Label::new(Some(title));
    title.add_css_class("detail-hero-title");
    title.set_xalign(0.0);
    let detail = gtk::Label::new(Some(detail));
    detail.add_css_class("supporting-text");
    detail.set_xalign(0.0);
    text.append(&title);
    text.append(&detail);
    root.append(&text);
    root
}

fn metric_row(title: &str) -> MetricGauge {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 8);
    row.add_css_class("metric-card");

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.add_css_class("metric-header");
    let title = gtk::Label::new(Some(title));
    title.set_xalign(0.0);
    title.set_hexpand(true);
    let value = gtk::Label::new(None);
    value.add_css_class("metric-value");
    value.set_xalign(1.0);
    header.append(&title);
    header.append(&value);
    row.append(&header);

    let track = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    track.add_css_class("metric-gauge");
    track.set_homogeneous(true);
    let segments = (0..METRIC_SEGMENTS)
        .map(|_| {
            let segment = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            segment.add_css_class("metric-segment");
            segment.set_hexpand(true);
            track.append(&segment);
            segment
        })
        .collect();
    row.append(&track);

    MetricGauge {
        root: row,
        value,
        segments,
    }
}

fn reconcile_keyboard_layouts(
    list: &gtk::Box,
    previous: &mut Vec<KeyboardLayoutControlSpec>,
    layouts: &[KeyboardLayoutControlSpec],
    sender: Sender<ActionRequest>,
    errors: Rc<RefCell<ControlCenterErrors>>,
    navigation: NavigationUi,
) {
    if previous == layouts {
        return;
    }
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    *previous = layouts.to_vec();

    if layouts.is_empty() {
        let empty = gtk::Label::new(Some("No keyboard layouts available"));
        empty.add_css_class("empty-state");
        empty.set_xalign(0.0);
        list.append(&empty);
        return;
    }

    let handle = ActionHandle {
        sender,
        errors,
        navigation,
        focus: ControlCenterFocus::Keyboard,
    };
    for layout in layouts {
        let row = gtk::Button::new();
        row.add_css_class("keyboard-layout-row");
        row.set_has_frame(false);
        row.set_hexpand(true);
        row.set_tooltip_text(Some(&format!("Switch to {}", layout.raw_name)));
        if layout.selected {
            row.add_css_class("selected");
        }

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let badge = gtk::Label::new(Some(&layout.title));
        badge.add_css_class("keyboard-layout-badge");
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        let title = gtk::Label::new(Some(&layout.title));
        title.add_css_class("keyboard-layout-title");
        title.set_xalign(0.0);
        let detail = gtk::Label::new(Some(&layout.detail));
        detail.add_css_class("keyboard-layout-detail");
        detail.set_xalign(0.0);
        text.append(&title);
        text.append(&detail);
        let selected = gtk::Image::from_icon_name("object-select-symbolic");
        selected.add_css_class("keyboard-layout-selected");
        selected.set_visible(layout.selected);
        content.append(&badge);
        content.append(&text);
        content.append(&selected);
        row.set_child(Some(&content));

        if !layout.selected {
            let handle = handle.clone();
            let index = layout.index;
            row.connect_clicked(move |_| {
                handle.send(
                    "select-layout",
                    ActionIntent::SelectKeyboardLayout { index },
                );
            });
        }
        list.append(&row);
    }
}

fn icon_button(icon_name: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon_name);
    button.set_tooltip_text(Some(tooltip));
    button.set_has_frame(false);
    button
}

fn control_scale() -> gtk::Scale {
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    scale.set_draw_value(false);
    scale.set_hexpand(true);
    scale
}

fn connect_toggle_controls<'a, I, F>(
    toggles: I,
    suppress: Rc<Cell<bool>>,
    current: Rc<RefCell<ControlCenterSpec>>,
    handle: ActionHandle,
    intent: F,
) where
    I: IntoIterator<Item = &'a gtk::Switch>,
    F: Fn(&ControlCenterSpec, bool) -> Option<ActionIntent> + Clone + 'static,
{
    for toggle in toggles {
        let suppress = suppress.clone();
        let current = current.clone();
        let handle = handle.clone();
        let intent = intent.clone();
        toggle.connect_active_notify(move |toggle| {
            if suppress.get() {
                return;
            }
            if let Some(intent) = intent(&current.borrow(), toggle.is_active()) {
                handle.send("toggle", intent);
            }
        });
    }
}

fn live_percent_label(value: f64) -> String {
    format!("{}%", value.round().clamp(0.0, 100.0) as u8)
}

fn install_percent_debounce<F>(
    scale: &gtk::Scale,
    value_label: gtk::Label,
    suppress: Rc<Cell<bool>>,
    handle: ActionHandle,
    action: &'static str,
    intent: F,
) where
    F: Fn(u8) -> ActionIntent + Clone + 'static,
{
    let pending = Rc::new(RefCell::new(None::<glib::SourceId>));
    scale.connect_value_changed(move |scale| {
        value_label.set_label(&live_percent_label(scale.value()));
        if suppress.get() {
            return;
        }
        handle.errors.borrow_mut().retry(handle.focus);
        handle.navigation.render_error();
        if let Some(source) = pending.borrow_mut().take() {
            source.remove();
        }
        let percent = (scale.value().round() as u8).min(100);
        let handle = handle.clone();
        let pending_for_timeout = pending.clone();
        let intent = intent.clone();
        *pending.borrow_mut() = Some(glib::timeout_add_local_once(
            Duration::from_millis(150),
            move || {
                handle.send(action, intent(percent));
                pending_for_timeout.borrow_mut().take();
            },
        ));
    });
}

fn update_toggle_tile(tile: &ToggleTile, spec: &QuickControlSpec) {
    tile.icon.set_icon_name(Some(&spec.icon_name));
    tile.title.set_label(&spec.label);
    tile.detail.set_label(&spec.detail);
    tile.root.set_visible(spec.available);
    tile.root.set_sensitive(spec.available);
    tile.toggle.set_active(spec.enabled);
    tile.toggle.set_visible(spec.toggle_available);
    tile.toggle.set_sensitive(spec.toggle_available);
    set_enabled_class(&tile.root, spec.enabled);
}

fn update_action_tile(tile: &ActionTile, spec: &QuickControlSpec) {
    tile.icon.set_icon_name(Some(&spec.icon_name));
    tile.title.set_label(&spec.label);
    tile.detail.set_label(&spec.detail);
    tile.root.set_visible(spec.available);
    tile.root.set_sensitive(spec.available);
    set_enabled_class(&tile.root, spec.enabled);
}

fn quick_grid_placements(count: usize) -> Vec<(i32, i32, i32)> {
    (0..count)
        .map(|index| {
            let column = i32::try_from(index % 2).unwrap_or_default();
            let row = i32::try_from(index / 2).unwrap_or_default();
            let width = if count % 2 == 1 && index + 1 == count {
                2
            } else {
                1
            };
            (column, row, width)
        })
        .collect()
}

fn reflow_quick_grid<'a>(grid: &gtk::Grid, tiles: impl IntoIterator<Item = (&'a gtk::Box, bool)>) {
    while let Some(child) = grid.first_child() {
        grid.remove(&child);
    }
    let tiles = tiles
        .into_iter()
        .filter_map(|(tile, visible)| visible.then_some(tile))
        .collect::<Vec<_>>();
    let placements = quick_grid_placements(tiles.len());
    for (tile, (column, row, width)) in tiles.into_iter().zip(placements) {
        grid.attach(tile, column, row, width, 1);
    }
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn update_metric(gauge: &MetricGauge, percent: Option<u8>) {
    let visual = metric_visual(percent);
    gauge.value.set_label(&visual.label);

    for class in ["warning", "critical", "unavailable"] {
        gauge.root.remove_css_class(class);
    }
    match visual.level {
        MetricLevel::Normal => {}
        MetricLevel::Warning => gauge.root.add_css_class("warning"),
        MetricLevel::Critical => gauge.root.add_css_class("critical"),
        MetricLevel::Unavailable => gauge.root.add_css_class("unavailable"),
    }

    for (index, segment) in gauge.segments.iter().enumerate() {
        if index < visual.active_segments {
            segment.add_css_class("active");
        } else {
            segment.remove_css_class("active");
        }
    }
}

fn metric_visual(percent: Option<u8>) -> MetricVisual {
    let Some(percent) = percent else {
        return MetricVisual {
            active_segments: 0,
            level: MetricLevel::Unavailable,
            label: "Unavailable".to_string(),
        };
    };

    let percent = percent.min(100);
    let active_segments = if percent == 0 {
        0
    } else {
        usize::from(percent).div_ceil(100 / METRIC_SEGMENTS)
    };
    let level = match percent {
        90..=100 => MetricLevel::Critical,
        75..=89 => MetricLevel::Warning,
        _ => MetricLevel::Normal,
    };

    MetricVisual {
        active_segments,
        level,
        label: format!("{percent}%"),
    }
}

fn percent_label(percent: Option<u8>) -> String {
    percent.map_or_else(|| "--".to_string(), |value| value.to_string())
}

fn set_enabled_class(widget: &impl IsA<gtk::Widget>, enabled: bool) {
    if enabled {
        widget.add_css_class("enabled");
    } else {
        widget.remove_css_class("enabled");
    }
}

#[cfg(test)]
mod tests {
    use gtk4::glib;

    use crate::{
        AudioOutputState, AudioState, BarSnapshot, BluetoothDeviceOperation, BluetoothDeviceState,
        BluetoothState, BrightnessState, CalendarAgendaEvent, ConnectivityState,
        KeyboardLayoutOption, KeyboardLayoutState, MediaState, NetworkState, PlaybackStatus,
        PowerProfile, PowerState, ResourceState, TimerState,
    };

    use super::{
        ControlCenterErrors, ControlCenterFocus, ControlCenterMotionEvent,
        ControlCenterMotionPhase, MetricLevel, NotificationImagePresentation, SliderDebounce,
        TimerActionState, TimerControlSpec, build_control_center_spec, control_center_origin,
        events_for_local_day, focus_from_origin, format_duration, format_network_rate,
        live_percent_label, metric_visual, notification_age_label, notification_image_presentation,
        power_page_spec, quick_grid_placements, timer_action_state, volume_icon_name,
    };

    #[test]
    fn desktop_power_page_shows_profile_hero_and_cpu_readings() {
        let power = PowerState {
            profile: PowerProfile::Balanced,
            available_profiles: vec![PowerProfile::PowerSaver, PowerProfile::Balanced],
            stats: crate::PowerStats {
                cpu_temp_celsius: Some(84),
                cpu_freq_mhz: Some(3_390),
                cpu_max_freq_mhz: Some(4_950),
                cpu_energy_preference: Some("balance_power".to_string()),
                ..crate::PowerStats::default()
            },
            ..PowerState::default()
        };

        let spec = power_page_spec(&power);

        assert_eq!(spec.hero_eyebrow, "Power profile");
        assert_eq!(spec.hero_title, "Balanced");
        let availability = spec
            .profiles
            .iter()
            .map(|option| (option.available, option.selected))
            .collect::<Vec<_>>();
        assert_eq!(availability, [(true, false), (true, true), (false, false)]);
        let stats = spec
            .stats
            .iter()
            .map(|stat| (stat.id, stat.value.as_str(), stat.level))
            .collect::<Vec<_>>();
        assert_eq!(
            stats,
            [
                ("cpu-temp", "84 °C", Some("warning")),
                ("cpu-clock", "3.39 GHz", None),
                ("cpu-bias", "Balanced", None),
            ]
        );
        assert_eq!(
            spec.stats[2].caption.as_deref(),
            Some("leans to efficiency")
        );
    }

    #[test]
    fn laptop_power_page_reports_battery_time_health_and_degradation() {
        let mut power = PowerState {
            battery_present: true,
            battery_percent: Some(64),
            profile: PowerProfile::Performance,
            performance_degraded: Some("lap-detected".to_string()),
            stats: crate::PowerStats {
                energy_rate_mw: Some(12_300),
                time_to_empty_seconds: Some(2 * 3_600 + 5 * 60),
                energy_now_mwh: Some(36_000),
                energy_full_mwh: Some(50_000),
                energy_full_design_mwh: Some(60_000),
                cycle_count: Some(311),
                charge_limit_percent: Some(80),
                ..crate::PowerStats::default()
            },
            ..PowerState::default()
        };

        let spec = power_page_spec(&power);
        assert_eq!(spec.hero_title, "64%");
        assert_eq!(spec.hero_detail, "On battery · 2 h 05 min left");
        assert!(spec.performance_degraded);
        assert_eq!(
            spec.profile_note,
            "Performance limited · device is on a lap"
        );
        assert!(spec.profiles.iter().all(|option| option.available));
        let rate = &spec.stats[0];
        assert_eq!((rate.label, rate.value.as_str()), ("Draw", "12.3 W"));
        assert_eq!(rate.caption.as_deref(), Some("36.0 Wh stored"));
        let health = &spec.stats[1];
        assert_eq!(health.value, "83%");
        assert_eq!(health.caption.as_deref(), Some("50.0 of 60.0 Wh"));
        assert_eq!(spec.stats[2].value, "311");

        power.charging = true;
        power.battery_percent = Some(80);
        assert_eq!(
            power_page_spec(&power).hero_detail,
            "Plugged in · held at 80%"
        );
        power.battery_percent = Some(50);
        power.stats.time_to_full_seconds = Some(40 * 60);
        let charging = power_page_spec(&power);
        assert_eq!(charging.hero_detail, "Charging · 40 min to full");
        assert_eq!(charging.stats[0].label, "Charging at");
    }

    #[test]
    fn durations_switch_to_hours_after_an_hour() {
        assert_eq!(format_duration(59), "1 min");
        assert_eq!(format_duration(59 * 60), "59 min");
        assert_eq!(format_duration(3_600), "1 h 00 min");
        assert_eq!(format_duration(26 * 3_600 + 7 * 60), "26 h 07 min");
    }

    #[test]
    fn gtk_calendar_months_are_zero_based() {
        use super::calendar_month_from_gtk;

        assert_eq!(calendar_month_from_gtk(0), 1);
        assert_eq!(calendar_month_from_gtk(9), 10);
        assert_eq!(calendar_month_from_gtk(11), 12);
    }

    #[test]
    fn panel_stage_origin_falls_back_in_order() {
        use super::panel_origin;
        use crate::ui::morph::MorphRect;
        use crate::ui::morph_bin::MorphOrigin;

        let button = MorphRect::new(2300.0, 9.0, 30.0, 28.0);
        assert_eq!(
            panel_origin(Some(button), 2560, 480),
            MorphOrigin::Local(MorphRect::new(253.0, -45.0, 30.0, 28.0))
        );
        assert_eq!(panel_origin(None, 2560, 480), MorphOrigin::Scaled(0.92));
    }

    #[test]
    fn control_center_motion_reopens_during_exit() {
        let phase = ControlCenterMotionPhase::Visible
            .transition(ControlCenterMotionEvent::Dismiss)
            .transition(ControlCenterMotionEvent::Present);

        assert_eq!(phase, ControlCenterMotionPhase::Entering);
        assert!(phase.is_presented());
    }

    #[test]
    fn control_center_motion_only_completes_matching_phases() {
        assert_eq!(
            ControlCenterMotionPhase::Entering.transition(ControlCenterMotionEvent::Entered),
            ControlCenterMotionPhase::Visible
        );
        assert_eq!(
            ControlCenterMotionPhase::Exiting.transition(ControlCenterMotionEvent::Exited),
            ControlCenterMotionPhase::Hidden
        );
        assert_eq!(
            ControlCenterMotionPhase::Visible.transition(ControlCenterMotionEvent::Exited),
            ControlCenterMotionPhase::Visible
        );
    }

    #[test]
    fn live_percent_label_rounds_and_clamps() {
        assert_eq!(live_percent_label(0.0), "0%");
        assert_eq!(live_percent_label(49.4), "49%");
        assert_eq!(live_percent_label(49.5), "50%");
        assert_eq!(live_percent_label(100.0), "100%");
        assert_eq!(live_percent_label(100.4), "100%");
        assert_eq!(live_percent_label(-1.0), "0%");
        assert_eq!(live_percent_label(150.0), "100%");
    }

    #[test]
    fn volume_icons_follow_mute_and_level_boundaries() {
        assert_eq!(
            volume_icon_name(true, Some(80)),
            "audio-volume-muted-symbolic"
        );
        assert_eq!(
            volume_icon_name(false, Some(0)),
            "audio-volume-muted-symbolic"
        );
        assert_eq!(
            volume_icon_name(false, Some(1)),
            "audio-volume-low-symbolic"
        );
        assert_eq!(
            volume_icon_name(false, Some(33)),
            "audio-volume-low-symbolic"
        );
        assert_eq!(
            volume_icon_name(false, Some(34)),
            "audio-volume-medium-symbolic"
        );
        assert_eq!(
            volume_icon_name(false, Some(66)),
            "audio-volume-medium-symbolic"
        );
        assert_eq!(
            volume_icon_name(false, Some(67)),
            "audio-volume-high-symbolic"
        );
    }

    #[test]
    fn notification_images_use_avatar_layout_only_for_near_square_shapes() {
        for (width, height) in [(64, 64), (320, 320), (167, 167), (80, 100), (125, 100)] {
            assert_eq!(
                notification_image_presentation(width, height),
                NotificationImagePresentation::Avatar,
                "{width}x{height}"
            );
        }
        for (width, height) in [(1600, 900), (130, 100), (1920, 1080)] {
            assert_eq!(
                notification_image_presentation(width, height),
                NotificationImagePresentation::WideMedia,
                "{width}x{height}"
            );
        }
        for (width, height) in [(900, 1600), (79, 100), (126, 100), (0, 64), (64, -1)] {
            assert_eq!(
                notification_image_presentation(width, height),
                NotificationImagePresentation::PortraitMedia,
                "{width}x{height}"
            );
        }
    }

    #[test]
    fn notification_age_uses_compact_relative_buckets() {
        assert_eq!(notification_age_label(1_000, 1_000), "now");
        assert_eq!(notification_age_label(1_000, 1_059), "now");
        assert_eq!(notification_age_label(1_000, 1_060), "1m");
        assert_eq!(notification_age_label(1_000, 4_600), "1h");
        assert_eq!(notification_age_label(1_000, 87_400), "1d");
        assert_eq!(notification_age_label(2_000, 1_000), "now");
    }

    #[test]
    fn network_rates_use_compact_binary_units() {
        assert_eq!(format_network_rate(None), "--");
        assert_eq!(format_network_rate(Some(512)), "512 B/s");
        assert_eq!(format_network_rate(Some(1536)), "1.5 KB/s");
        assert_eq!(format_network_rate(Some(12 * 1024)), "12 KB/s");
        assert_eq!(format_network_rate(Some(3 * 1024 * 1024)), "3.0 MB/s");
    }

    #[test]
    fn selected_day_includes_overlapping_events_and_sorts_all_day_first() {
        let day = glib::DateTime::from_local(2027, 1, 20, 0, 0, 0.0).unwrap();
        let previous = day.add_days(-1).unwrap();
        let next = day.add_days(1).unwrap();
        let events = vec![
            CalendarAgendaEvent {
                id: "meeting".to_string(),
                title: "Meeting".to_string(),
                location: None,
                calendar: Some("Work".to_string()),
                start_epoch: day.to_unix() + 10 * 60 * 60,
                end_epoch: day.to_unix() + 11 * 60 * 60,
                all_day: false,
            },
            CalendarAgendaEvent {
                id: "conference".to_string(),
                title: "Conference".to_string(),
                location: None,
                calendar: None,
                start_epoch: previous.to_unix(),
                end_epoch: next.to_unix(),
                all_day: true,
            },
            CalendarAgendaEvent {
                id: "tomorrow".to_string(),
                title: "Tomorrow".to_string(),
                location: None,
                calendar: None,
                start_epoch: next.to_unix() + 60,
                end_epoch: next.to_unix() + 120,
                all_day: false,
            },
        ];

        let selected = events_for_local_day(&events, 2027, 1, 20);
        assert_eq!(
            selected
                .iter()
                .map(|event| event.id.as_str())
                .collect::<Vec<_>>(),
            vec!["conference", "meeting"]
        );
    }

    #[test]
    fn day_event_levels_weight_timed_events_over_all_day_ones() {
        use super::day_event_levels;

        let at = |day: i32, hour: i32| {
            glib::DateTime::from_local(2027, 1, day, hour, 0, 0.0)
                .unwrap()
                .to_unix()
        };
        let event = |id: &str, start: i64, end: i64, all_day: bool| CalendarAgendaEvent {
            id: id.to_string(),
            title: id.to_string(),
            location: None,
            calendar: None,
            start_epoch: start,
            end_epoch: end,
            all_day,
        };
        let events = vec![
            event("trip", at(4, 0), at(7, 0), true),
            event("lecture", at(5, 9), at(5, 11), false),
            event("a", at(12, 9), at(12, 10), false),
            event("b", at(12, 13), at(12, 14), false),
            event("c", at(20, 9), at(20, 10), false),
            event("d", at(20, 11), at(20, 12), false),
            event("e", at(20, 15), at(20, 16), false),
            event("f", at(20, 18), at(20, 19), false),
        ];

        let levels = day_event_levels(&events, 2027, 1);
        assert_eq!(
            levels.into_iter().collect::<Vec<_>>(),
            vec![(4, 1), (5, 1), (6, 1), (12, 2), (20, 3)]
        );
    }

    #[test]
    fn selected_day_heading_names_nearby_days_and_other_years() {
        use super::selected_day_heading;

        let today = (2026, 10, 21);
        assert_eq!(
            selected_day_heading((2026, 10, 21), today),
            "Today · Wednesday, 21 October"
        );
        assert_eq!(
            selected_day_heading((2026, 10, 22), today),
            "Tomorrow · Thursday, 22 October"
        );
        assert_eq!(
            selected_day_heading((2026, 10, 20), today),
            "Yesterday · Tuesday, 20 October"
        );
        assert_eq!(
            selected_day_heading((2026, 11, 1), today),
            "Sunday, 1 November"
        );
        assert_eq!(
            selected_day_heading((2027, 1, 4), today),
            "Monday, 4 January 2027"
        );
    }

    #[test]
    fn overview_spec_combines_controls_metrics_and_optional_content() {
        let mut snapshot = BarSnapshot::default();
        snapshot.system.network = NetworkState {
            connectivity: ConnectivityState::Connected,
            icon_hint: None,
            label: Some("Home".to_string()),
            wifi_available: true,
            ethernet_available: true,
            wifi_enabled: Some(true),
            interface: Some("wlan0".to_string()),
            download_bytes_per_second: Some(1536),
            upload_bytes_per_second: Some(512),
            download_history: vec![0, 512, 1536],
            upload_history: vec![0, 128, 512],
        };
        snapshot.system.bluetooth = BluetoothState {
            available: true,
            powered: true,
            connected_device: Some("Headphones".to_string()),
            audio_device: Some("Headphones".to_string()),
            ..BluetoothState::default()
        };
        snapshot.system.audio = AudioState {
            volume_percent: Some(49),
            muted: false,
            outputs: vec![AudioOutputState {
                name: "bluez_output.headphones".to_string(),
                description: "Robin's Headphones".to_string(),
                alias: Some("Headphones".to_string()),
                port_description: None,
                port_type: Some("Headphones".to_string()),
                bus: Some("bluetooth".to_string()),
                is_default: true,
            }],
        };
        snapshot.system.brightness = BrightnessState {
            device: Some("intel_backlight".to_string()),
            percent: Some(67),
        };
        snapshot.system.resources = ResourceState {
            cpu_percent: Some(14),
            memory_percent: Some(32),
        };
        snapshot.system.power = PowerState {
            battery_present: true,
            battery_percent: Some(82),
            charging: true,
            profile: PowerProfile::Balanced,
            changed_at: 0,
            ..PowerState::default()
        };
        snapshot.system.keyboard_layout = KeyboardLayoutState {
            current_index: Some(0),
            current_name: Some("English (US)".to_string()),
            layouts: vec![
                KeyboardLayoutOption {
                    index: 0,
                    name: "English (US)".to_string(),
                    layout: Some("us".to_string()),
                    variant: None,
                },
                KeyboardLayoutOption {
                    index: 1,
                    name: "English (Dvorak)".to_string(),
                    layout: Some("us".to_string()),
                    variant: Some("dvorak".to_string()),
                },
                KeyboardLayoutOption {
                    index: 2,
                    name: "German (KOY)".to_string(),
                    layout: Some("de".to_string()),
                    variant: Some("koy".to_string()),
                },
            ],
        };
        snapshot.system.media = Some(MediaState {
            player: "spotify".to_string(),
            status: PlaybackStatus::Playing,
            title: Some("Says".to_string()),
            artist: Some("Nils Frahm".to_string()),
            art_url: Some("https://example.test/says.jpg".to_string()),
            changed_at: 0,
        });
        snapshot.system.timers = vec![TimerState {
            id: "timer-1".to_string(),
            label: "Tea".to_string(),
            remaining_seconds: 125,
            target_epoch: Some(125),
            completed: false,
            changed_at: 0,
        }];

        let spec = build_control_center_spec(&snapshot);

        assert_eq!(spec.network.detail, "Home");
        assert!(spec.network.enabled);
        assert_eq!(spec.network_traffic.interface.as_deref(), Some("wlan0"));
        assert_eq!(spec.network_traffic.download_history, vec![0, 512, 1536]);
        assert_eq!(spec.audio.percent, Some(49));
        assert_eq!(spec.audio.detail, "Headphones");
        assert_eq!(spec.audio.outputs[0].icon_name, "audio-headphones-symbolic");
        assert!(spec.audio.outputs[0].selected);
        assert_eq!(spec.brightness.as_ref().unwrap().device, "intel_backlight");
        assert_eq!(spec.cpu_percent, Some(14));
        assert_eq!(spec.memory_percent, Some(32));
        assert_eq!(spec.keyboard.summary, "US");
        assert_eq!(spec.keyboard.current, "US — Standard");
        assert_eq!(spec.keyboard.layouts[1].detail, "Dvorak");
        assert_eq!(spec.keyboard.layouts[2].title, "DE");
        assert_eq!(spec.keyboard.layouts[2].detail, "KOY");
        assert_eq!(spec.battery_percent, Some(82));
        assert!(spec.battery_present);
        assert!(spec.charging);
        assert_eq!(spec.media.as_ref().unwrap().title, "Says");
        assert_eq!(spec.media.as_ref().unwrap().player, "spotify");
        assert_eq!(spec.timers[0].remaining_seconds, 125);
        assert!(!spec.timers[0].paused);
    }

    #[test]
    fn overview_hides_brightness_when_no_real_backlight_is_available() {
        let spec = build_control_center_spec(&BarSnapshot::default());

        assert_eq!(spec.brightness, None);
    }

    #[test]
    fn wifi_tile_describes_wired_connectivity_without_using_connection_name() {
        let mut snapshot = BarSnapshot::default();
        snapshot.system.network = NetworkState {
            connectivity: ConnectivityState::Connected,
            icon_hint: Some("network-wired-symbolic".to_string()),
            label: Some("Ethernet connection 1".to_string()),
            wifi_available: true,
            ethernet_available: true,
            wifi_enabled: Some(true),
            ..NetworkState::default()
        };

        let spec = build_control_center_spec(&snapshot);

        assert_eq!(spec.network.detail, "On · Ethernet active");
    }

    #[test]
    fn desktop_spec_uses_ethernet_and_omits_laptop_only_hardware() {
        let mut snapshot = BarSnapshot::default();
        snapshot.system.network = NetworkState {
            connectivity: ConnectivityState::Connected,
            icon_hint: Some("network-wired-symbolic".to_string()),
            label: Some("Ethernet".to_string()),
            wifi_available: false,
            ethernet_available: true,
            wifi_enabled: None,
            ..NetworkState::default()
        };
        snapshot.system.bluetooth = BluetoothState {
            available: true,
            powered: true,
            ..BluetoothState::default()
        };
        snapshot.system.power.profile = PowerProfile::Performance;

        let spec = build_control_center_spec(&snapshot);

        assert_eq!(spec.network.label, "Ethernet");
        assert_eq!(spec.network.icon_name, "network-wired-symbolic");
        assert_eq!(spec.network.detail, "Ethernet");
        assert!(spec.network.available);
        assert!(spec.network.enabled);
        assert!(!spec.network.toggle_available);
        assert!(spec.bluetooth.available);
        assert!(!spec.battery_present);
        assert_eq!(spec.brightness, None);
        assert_eq!(spec.power.detail, "Performance");
    }

    #[test]
    fn bluetooth_manager_spec_preserves_device_sections_and_progress() {
        let mut snapshot = BarSnapshot::default();
        snapshot.system.bluetooth = BluetoothState {
            available: true,
            powered: true,
            discovering: true,
            devices: vec![
                BluetoothDeviceState {
                    address: "AA:00:00:00:00:01".to_string(),
                    name: "Headphones".to_string(),
                    icon_name: "audio-headphones-symbolic".to_string(),
                    paired: true,
                    trusted: true,
                    connected: true,
                    audio_capable: true,
                    battery_percent: Some(73),
                    ..BluetoothDeviceState::default()
                },
                BluetoothDeviceState {
                    address: "AA:00:00:00:00:02".to_string(),
                    name: "Controller".to_string(),
                    icon_name: "input-gaming-symbolic".to_string(),
                    paired: true,
                    operation: Some(BluetoothDeviceOperation::Connecting),
                    ..BluetoothDeviceState::default()
                },
            ],
            ..BluetoothState::default()
        };

        let spec = build_control_center_spec(&snapshot).bluetooth_manager;

        assert!(spec.discovering);
        assert_eq!(spec.connected_count, 1);
        assert_eq!(spec.devices[0].detail, "Connected · 73% battery");
        assert_eq!(spec.devices[1].detail, "Connecting…");
    }

    #[test]
    fn quick_grid_spans_the_final_tile_when_optional_hardware_is_absent() {
        assert_eq!(quick_grid_placements(0), vec![]);
        assert_eq!(quick_grid_placements(1), vec![(0, 0, 2)]);
        assert_eq!(
            quick_grid_placements(3),
            vec![(0, 0, 1), (1, 0, 1), (0, 1, 2)]
        );
        assert_eq!(
            quick_grid_placements(4),
            vec![(0, 0, 1), (1, 0, 1), (0, 1, 1), (1, 1, 1)]
        );
    }

    #[test]
    fn action_origins_round_trip_every_overview_and_detail_page() {
        for focus in ControlCenterFocus::ALL {
            let origin = control_center_origin(focus, "open");
            assert_eq!(focus_from_origin(&origin), Some(focus));
        }
        assert_eq!(focus_from_origin("window-popover:DP-5:42"), None);
    }

    #[test]
    fn bluetooth_has_independent_action_and_error_routing() {
        let origin = control_center_origin(ControlCenterFocus::Bluetooth, "toggle");
        assert_eq!(
            focus_from_origin(&origin),
            Some(ControlCenterFocus::Bluetooth)
        );

        let mut errors = ControlCenterErrors::default();
        errors.record_failure(ControlCenterFocus::Bluetooth, "bluetoothctl failed");
        assert_eq!(
            errors.message(ControlCenterFocus::Bluetooth),
            Some("bluetoothctl failed")
        );
        assert_eq!(errors.message(ControlCenterFocus::Audio), None);
    }

    #[test]
    fn slider_debounce_keeps_only_the_latest_clamped_value() {
        let mut debounce = SliderDebounce::default();
        debounce.schedule(20);
        debounce.schedule(140);

        assert_eq!(debounce.take(), Some(100));
        assert_eq!(debounce.take(), None);
    }

    #[test]
    fn metric_gauge_rounds_up_segments_and_applies_pressure_thresholds() {
        let cases = [
            (0, 0, MetricLevel::Normal),
            (1, 1, MetricLevel::Normal),
            (74, 8, MetricLevel::Normal),
            (75, 8, MetricLevel::Warning),
            (89, 9, MetricLevel::Warning),
            (90, 9, MetricLevel::Critical),
            (100, 10, MetricLevel::Critical),
            (140, 10, MetricLevel::Critical),
        ];

        for (percent, active_segments, level) in cases {
            let visual = metric_visual(Some(percent));
            assert_eq!(visual.active_segments, active_segments);
            assert_eq!(visual.level, level);
            assert_eq!(visual.label, format!("{}%", percent.min(100)));
        }
    }

    #[test]
    fn unavailable_metric_gauge_has_no_active_segments() {
        let visual = metric_visual(None);

        assert_eq!(visual.active_segments, 0);
        assert_eq!(visual.level, MetricLevel::Unavailable);
        assert_eq!(visual.label, "Unavailable");
    }

    #[test]
    fn countdown_ticks_preserve_the_interactive_timer_action_state() {
        let mut timer = TimerControlSpec {
            id: "timer-1".to_string(),
            label: "Tea".to_string(),
            remaining_seconds: 120,
            completed: false,
            paused: false,
        };
        let initial = timer_action_state(&timer);
        assert_eq!(initial, TimerActionState::Pause);

        timer.remaining_seconds = 119;
        assert_eq!(timer_action_state(&timer), initial);

        timer.paused = true;
        assert_eq!(timer_action_state(&timer), TimerActionState::Resume);
    }

    #[test]
    fn action_errors_are_retained_per_section_until_retry_or_close() {
        let mut errors = ControlCenterErrors::default();
        errors.record_failure(ControlCenterFocus::Network, "nmcli failed");
        errors.record_failure(ControlCenterFocus::Audio, "wpctl failed");

        errors.retry(ControlCenterFocus::Audio);
        assert_eq!(
            errors.message(ControlCenterFocus::Network),
            Some("nmcli failed")
        );
        assert_eq!(errors.message(ControlCenterFocus::Audio), None);

        errors.close();
        assert_eq!(errors.message(ControlCenterFocus::Network), None);
    }
}
