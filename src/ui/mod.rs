mod ai_usage;
mod artwork;
pub mod context_card;
pub mod control_center;
mod morph;
mod morph_bin;
mod osd;
pub mod popovers;
mod stage;
pub mod surface;
pub mod system;
pub mod theme;
pub mod wm;
mod workspace_pill;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender, TryRecvError},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use tracing::warn;

use crate::swipe::SwipeFeed;
use crate::{
    ActionCompletion, ActionRequest, ActionResult, ActionRouter, ActivityStatus, ActivityTracker,
    ActivityUpdate, AppConfig, CalendarMonthRequest, CommandActivity, ContextAction,
    ControlRequest, ControlResponse, ControlSocket, NotificationControl, ReloadStatus,
    SourceHealth, SourceId, StateStore, StateUpdate, SuggestionEngine, SystemActionBackend,
    SystemUpdate, TimerStore, context_snapshots, detect_compositor, intent_for_context_action,
    reload_runtime_config, spawn_action_worker, spawn_audio_source, spawn_bluetooth_source,
    spawn_brightness_source, spawn_calendar_agenda_source, spawn_clock_source, spawn_media_source,
    spawn_network_source, spawn_notification_server, spawn_power_source, spawn_resource_source,
};

pub use surface::{PrimarySurface, ReducedSurface, SurfaceRegistry, surface_specs};

const UI_TICK_INTERVAL: Duration = Duration::from_millis(50);
const CONTROL_SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(100);
const TIMER_TICK_INTERVAL: Duration = Duration::from_secs(1);
const COMPOSITOR_RECONNECT_INTERVAL: Duration = Duration::from_secs(2);
const CONTROL_UI_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const INITIAL_DRAIN_WINDOW: Duration = Duration::from_millis(150);
static RELOAD_REQUESTED: AtomicBool = AtomicBool::new(false);

pub struct BarApplication {
    application: gtk::Application,
    runtime: RuntimeHandles,
}

struct RuntimeHandles {
    cancelled: Arc<AtomicBool>,
    joins: Vec<JoinHandle<()>>,
}

struct UiRuntime {
    config: AppConfig,
    config_path: PathBuf,
    store: StateStore,
    activity_tracker: ActivityTracker,
    registry: SurfaceRegistry,
    state_rx: Receiver<StateUpdate>,
    completion_rx: Receiver<ActionCompletion>,
    action_tx: Sender<ActionRequest>,
    control_rx: Receiver<UiControlRequest>,
    control_wakeup: Option<UnixDatagram>,
    pending_control_actions: BTreeMap<String, Sender<ControlResponse>>,
    next_control_action_id: u64,
    osd: osd::Osd,
}

struct UiControlRequest {
    request: ControlRequest,
    response_tx: Sender<ControlResponse>,
}

impl BarApplication {
    pub fn new(config: AppConfig, config_path: &Path) -> Result<Self> {
        stage::configure(config.style);
        install_sighup_handler()?;
        let (ui_runtime, runtime) = start_runtime(config, config_path)?;
        let application = gtk::Application::builder()
            .application_id("me.robindecker.Vigil")
            .build();

        let ui_runtime = Rc::new(RefCell::new(Some(ui_runtime)));
        let monitor_dirty = Rc::new(Cell::new(true));

        {
            let ui_runtime = Rc::clone(&ui_runtime);
            let monitor_dirty = Rc::clone(&monitor_dirty);
            application.connect_activate(move |application| {
                let Some(runtime) = ui_runtime.borrow_mut().take() else {
                    monitor_dirty.set(true);
                    return;
                };
                install_ui_loop(application, runtime, Rc::clone(&monitor_dirty));
            });
        }

        {
            let cancelled = Arc::clone(&runtime.cancelled);
            application.connect_shutdown(move |_| {
                cancelled.store(true, Ordering::Relaxed);
            });
        }

        Ok(Self {
            application,
            runtime,
        })
    }

    pub fn run(self) {
        self.application.run_with_args(&["vigil"]);
        self.runtime.shutdown();
    }
}

