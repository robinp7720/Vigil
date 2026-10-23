pub mod actions;
pub mod activity;
pub mod compositor;
pub mod config;
pub mod context;
pub mod integration;
pub mod ipc;
pub mod model;
pub mod notifications;
pub mod sources;
pub mod state;
pub mod suggestions;
pub mod swipe;
pub mod timers;
pub mod ui;

use std::time::{SystemTime, UNIX_EPOCH};

use std::path::Path;

use anyhow::Result;

pub use actions::{
    ActionBackend, ActionCompletion, ActionRequest, ActionResult, ActionRouter, ProcessSpec,
    SystemActionBackend, spawn_action_worker,
};
pub use activity::ActivityTracker;
pub use compositor::{
    CompositorAction, CompositorAdapter, HyprlandAdapter, NiriAdapter, detect_compositor,
};
pub use config::{
    AmbientMotion, AppConfig, CommandActivityConfig, CommandRule, ExperienceConfig,
    FreshnessConfig, ModuleConfig, ModuleName, MotionStyle, NotificationConfig, ReloadStatus,
    RoutineConfig, RoutineContextDetail, RuntimeConfigReload, ThresholdConfig,
    reload_runtime_config,
};
pub use context::{ContextCard, ContextTier, Dismissals, select_context};
pub use integration::{
    context_snapshot, context_snapshots, intent_for_context_action, lockscreen_snapshot,
};
pub use ipc::{
    ControlClient, ControlRequest, ControlResponse, ControlSocket, OsdContent, OsdKind,
    control_socket_path,
};
pub use model::{
    ActionIntent, ActivityState, ActivityStatus, ActivityUpdate, AiProviderUsage, AiQuota,
    AiQuotaEstimate, AiUsageState, AudioOutputState, AudioState, BarSnapshot,
    BluetoothDeviceOperation, BluetoothDeviceState, BluetoothPairingPrompt,
    BluetoothPairingPromptKind, BluetoothPairingResponse, BluetoothState, BrightnessState,
    CalendarAgenda, CalendarAgendaEvent, CalendarEvent, ClockState, CommandActivity,
    ConnectivityState, ContextAction, ContextActionSpec, ContextHealth, ContextSnapshot,
    DesktopContext, Direction, KeyboardLayoutOption, KeyboardLayoutState, LockscreenSnapshot,
    MediaControlAction, MediaState, NetworkState, Notification, NotificationAction,
    NotificationState, NotificationUrgency, OutputRole, OutputState, PlaybackStatus, PowerProfile,
    PowerState, PowerStats, ResourceState, SourceHealth, SourceId, StateUpdate, SystemState,
    SystemUpdate, TimerState, TokenUsage, WindowState, WorkspaceState,
};
pub use notifications::{NotificationCommand, NotificationControl, spawn_notification_server};
pub use sources::{
    BluetoothCommand, BluetoothControlClient, CalendarMonthRequest, CalendarRecord,
    SourceSupervisor, battery_severity, parse_calendar_agenda_json, parse_calendar_json,
    read_proc_sample, spawn_ai_usage_source, spawn_audio_source, spawn_bluetooth_source,
    spawn_brightness_source, spawn_calendar_agenda_source, spawn_calendar_source,
    spawn_clock_source, spawn_media_source, spawn_network_source, spawn_power_source,
    spawn_resource_source,
};
pub use state::StateStore;
pub use suggestions::{Suggestion, SuggestionAction, SuggestionEngine, SuggestionSource};
pub use timers::{TimerRecord, TimerStore};
pub use ui::{BarApplication, run_preview};

pub fn startup(config: &AppConfig) -> Result<()> {
    if let Some(locale) = config.locale.as_deref() {
        apply_locale_override(locale);
    }
    Ok(())
}

fn apply_locale_override(locale: &str) {
    let Ok(value) = std::ffi::CString::new(locale) else {
        tracing::warn!("locale {locale:?} contains a NUL byte; ignoring override");
        return;
    };
    let applied = unsafe { libc::setlocale(libc::LC_TIME, value.as_ptr()) };
    if applied.is_null() {
        tracing::warn!(
            "locale {locale:?} is not installed on this system; falling back to the system default"
        );
    }
}

pub fn run_test_control_server(requests: usize) -> Result<()> {
    let socket = ControlSocket::bind()?;
    let mut store = TimerStore::load(current_epoch())?;

    for _ in 0..requests {
        socket.serve_once(|request| match request {
            ControlRequest::TimerStart { .. }
            | ControlRequest::TimerPause { .. }
            | ControlRequest::TimerResume { .. }
            | ControlRequest::TimerCancel { .. }
            | ControlRequest::TimerList => Ok(ControlResponse::Timers {
                timers: store.apply(&request, current_epoch())?,
            }),
            ControlRequest::ActivityStart { .. } | ControlRequest::ActivityFinish { .. } => {
                Ok(ControlResponse::Accepted)
            }
            ControlRequest::RoutinesClear
            | ControlRequest::ContextGet { .. }
            | ControlRequest::ContextExecute { .. }
            | ControlRequest::ControlCenterOpen { .. }
            | ControlRequest::ControlCenterToggle { .. }
            | ControlRequest::ControlCenterClose
            | ControlRequest::LockscreenGet
            | ControlRequest::NotificationsGet
            | ControlRequest::NotificationInvoke { .. }
            | ControlRequest::NotificationDismiss { .. }
            | ControlRequest::NotificationsClear
            | ControlRequest::NotificationsSetDnd { .. }
            | ControlRequest::PresentationGet
            | ControlRequest::PresentationSet { .. }
            | ControlRequest::OsdShow { .. }
            | ControlRequest::OsdHide
            | ControlRequest::MediaGet
            | ControlRequest::MediaControl { .. } => Ok(ControlResponse::Error {
                message: "desktop integration requires the running bar UI".to_string(),
            }),
        })?;
    }

    Ok(())
}

pub fn run(config_path: &Path) -> Result<()> {
    let config = AppConfig::load(config_path)?;
    startup(&config)?;
    BarApplication::new(config, config_path)?.run();
    Ok(())
}

fn current_epoch() -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time");
    i64::try_from(now.as_secs()).unwrap_or(i64::MAX)
}