pub fn run_preview(config_path: &Path, scenario: &str) -> Result<()> {
    let config = AppConfig::load(config_path)?;
    stage::configure(config.style);
    let application = gtk::Application::builder()
        .application_id("me.robindecker.Vigil.Preview")
        .build();
    let registry = Rc::new(RefCell::new(None::<SurfaceRegistry>));
    let registry_for_activate = registry.clone();
    let config_path = config_path.to_path_buf();
    let scenario = scenario.to_string();

    application.connect_activate(move |application| {
        let mut preview_config = config.clone();
        let Some(display) = gtk::gdk::Display::default() else {
            warn!("preview requires a graphical Wayland session");
            application.quit();
            return;
        };
        if let Err(error) = theme::load_css(&display, &config_path) {
            warn!("preview theme unavailable: {error:#}");
        }
        if scenario == "reduced-motion"
            && let Some(settings) = gtk::Settings::default()
        {
            settings.set_gtk_enable_animations(false);
        }

        let monitors = display.monitors();
        let connectors = (0..monitors.n_items())
            .filter_map(|index| monitors.item(index))
            .filter_map(|item| item.downcast::<gtk::gdk::Monitor>().ok())
            .filter_map(|monitor| monitor.connector().map(|name| name.to_string()))
            .collect::<Vec<_>>();
        let Some(primary) = connectors.first().cloned() else {
            warn!("preview could not resolve a monitor connector");
            application.quit();
            return;
        };

        preview_config.primary_output = Some(primary.clone());
        let now_epoch = current_epoch();
        let snapshot = preview_snapshot(&connectors, &scenario, now_epoch);
        let mut suggestion_engine = SuggestionEngine::default();
        if scenario == "routine" {
            let action = crate::ActionIntent::SetPowerProfile {
                profile: crate::PowerProfile::Performance,
            };
            for days_ago in [3_i64, 2, 1] {
                let _ = suggestion_engine.record_success(
                    "preview",
                    &action,
                    &snapshot,
                    &preview_config.routines,
                    now_epoch.saturating_sub(days_ago * 24 * 60 * 60),
                );
            }
        }

        let (action_tx, _action_rx) = mpsc::channel();
        let mut surfaces = SurfaceRegistry::with_suggestion_engine(None, suggestion_engine);
        surfaces.reconcile(application, &snapshot, &preview_config, &action_tx);
        if matches!(
            scenario.as_str(),
            "control-center" | "suggestion" | "routine"
        ) {
            surfaces.open_control_center(crate::DesktopContext::Overview, Some(&primary));
        }
        if scenario == "power" {
            surfaces.open_control_center_page(
                control_center::ControlCenterFocus::Power,
                Some(&primary),
            );
        }
        if scenario == "ai-usage" {
            surfaces.open_control_center_page(
                control_center::ControlCenterFocus::AiUsage,
                Some(&primary),
            );
        }
        *registry_for_activate.borrow_mut() = Some(surfaces);
    });

    application.run_with_args(&["vigil-preview"]);
    if let Some(mut registry) = registry.borrow_mut().take() {
        registry.clear();
    }
    Ok(())
}

fn preview_snapshot(connectors: &[String], scenario: &str, now_epoch: i64) -> crate::BarSnapshot {
    let outputs = connectors
        .iter()
        .enumerate()
        .map(|(index, connector)| {
            let active_workspace = format!("{}", index * 3 + 1);
            let workspaces = (0..3)
                .map(|offset| {
                    let label = format!("{}", index * 3 + offset + 1);
                    crate::WorkspaceState {
                        id: label.clone(),
                        label: if offset == 1 {
                            "code".to_string()
                        } else {
                            label.clone()
                        },
                        output: connector.clone(),
                        active: label == active_workspace,
                        urgent: scenario == "escalation" && index == 0 && offset == 2,
                        changed_at: now_epoch,
                    }
                })
                .collect::<Vec<_>>();
            let window = crate::WindowState {
                id: format!("preview-window-{index}"),
                app_id: Some(if index == 0 { "Code" } else { "Terminal" }.to_string()),
                title: if index == 0 {
                    "Vigil — expressive cockpit".to_string()
                } else {
                    "cargo test".to_string()
                },
                urgent: false,
                workspace_id: Some(active_workspace),
                changed_at: now_epoch,
            };
            (
                connector.clone(),
                crate::OutputState {
                    name: connector.clone(),
                    workspaces,
                    windows: vec![window.clone()],
                    focused_window: Some(window),
                    urgent: false,
                    changed_at: now_epoch,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    let mut snapshot = crate::BarSnapshot {
        focused_output: connectors.first().cloned(),
        outputs,
        system: crate::SystemState {
            keyboard_layout: crate::KeyboardLayoutState {
                current_index: Some(0),
                current_name: Some("English (US)".to_string()),
                layouts: vec![
                    crate::KeyboardLayoutOption {
                        index: 0,
                        name: "English (US)".to_string(),
                        layout: Some("us".to_string()),
                        variant: None,
                    },
                    crate::KeyboardLayoutOption {
                        index: 1,
                        name: "German".to_string(),
                        layout: Some("de".to_string()),
                        variant: None,
                    },
                ],
            },
            resources: crate::ResourceState {
                cpu_percent: Some(42),
                memory_percent: Some(58),
            },
            network: crate::NetworkState {
                connectivity: crate::ConnectivityState::Connected,
                label: Some("Studio Wi-Fi".to_string()),
                wifi_available: true,
                wifi_enabled: Some(true),
                interface: Some("wlan0".to_string()),
                ..crate::NetworkState::default()
            },
            bluetooth: crate::BluetoothState {
                available: true,
                powered: true,
                connected_device: Some("Studio Headphones".to_string()),
                audio_device: Some("Studio Headphones".to_string()),
                ..crate::BluetoothState::default()
            },
            audio: crate::AudioState {
                volume_percent: Some(64),
                muted: false,
                outputs: vec![crate::AudioOutputState {
                    name: "preview-output".to_string(),
                    description: "Studio Headphones".to_string(),
                    alias: None,
                    port_description: None,
                    port_type: Some("headphones".to_string()),
                    bus: Some("bluetooth".to_string()),
                    is_default: true,
                }],
            },
            brightness: crate::BrightnessState {
                device: Some("preview-backlight".to_string()),
                percent: Some(72),
            },
            power: crate::PowerState {
                battery_present: true,
                battery_percent: Some(78),
                charging: false,
                profile: crate::PowerProfile::Balanced,
                available_profiles: vec![
                    crate::PowerProfile::PowerSaver,
                    crate::PowerProfile::Balanced,
                    crate::PowerProfile::Performance,
                ],
                performance_degraded: None,
                stats: crate::PowerStats {
                    energy_rate_mw: Some(8_400),
                    time_to_empty_seconds: Some(5 * 3_600 + 12 * 60),
                    time_to_full_seconds: None,
                    energy_now_mwh: Some(44_460),
                    energy_full_mwh: Some(57_000),
                    energy_full_design_mwh: Some(62_000),
                    cycle_count: Some(143),
                    charge_limit_percent: Some(80),
                    cpu_temp_celsius: Some(47),
                    cpu_freq_mhz: Some(1_840),
                    cpu_max_freq_mhz: Some(4_950),
                    cpu_energy_preference: Some("balance_performance".to_string()),
                },
                changed_at: now_epoch,
            },
            clock: crate::ClockState {
                epoch_seconds: now_epoch,
                label: "16:42".to_string(),
            },
            ..crate::SystemState::default()
        },
        ..crate::BarSnapshot::default()
    };

    match scenario {
        "ai-usage" => {
            snapshot.system.ai_usage = crate::AiUsageState {
                day: "Preview data".to_string(),
                codex: crate::AiProviderUsage {
                    available: true,
                    tokens: crate::TokenUsage {
                        input_tokens: 2_700_000,
                        cached_input_tokens: 2_100_000,
                        output_tokens: 300_000,
                        reasoning_output_tokens: 120_000,
                        total_tokens: 3_000_000,
                        ..crate::TokenUsage::default()
                    },
                    last_event_at: Some(now_epoch),
                    quotas: vec![crate::AiQuota {
                        used_percent: 43,
                        window_minutes: 10080,
                        resets_at: Some(now_epoch + 2 * 86400),
                        estimate: crate::AiQuotaEstimate::Available {
                            remaining_seconds: 3 * 3600,
                            exhausts_at: now_epoch + 3 * 3600,
                            sample_seconds: 3600,
                            used_percent_delta: 19,
                            lasts_until_reset: Some(false),
                        },
                    }],
                    quota_observed_at: Some(now_epoch),
                },
                claude: crate::AiProviderUsage {
                    available: true,
                    tokens: crate::TokenUsage {
                        input_tokens: 850_000,
                        cached_input_tokens: 500_000,
                        cache_write_input_tokens: 100_000,
                        output_tokens: 150_000,
                        total_tokens: 1_000_000,
                        ..crate::TokenUsage::default()
                    },
                    last_event_at: Some(now_epoch - 60),
                    ..crate::AiProviderUsage::default()
                },
            };
        }
        "escalation" => {
            snapshot.system.power.battery_percent = Some(6);
        }
        "suggestion" => {
            snapshot.system.audio.muted = true;
            snapshot.system.media = Some(crate::MediaState {
                player: "preview-player".to_string(),
                status: crate::PlaybackStatus::Playing,
                title: Some("Electric Feel".to_string()),
                artist: Some("MGMT".to_string()),
                changed_at: now_epoch,
                ..crate::MediaState::default()
            });
        }
        "control-center" => {
            snapshot.system.calendar = Some(crate::CalendarEvent {
                id: "preview-event".to_string(),
                title: "Design review".to_string(),
                location: Some("Studio".to_string()),
                start_epoch: now_epoch + 45 * 60,
                end_epoch: Some(now_epoch + 75 * 60),
                changed_at: now_epoch,
            });
            snapshot.system.timers.push(crate::TimerState {
                id: "preview-timer".to_string(),
                label: "Deep work".to_string(),
                remaining_seconds: 4 * 60,
                target_epoch: Some(now_epoch + 4 * 60),
                completed: false,
                changed_at: now_epoch,
            });
        }
        _ => {}
    }
    snapshot
}

impl RuntimeHandles {
    fn shutdown(self) {
        self.cancelled.store(true, Ordering::Relaxed);
        for handle in self.joins {
            if let Err(error) = handle.join() {
                warn!("worker panicked during shutdown: {:?}", error);
            }
        }
    }
}

fn install_ui_loop(
    application: &gtk::Application,
    runtime: UiRuntime,
    monitor_dirty: Rc<Cell<bool>>,
) {
    let state = Rc::new(RefCell::new(runtime));
    let theme_provider = Rc::new(RefCell::new(None::<gtk::CssProvider>));

    if let Some(wakeup) = state.borrow_mut().control_wakeup.take() {
        let state = Rc::clone(&state);
        glib::source::unix_fd_add_local(wakeup.as_raw_fd(), glib::IOCondition::IN, move |_, _| {
            // Coalesce queued wakeups; requests remain in the typed channel.
            while wakeup.recv(&mut [0; 64]).is_ok() {}
            drain_control_requests(&mut state.borrow_mut());
            glib::ControlFlow::Continue
        });
    }

    install_swipe_wakeup();

    if let Some(display) = gtk::gdk::Display::default() {
        if let Err(error) = refresh_theme_provider(
            &display,
            &state.borrow().config_path,
            &mut theme_provider.borrow_mut(),
        ) {
            warn!("theme css unavailable: {error:#}");
        }

        let monitors = display.monitors();
        let monitor_dirty_flag = Rc::clone(&monitor_dirty);
        monitors.connect_items_changed(move |_, _, _, _| {
            monitor_dirty_flag.set(true);
        });
    }

    {
        let monitor_dirty = Rc::clone(&monitor_dirty);
        let state = Rc::clone(&state);
        let application = application.clone();
        let theme_provider = Rc::clone(&theme_provider);
        glib::timeout_add_local(UI_TICK_INTERVAL, move || {
            let mut runtime = state.borrow_mut();
            let now_epoch = current_epoch();
            let mut dirty = drain_updates(&mut runtime, now_epoch);
            dirty |= handle_reload_request(
                &mut runtime,
                gtk::gdk::Display::default().as_ref(),
                &mut theme_provider.borrow_mut(),
                now_epoch,
            );
            dirty |= prune_completed_activities(&mut runtime, now_epoch);
            dirty |= runtime.store.expire(now_epoch);
            dirty |= runtime.registry.take_context_dirty();
            drain_control_requests(&mut runtime);

            if dirty || monitor_dirty.replace(false) {
                let snapshot = runtime.store.snapshot().clone();
                let config = runtime.config.clone();
                let action_tx = runtime.action_tx.clone();
                runtime
                    .registry
                    .reconcile(&application, &snapshot, &config, &action_tx);
            }

            glib::ControlFlow::Continue
        });
    }

    let mut runtime = state.borrow_mut();
    let snapshot = runtime.store.snapshot().clone();
    let config = runtime.config.clone();
    let action_tx = runtime.action_tx.clone();
    runtime
        .registry
        .reconcile(application, &snapshot, &config, &action_tx);
}

/// Swipe samples bypass the state loop; this wakes idle workspace pills on the
/// main thread when a swipe starts so they can follow it frame by frame.
fn install_swipe_wakeup() {
    let (wakeup, waker) = match UnixDatagram::pair() {
        Ok(pair) => pair,
        Err(error) => {
            warn!("swipe wakeup unavailable: {error:#}");
            return;
        }
    };
    let _ = wakeup.set_nonblocking(true);
    let _ = waker.set_nonblocking(true);
    SwipeFeed::global().set_waker(move || {
        let _ = waker.send(&[1]);
    });
    glib::source::unix_fd_add_local(wakeup.as_raw_fd(), glib::IOCondition::IN, move |_, _| {
        while wakeup.recv(&mut [0; 64]).is_ok() {}
        workspace_pill::wake_all();
        glib::ControlFlow::Continue
    });
}

fn drain_updates(runtime: &mut UiRuntime, now_epoch: i64) -> bool {
    let mut dirty = false;

    loop {
        match runtime.state_rx.try_recv() {
            Ok(StateUpdate::Activity(update)) => {
                if runtime.activity_tracker.apply(update, now_epoch) {
                    let activities = runtime
                        .activity_tracker
                        .snapshot()
                        .items
                        .into_values()
                        .collect::<Vec<_>>();
                    dirty |= runtime.store.apply(
                        StateUpdate::Activity(ActivityUpdate::Snapshot(activities)),
                        now_epoch,
                    );
                }
            }
            Ok(update) => {
                dirty |= runtime.store.apply(update, now_epoch);
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => break,
        }
    }

    loop {
        match runtime.completion_rx.try_recv() {
            Ok(completion) => {
                if let Some(response_tx) =
                    runtime.pending_control_actions.remove(&completion.origin)
                {
                    let response = match &completion.result {
                        ActionResult::Completed => ControlResponse::ActionResult {
                            success: true,
                            message: None,
                        },
                        ActionResult::Failed { detail, .. } => ControlResponse::ActionResult {
                            success: false,
                            message: Some(detail.clone()),
                        },
                    };
                    let _ = response_tx.send(response);
                }
                dirty |= runtime.registry.handle_completion(
                    &completion,
                    runtime.store.snapshot(),
                    &runtime.config,
                    now_epoch,
                );
                if let ActionResult::Failed { detail, .. } = &completion.result {
                    warn!("action {} failed: {}", completion.origin, detail);
                }
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => break,
        }
    }

    dirty
}

fn start_runtime(config: AppConfig, config_path: &Path) -> Result<(UiRuntime, RuntimeHandles)> {
    let (state_tx, state_rx) = mpsc::channel::<StateUpdate>();
    let cancelled = Arc::new(AtomicBool::new(false));
    let timer_store = Arc::new(Mutex::new(TimerStore::load(current_epoch())?));
    let (control_tx, control_rx) = mpsc::channel::<UiControlRequest>();
    let (control_wakeup_rx, control_wakeup_tx) = UnixDatagram::pair()?;
    control_wakeup_rx.set_nonblocking(true)?;
    control_wakeup_tx.set_nonblocking(true)?;
    let (bluetooth, bluetooth_join) =
        spawn_bluetooth_source(state_tx.clone(), Arc::clone(&cancelled));
    let mut joins = vec![
        spawn_compositor_worker(state_tx.clone(), Arc::clone(&cancelled)),
        spawn_resource_source(state_tx.clone(), Arc::clone(&cancelled)),
        crate::spawn_ai_usage_source(state_tx.clone(), Arc::clone(&cancelled)),
        spawn_power_source(state_tx.clone(), Arc::clone(&cancelled)),
        spawn_network_source(state_tx.clone(), Arc::clone(&cancelled)),
        bluetooth_join,
        spawn_audio_source(state_tx.clone(), Arc::clone(&cancelled)),
        spawn_brightness_source(state_tx.clone(), Arc::clone(&cancelled)),
        spawn_media_source(state_tx.clone(), Arc::clone(&cancelled)),
        spawn_clock_source(state_tx.clone(), Arc::clone(&cancelled)),
        spawn_timer_tick_worker(
            state_tx.clone(),
            Arc::clone(&cancelled),
            Arc::clone(&timer_store),
        ),
    ];
    let notification_control = if config.notifications.enabled {
        let (control, handle) = spawn_notification_server(
            config.notifications.clone(),
            state_tx.clone(),
            Arc::clone(&cancelled),
        )?;
        joins.push(handle);
        Some(control)
    } else {
        None
    };
    joins.push(spawn_control_socket_server(
        state_tx.clone(),
        Arc::clone(&cancelled),
        Arc::clone(&timer_store),
        control_tx,
        control_wakeup_tx,
        notification_control.clone(),
    )?);

    let mut calendar_sender: Option<Sender<CalendarMonthRequest>> = None;
    match resolve_calendar_script(config_path) {
        Some(calendar_script) => {
            joins.push(crate::spawn_calendar_source(
                calendar_script.clone(),
                state_tx.clone(),
                Arc::clone(&cancelled),
            ));
            let (sender, handle) = spawn_calendar_agenda_source(
                calendar_script,
                state_tx.clone(),
                Arc::clone(&cancelled),
            );
            calendar_sender = Some(sender);
            joins.push(handle);
        }
        None => {
            let message = format!(
                "calendar helper unavailable for config {}",
                config_path.display()
            );
            warn!("{message}");
            let _ = state_tx.send(StateUpdate::Health {
                source: SourceId::Calendar,
                health: SourceHealth::Disconnected {
                    message: message.clone(),
                },
            });
            let _ = state_tx.send(StateUpdate::Health {
                source: SourceId::CalendarAgenda,
                health: SourceHealth::Disconnected { message },
            });
        }
    }

    let mut store = StateStore::new(config.freshness.clone());
    let mut activity_tracker = ActivityTracker::new(config.thresholds.work_completed_seconds);
    seed_initial_state(&state_rx, &mut store, &mut activity_tracker);

    let router = ActionRouter::new(SystemActionBackend::from_env(bluetooth)?)
        .with_power_profile_state(store.snapshot().system.power.profile.clone());
    let (completion_tx, completion_rx) = mpsc::channel();
    let (action_tx, action_handle) =
        spawn_action_worker(router, completion_tx, Arc::clone(&cancelled));
    joins.push(action_handle);
    let suggestion_engine = SuggestionEngine::load()?;

    Ok((
        UiRuntime {
            config,
            config_path: config_path.to_path_buf(),
            store,
            activity_tracker,
            registry: SurfaceRegistry::with_services(
                calendar_sender,
                suggestion_engine,
                notification_control.clone(),
            ),
            state_rx,
            completion_rx,
            action_tx,
            control_rx,
            control_wakeup: Some(control_wakeup_rx),
            pending_control_actions: BTreeMap::new(),
            next_control_action_id: 0,
            osd: osd::Osd::default(),
        },
        RuntimeHandles { cancelled, joins },
    ))
}

fn seed_initial_state(
    state_rx: &Receiver<StateUpdate>,
    store: &mut StateStore,
    activity_tracker: &mut ActivityTracker,
) {
    let deadline = Instant::now() + INITIAL_DRAIN_WINDOW;

    loop {
        let now_epoch = current_epoch();
        match state_rx.try_recv() {
            Ok(StateUpdate::Activity(update)) => {
                if activity_tracker.apply(update, now_epoch) {
                    let activities = activity_tracker
                        .snapshot()
                        .items
                        .into_values()
                        .collect::<Vec<_>>();
                    let _ = store.apply(
                        StateUpdate::Activity(ActivityUpdate::Snapshot(activities)),
                        now_epoch,
                    );
                }
            }
            Ok(update) => {
                let _ = store.apply(update, now_epoch);
            }
            Err(TryRecvError::Empty) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
}

fn prune_completed_activities(runtime: &mut UiRuntime, now_epoch: i64) -> bool {
    if !runtime.activity_tracker.prune(now_epoch) {
        return false;
    }

    let activities = runtime
        .activity_tracker
        .snapshot()
        .items
        .into_values()
        .collect::<Vec<_>>();
    runtime.store.apply(
        StateUpdate::Activity(ActivityUpdate::Snapshot(activities)),
        now_epoch,
    )
}

fn spawn_compositor_worker(
    sender: Sender<StateUpdate>,
    cancelled: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        while !cancelled.load(Ordering::Relaxed) {
            let env = std::env::vars().collect::<Vec<_>>();
            let env_refs = env
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect::<Vec<_>>();

            match detect_compositor(&env_refs) {
                Ok(mut compositor) => {
                    for update in compositor.initial_snapshot().unwrap_or_default() {
                        if sender.send(update).is_err() {
                            cancelled.store(true, Ordering::Relaxed);
                            return;
                        }
                    }
                    let _ = sender.send(StateUpdate::Health {
                        source: SourceId::Compositor,
                        health: SourceHealth::Healthy,
                    });

                    loop {
                        if cancelled.load(Ordering::Relaxed) {
                            return;
                        }

                        match compositor.next_update_interruptibly(cancelled.as_ref()) {
                            Ok(Some(update)) => {
                                if sender.send(update).is_err() {
                                    cancelled.store(true, Ordering::Relaxed);
                                    return;
                                }
                            }
                            Ok(None) => return,
                            Err(error) => {
                                // An end event may have been lost with the connection.
                                SwipeFeed::global().clear();
                                let _ = sender.send(StateUpdate::Health {
                                    source: SourceId::Compositor,
                                    health: SourceHealth::Disconnected {
                                        message: error.to_string(),
                                    },
                                });
                                break;
                            }
                        }
                    }
                }
                Err(error) => {
                    let _ = sender.send(StateUpdate::Health {
                        source: SourceId::Compositor,
                        health: SourceHealth::Disconnected {
                            message: error.to_string(),
                        },
                    });
                }
            }

            if cancelled.load(Ordering::Relaxed) {
                break;
            }
            if !wait_for_cancellation(&cancelled, COMPOSITOR_RECONNECT_INTERVAL) {
                break;
            }
        }
    })
}

fn spawn_control_socket_server(
    sender: Sender<StateUpdate>,
    cancelled: Arc<AtomicBool>,
    timer_store: Arc<Mutex<TimerStore>>,
    control_tx: Sender<UiControlRequest>,
    control_wakeup: UnixDatagram,
    notification_control: Option<NotificationControl>,
) -> Result<JoinHandle<()>> {
    let socket = ControlSocket::bind()?;
    socket.set_nonblocking(true)?;

    Ok(thread::spawn(move || {
        while !cancelled.load(Ordering::Relaxed) {
            match socket.wait_for_request(CONTROL_SOCKET_POLL_INTERVAL) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(error) => {
                    warn!("control socket wait failed: {error:#}");
                    if !wait_for_cancellation(&cancelled, CONTROL_SOCKET_POLL_INTERVAL) {
                        break;
                    }
                    continue;
                }
            }
            match socket.try_serve_once(|request| {
                handle_control_request(
                    request,
                    &sender,
                    &timer_store,
                    &control_tx,
                    &control_wakeup,
                    notification_control.as_ref(),
                )
            }) {
                Ok(true) => {}
                Ok(false) => {}
                Err(error) => {
                    warn!("control socket error: {error:#}");
                    if !wait_for_cancellation(&cancelled, CONTROL_SOCKET_POLL_INTERVAL) {
                        break;
                    }
                }
            }
        }
    }))
}

fn handle_control_request(
    request: ControlRequest,
    sender: &Sender<StateUpdate>,
    timer_store: &Arc<Mutex<TimerStore>>,
    control_tx: &Sender<UiControlRequest>,
    control_wakeup: &UnixDatagram,
    notification_control: Option<&NotificationControl>,
) -> Result<ControlResponse> {
    match request {
        ControlRequest::TimerStart { .. }
        | ControlRequest::TimerPause { .. }
        | ControlRequest::TimerResume { .. }
        | ControlRequest::TimerCancel { .. }
        | ControlRequest::TimerList => {
            let now_epoch = current_epoch();
            let timers = timer_store
                .lock()
                .map_err(|_| anyhow!("timer store lock poisoned"))?
                .apply(&request, now_epoch)?;
            let _ = sender.send(StateUpdate::System(SystemUpdate::Timers(timers.clone())));
            Ok(ControlResponse::Timers { timers })
        }
        ControlRequest::ActivityStart {
            id,
            label,
            cwd,
            started_at,
        } => {
            let activity = CommandActivity {
                id,
                label,
                cwd,
                status: ActivityStatus::Running,
                started_at,
                finished_at: None,
                exit_code: None,
            };
            let _ = sender.send(StateUpdate::Activity(ActivityUpdate::Started(activity)));
            Ok(ControlResponse::Accepted)
        }
        ControlRequest::ActivityFinish {
            id,
            exit_code,
            finished_at,
        } => {
            let _ = sender.send(StateUpdate::Activity(ActivityUpdate::Finished {
                id,
                finished_at,
                exit_code,
            }));
            Ok(ControlResponse::Accepted)
        }
        ControlRequest::NotificationsGet => {
            let control = notification_control.context("notification server is disabled")?;
            Ok(ControlResponse::Notifications {
                state: control.snapshot()?,
            })
        }
        ControlRequest::NotificationInvoke { id, action_key } => {
            notification_control
                .context("notification server is disabled")?
                .invoke(id, action_key)?;
            Ok(ControlResponse::Accepted)
        }
        ControlRequest::NotificationDismiss { id } => {
            notification_control
                .context("notification server is disabled")?
                .dismiss(id)?;
            Ok(ControlResponse::Accepted)
        }
        ControlRequest::NotificationsClear => {
            notification_control
                .context("notification server is disabled")?
                .clear()?;
            Ok(ControlResponse::Accepted)
        }
        ControlRequest::NotificationsSetDnd { enabled } => {
            notification_control
                .context("notification server is disabled")?
                .set_dnd(enabled)?;
            Ok(ControlResponse::Accepted)
        }
        request @ (ControlRequest::RoutinesClear
        | ControlRequest::ContextGet { .. }
        | ControlRequest::ContextExecute { .. }
        | ControlRequest::ControlCenterOpen { .. }
        | ControlRequest::ControlCenterToggle { .. }
        | ControlRequest::ControlCenterClose
        | ControlRequest::LockscreenGet
        | ControlRequest::PresentationGet
        | ControlRequest::PresentationSet { .. }
        | ControlRequest::OsdShow { .. }
        | ControlRequest::OsdHide
        | ControlRequest::MediaGet
        | ControlRequest::MediaControl { .. }) => {
            let (response_tx, response_rx) = mpsc::channel();
            control_tx
                .send(UiControlRequest {
                    request,
                    response_tx,
                })
                .map_err(|_| anyhow!("bar UI is unavailable"))?;
            // A full wakeup socket already has a readable notification queued.
            let _ = control_wakeup.send(&[1]);
            response_rx
                .recv_timeout(CONTROL_UI_RESPONSE_TIMEOUT)
                .map_err(|_| anyhow!("bar UI did not answer the integration request"))
        }
    }
}

fn execute_control_action(
    runtime: &mut UiRuntime,
    action: ContextAction,
    response_tx: Sender<ControlResponse>,
) {
    let intent = match intent_for_context_action(runtime.store.snapshot(), action) {
        Ok(intent) => intent,
        Err(error) => {
            let _ = response_tx.send(ControlResponse::ActionResult {
                success: false,
                message: Some(error.to_string()),
            });
            return;
        }
    };
    runtime.next_control_action_id = runtime.next_control_action_id.wrapping_add(1);
    let origin = format!("luma-context:{}", runtime.next_control_action_id);
    runtime
        .pending_control_actions
        .insert(origin.clone(), response_tx.clone());
    if runtime
        .action_tx
        .send(ActionRequest {
            origin: origin.clone(),
            intent,
        })
        .is_err()
    {
        runtime.pending_control_actions.remove(&origin);
        let _ = response_tx.send(ControlResponse::ActionResult {
            success: false,
            message: Some("bar action worker is unavailable".to_string()),
        });
    }
}

fn drain_control_requests(runtime: &mut UiRuntime) {
    loop {
        let request = match runtime.control_rx.try_recv() {
            Ok(request) => request,
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        };
        match request.request {
            ControlRequest::RoutinesClear => {
                let response = match runtime.registry.clear_routine_history() {
                    Ok(()) => ControlResponse::Accepted,
                    Err(error) => ControlResponse::Error {
                        message: format!("failed to clear routine history: {error:#}"),
                    },
                };
                let _ = request.response_tx.send(response);
            }
            ControlRequest::ContextGet { context } => {
                let contexts = context_snapshots(runtime.store.snapshot(), context);
                let _ = request
                    .response_tx
                    .send(ControlResponse::Contexts { contexts });
            }
            ControlRequest::ContextExecute { action } => {
                execute_control_action(runtime, action, request.response_tx);
            }
            ControlRequest::MediaGet => {
                let media = runtime.store.snapshot().system.media.clone();
                let _ = request.response_tx.send(ControlResponse::Media { media });
            }
            ControlRequest::MediaControl { action, player } => {
                let player = player.or_else(|| {
                    runtime
                        .store
                        .snapshot()
                        .system
                        .media
                        .as_ref()
                        .map(|media| media.player.clone())
                });
                match player {
                    Some(player) => execute_control_action(
                        runtime,
                        ContextAction::ControlMedia { player, action },
                        request.response_tx,
                    ),
                    None => {
                        let _ = request.response_tx.send(ControlResponse::ActionResult {
                            success: false,
                            message: Some("no media player is active".to_string()),
                        });
                    }
                }
            }
            ControlRequest::ControlCenterOpen { context, output } => {
                let focused_output = runtime.store.snapshot().focused_output.clone();
                let opened = runtime
                    .registry
                    .open_control_center(context, output.as_deref().or(focused_output.as_deref()));
                let _ = request.response_tx.send(if opened {
                    ControlResponse::Accepted
                } else {
                    ControlResponse::Error {
                        message: "the primary quick-settings surface is unavailable".to_string(),
                    }
                });
            }
            ControlRequest::ControlCenterToggle { context, output } => {
                let focused_output = runtime.store.snapshot().focused_output.clone();
                let toggled = runtime.registry.toggle_control_center(
                    context,
                    output.as_deref().or(focused_output.as_deref()),
                );
                let _ = request.response_tx.send(if toggled {
                    ControlResponse::Accepted
                } else {
                    ControlResponse::Error {
                        message: "the primary quick-settings surface is unavailable".to_string(),
                    }
                });
            }
            ControlRequest::ControlCenterClose => {
                runtime.registry.close_control_center();
                let _ = request.response_tx.send(ControlResponse::Accepted);
            }
            ControlRequest::LockscreenGet => {
                let snapshot = crate::lockscreen_snapshot(runtime.store.snapshot());
                let _ = request
                    .response_tx
                    .send(ControlResponse::Lockscreen { snapshot });
            }
            ControlRequest::PresentationGet => {
                let hidden = runtime.registry.presentation_hidden();
                let _ = request
                    .response_tx
                    .send(ControlResponse::Presentation { hidden });
            }
            ControlRequest::PresentationSet { hidden } => {
                runtime.registry.set_presentation_hidden(hidden);
                let _ = request.response_tx.send(ControlResponse::Accepted);
            }
            ControlRequest::OsdShow { content, output } => {
                let focused_output = runtime.store.snapshot().focused_output.clone();
                let shown = runtime
                    .osd
                    .show(&content, output.as_deref().or(focused_output.as_deref()));
                let _ = request.response_tx.send(if shown {
                    ControlResponse::Accepted
                } else {
                    ControlResponse::Error {
                        message: "no monitor is available for the on-screen display".to_string(),
                    }
                });
            }
            ControlRequest::OsdHide => {
                runtime.osd.hide();
                let _ = request.response_tx.send(ControlResponse::Accepted);
            }
            _ => {
                let _ = request.response_tx.send(ControlResponse::Error {
                    message: "request does not require the bar UI".to_string(),
                });
            }
        }
    }
}

fn spawn_timer_tick_worker(
    sender: Sender<StateUpdate>,
    cancelled: Arc<AtomicBool>,
    timer_store: Arc<Mutex<TimerStore>>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        while !cancelled.load(Ordering::Relaxed) {
            let now_epoch = current_epoch();
            match timer_store
                .lock()
                .map_err(|_| anyhow!("timer store lock poisoned"))
                .and_then(|mut store| store.snapshot(now_epoch))
            {
                Ok(timers) => {
                    if sender
                        .send(StateUpdate::System(SystemUpdate::Timers(timers)))
                        .is_err()
                    {
                        cancelled.store(true, Ordering::Relaxed);
                        return;
                    }
                }
                Err(error) => warn!("timer tick error: {error:#}"),
            }

            if !wait_for_cancellation(&cancelled, TIMER_TICK_INTERVAL) {
                break;
            }
        }
    })
}

fn resolve_calendar_script(config_path: &Path) -> Option<PathBuf> {
    let canonical_config = config_path.canonicalize().ok();
    let candidates = [
        canonical_config
            .as_deref()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(|root| root.join("scripts").join("next_event.sh")),
        canonical_config
            .as_deref()
            .and_then(Path::parent)
            .map(|parent| parent.join("scripts").join("next_event.sh")),
        config_path
            .parent()
            .map(|parent| parent.join("scripts").join("next_event.sh")),
        std::env::current_dir()
            .ok()
            .map(|cwd| cwd.join("scripts").join("next_event.sh")),
    ];

    candidates
        .into_iter()
        .flatten()
        .find(|candidate| candidate.is_file())
}

fn current_epoch() -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time");
    i64::try_from(now.as_secs()).unwrap_or(i64::MAX)
}

extern "C" fn handle_sighup(_: libc::c_int) {
    RELOAD_REQUESTED.store(true, Ordering::Relaxed);
}

fn handle_reload_request(
    runtime: &mut UiRuntime,
    display: Option<&gtk::gdk::Display>,
    theme_provider: &mut Option<gtk::CssProvider>,
    now_epoch: i64,
) -> bool {
    if !RELOAD_REQUESTED.swap(false, Ordering::Relaxed) {
        return false;
    }

    match AppConfig::load(&runtime.config_path) {
        Ok(next) => match reload_runtime_config(&runtime.config, next) {
            crate::RuntimeConfigReload {
                config,
                status: ReloadStatus::Applied,
            } => {
                let threshold_changed = runtime.config.thresholds.work_completed_seconds
                    != config.thresholds.work_completed_seconds;
                runtime.config = config;
                if threshold_changed {
                    let activities = runtime
                        .activity_tracker
                        .snapshot()
                        .items
                        .into_values()
                        .collect::<Vec<_>>();
                    let mut tracker =
                        ActivityTracker::new(runtime.config.thresholds.work_completed_seconds);
                    let _ = tracker.apply(ActivityUpdate::Snapshot(activities), now_epoch);
                    runtime.activity_tracker = tracker;
                }
                if let Some(display) = display
                    && let Err(error) =
                        refresh_theme_provider(display, &runtime.config_path, theme_provider)
                {
                    warn!("theme reload failed: {error:#}");
                }
                true
            }
            crate::RuntimeConfigReload {
                status: ReloadStatus::RestartRequired { reasons },
                ..
            } => {
                warn!("config reload requires restart: {}", reasons.join(", "));
                false
            }
        },
        Err(error) => {
            warn!("config reload rejected: {error:#}");
            false
        }
    }
}

fn refresh_theme_provider(
    display: &gtk::gdk::Display,
    config_path: &Path,
    theme_provider: &mut Option<gtk::CssProvider>,
) -> Result<()> {
    if let Some(provider) = theme_provider.as_ref() {
        theme::reload_css(provider, config_path)?;
        return Ok(());
    }

    *theme_provider = Some(theme::load_css(display, config_path)?);
    Ok(())
}

fn install_sighup_handler() -> Result<()> {
    let previous = unsafe {
        libc::signal(
            libc::SIGHUP,
            handle_sighup as *const () as libc::sighandler_t,
        )
    };
    if previous == libc::SIG_ERR {
        return Err(anyhow!("failed to install SIGHUP handler"));
    }

    Ok(())
}

fn wait_for_cancellation(cancelled: &AtomicBool, duration: Duration) -> bool {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if cancelled.load(Ordering::Relaxed) {
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(Duration::from_millis(25)));
    }
    !cancelled.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::{RuntimeHandles, resolve_calendar_script};

    #[test]
    fn calendar_helper_resolves_through_installed_config_symlink() {
        let root = temp_dir("calendar-config-symlink");
        let repo = root.join("dotfiles");
        let bar = repo.join("bar");
        let scripts = repo.join("scripts");
        let config_home = root.join("config");
        fs::create_dir_all(&bar).unwrap();
        fs::create_dir_all(&scripts).unwrap();
        fs::create_dir_all(&config_home).unwrap();
        fs::write(bar.join("config.toml"), "").unwrap();
        fs::write(scripts.join("next_event.sh"), "#!/usr/bin/env bash\n").unwrap();
        std::os::unix::fs::symlink(&bar, config_home.join("vigil")).unwrap();

        let resolved = resolve_calendar_script(&config_home.join("vigil/config.toml"))
            .expect("calendar helper");

        assert_eq!(
            resolved.canonicalize().unwrap(),
            scripts.join("next_event.sh").canonicalize().unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shutdown_joins_workers_that_finish_after_cancellation() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_finished = Arc::new(AtomicBool::new(false));
        let worker_finished_flag = Arc::clone(&worker_finished);
        let cancelled_flag = Arc::clone(&cancelled);
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(250));
            if cancelled_flag.load(Ordering::Relaxed) {
                worker_finished_flag.store(true, Ordering::Relaxed);
            }
        });

        RuntimeHandles {
            cancelled,
            joins: vec![handle],
        }
        .shutdown();

        assert!(worker_finished.load(Ordering::Relaxed));
    }

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!("vigil-{label}-{unique}"))
    }
}
