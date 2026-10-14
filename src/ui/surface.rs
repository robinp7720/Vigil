use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::mpsc::Sender;

use gtk::gdk;
use gtk::gio::prelude::ListModelExtManual;
use gtk::glib;
use gtk::pango::EllipsizeMode;
use gtk::prelude::*;
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, Layer, LayerShell};

use crate::{
    ActionCompletion, ActionIntent, ActionRequest, AmbientMotion, AppConfig, BarSnapshot,
    CalendarMonthRequest, ContextCard, ContextTier, DesktopContext, Direction, Dismissals,
    MediaControlAction, MotionStyle, Notification, NotificationControl, OutputRole, Suggestion,
    SuggestionAction, SuggestionEngine, SuggestionSource, WorkspaceState, select_context,
};

use super::context_card::{
    ContextAction, context_action, context_presentation, context_tier, media_playback_icon,
    warning_text,
};
use super::control_center::{
    ControlCenterFocus, ControlCenterView, NotificationPresentation, notification_row,
};
use super::morph::{self, MorphRect};
use super::morph_bin::{MorphBin, MorphOrigin, MorphShadow};
use super::popovers::PopoverCoordinator;
use super::stage::{self, BAR_HEIGHT, SURFACE_MARGIN};
use super::system::{SystemButtonSpec, SystemCluster, SystemModuleId, build_system_cluster};
use super::wm::{WindowGroupSpec, select_primary_output, title_for_output, window_groups};

const WORKSPACE_BUTTON_MIN_WIDTH: i32 = 32;
const WORKSPACE_BUTTON_MIN_HEIGHT: i32 = 28;
const CENTER_TEXT_MAX_CHARS: i32 = 56;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceSpec {
    pub output_name: String,
    pub role: OutputRole,
    pub workspaces: Vec<WorkspaceButtonSpec>,
    pub title: TitleSpec,
    pub context: Option<ContextSpec>,
    pub warning: Option<WarningSpec>,
    pub suggestion: Option<Suggestion>,
    pub control_suggestions: Vec<Suggestion>,
    pub system: Option<SystemCluster>,
    pub clock_label: String,
    pub motion_style: MotionStyle,
    pub ambient_motion: AmbientMotion,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceButtonSpec {
    pub id: String,
    pub label: String,
    pub active: bool,
    pub urgent: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitleSpec {
    pub app: String,
    pub text: String,
    pub window_groups: Vec<WindowGroupSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextSpec {
    pub card: ContextCard,
    pub icon_name: String,
    pub text: String,
    pub tier: ContextTier,
    pub now_epoch: i64,
    pub critical_snooze_seconds: u64,
}

/// What the island shows, ignoring text and values that refresh in place (timer ticks,
/// `now_epoch`, battery percent). The island crossfades only when this changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ContextIdentity {
    content: ContextContent,
    suggestion: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ContextContent {
    Title,
    Card(String),
    Suggestion(String),
}

pub(super) fn context_identity(
    context: Option<&ContextSpec>,
    suggestion: Option<&Suggestion>,
) -> ContextIdentity {
    let content = match (context, suggestion) {
        (Some(context), _) => ContextContent::Card(card_identity(&context.card)),
        (None, Some(suggestion)) => ContextContent::Suggestion(suggestion.id.clone()),
        (None, None) => ContextContent::Title,
    };
    ContextIdentity {
        content,
        suggestion: suggestion.map(|suggestion| suggestion.id.clone()),
    }
}

fn card_identity(card: &ContextCard) -> String {
    match card {
        ContextCard::Battery { .. } => "battery".to_string(),
        ContextCard::Calendar { id, .. } => format!("calendar|{id}"),
        ContextCard::Timer { id, .. } => format!("timer|{id}"),
        ContextCard::Activity { id, .. } => format!("activity|{id}"),
        ContextCard::Media { player, .. } => format!("media|{player}"),
        ContextCard::Urgent {
            output,
            workspace,
            window_id,
            ..
        } => format!(
            "urgent|{output}|{}|{}",
            workspace.as_deref().unwrap_or_default(),
            window_id.as_deref().unwrap_or_default()
        ),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WarningSpec {
    pub text: String,
    pub tier: ContextTier,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderPlan {
    pub workspaces: bool,
    pub title: bool,
    pub context: bool,
    pub warning: bool,
    pub system: bool,
    pub clock: bool,
    pub suggestions: bool,
    pub motion: bool,
}

impl RenderPlan {
    pub fn between(previous: Option<&SurfaceSpec>, next: &SurfaceSpec) -> Self {
        match previous {
            None => Self {
                workspaces: true,
                title: true,
                context: true,
                warning: true,
                system: true,
                clock: true,
                suggestions: true,
                motion: true,
            },
            Some(previous) => Self {
                workspaces: previous.workspaces != next.workspaces,
                title: previous.title != next.title,
                context: previous.context != next.context || previous.suggestion != next.suggestion,
                warning: previous.warning != next.warning,
                system: previous.system != next.system,
                clock: previous.clock_label != next.clock_label,
                suggestions: previous.control_suggestions != next.control_suggestions,
                motion: previous.motion_style != next.motion_style
                    || previous.ambient_motion != next.ambient_motion,
            },
        }
    }
}

pub fn surface_specs(
    snapshot: &BarSnapshot,
    config: &AppConfig,
    dismissals: &Dismissals,
) -> Vec<SurfaceSpec> {
    surface_specs_with_suggestions(snapshot, config, dismissals, &[])
}

fn surface_specs_with_suggestions(
    snapshot: &BarSnapshot,
    config: &AppConfig,
    dismissals: &Dismissals,
    suggestions: &[Suggestion],
) -> Vec<SurfaceSpec> {
    let Some(primary_output) = select_primary_output(snapshot, config) else {
        return Vec::new();
    };

    let groups = window_groups(snapshot);
    let now_epoch = snapshot.system.clock.epoch_seconds;
    let selected_context = select_context(snapshot, now_epoch, &config.thresholds, dismissals);
    let primary_context = selected_context.as_ref().map(|card| {
        let presentation = context_presentation(card, now_epoch);
        ContextSpec {
            card: card.clone(),
            icon_name: presentation.icon_name,
            text: presentation.text,
            tier: presentation.tier,
            now_epoch,
            critical_snooze_seconds: config.thresholds.critical_snooze_seconds,
        }
    });
    let reduced_warning = selected_context
        .as_ref()
        .filter(|card| context_tier(card) == ContextTier::Critical)
        .map(|card| WarningSpec {
            text: warning_text(card),
            tier: ContextTier::Critical,
        });
    let primary_suggestion = bar_suggestion(selected_context.as_ref(), suggestions);
    let mut specs = Vec::with_capacity(snapshot.outputs.len());

    if let Some(output) = snapshot.outputs.get(&primary_output) {
        specs.push(spec_for_output(
            output,
            OutputRole::Primary,
            SurfaceContent {
                system: Some(build_system_cluster(snapshot, config)),
                clock_label: snapshot.system.clock.label.clone(),
                window_groups: groups.clone(),
                context: primary_context.clone(),
                warning: None,
                suggestion: primary_suggestion,
                control_suggestions: suggestions.to_vec(),
                motion_style: config.motion_style(),
                ambient_motion: config.experience.ambient_motion,
            },
        ));
    }

    for (output_name, output) in &snapshot.outputs {
        if *output_name == primary_output {
            continue;
        }

        specs.push(spec_for_output(
            output,
            OutputRole::Reduced,
            SurfaceContent {
                system: None,
                clock_label: snapshot.system.clock.label.clone(),
                window_groups: groups.clone(),
                context: None,
                warning: reduced_warning.clone(),
                suggestion: None,
                control_suggestions: Vec::new(),
                motion_style: config.motion_style(),
                ambient_motion: config.experience.ambient_motion,
            },
        ));
    }

    specs
}

struct SurfaceContent {
    system: Option<SystemCluster>,
    clock_label: String,
    window_groups: Vec<WindowGroupSpec>,
    context: Option<ContextSpec>,
    warning: Option<WarningSpec>,
    suggestion: Option<Suggestion>,
    control_suggestions: Vec<Suggestion>,
    motion_style: MotionStyle,
    ambient_motion: AmbientMotion,
}

fn spec_for_output(
    output: &crate::OutputState,
    role: OutputRole,
    content: SurfaceContent,
) -> SurfaceSpec {
    SurfaceSpec {
        output_name: output.name.clone(),
        role,
        workspaces: output
            .workspaces
            .iter()
            .map(workspace_button_spec)
            .collect(),
        title: TitleSpec {
            app: output
                .focused_window
                .as_ref()
                .and_then(|window| window.app_id.clone())
                .filter(|app| !app.trim().is_empty())
                .unwrap_or_else(|| "Desktop".to_string()),
            text: title_for_output(output),
            window_groups: content.window_groups,
        },
        context: content.context,
        warning: content.warning,
        suggestion: content.suggestion,
        control_suggestions: content.control_suggestions,
        system: content.system,
        clock_label: content.clock_label,
        motion_style: content.motion_style,
        ambient_motion: content.ambient_motion,
    }
}

fn bar_suggestion(
    selected_context: Option<&ContextCard>,
    suggestions: &[Suggestion],
) -> Option<Suggestion> {
    let Some(context) = selected_context else {
        return suggestions.first().cloned();
    };
    let tier = context_tier(context);
    if tier == ContextTier::Ambient {
        return suggestions.first().cloned();
    }

    let related_context = match context_action(context) {
        Some(ContextAction::OpenControlCenter(context)) => Some(context),
        _ => None,
    };
    suggestions
        .iter()
        .find(|suggestion| {
            suggestion.source == SuggestionSource::Live
                && Some(suggestion.context) == related_context
        })
        .cloned()
}

fn workspace_button_spec(workspace: &WorkspaceState) -> WorkspaceButtonSpec {
    WorkspaceButtonSpec {
        id: workspace.id.clone(),
        label: workspace.label.clone(),
        active: workspace.active,
        urgent: workspace.urgent,
    }
}

#[derive(Clone)]
enum ManagedOverlay {
    Popover(gtk::Popover),
    ControlCenter(Rc<ControlCenterView>),
}

impl ManagedOverlay {
    fn hide(&self) {
        match self {
            Self::Popover(popover) => popover.popdown(),
            Self::ControlCenter(control_center) => control_center.dismiss(),
        }
    }

    fn destroy(self) {
        match self {
            Self::Popover(popover) => {
                popover.popdown();
                if popover.parent().is_some() {
                    popover.unparent();
                }
            }
            Self::ControlCenter(control_center) => control_center.destroy(),
        }
    }
}

type PopoverRegistry = Rc<RefCell<BTreeMap<String, ManagedOverlay>>>;

struct ToastRow {
    morph: MorphBin,
    notification: Notification,
}

type ToastRows = Rc<RefCell<BTreeMap<u32, ToastRow>>>;

struct NotificationStackHandle {
    window: gtk::ApplicationWindow,
    content: gtk::Box,
    rows: ToastRows,
    notifications: Vec<Notification>,
    width: i32,
    output: String,
}

pub struct SurfaceRegistry {
    surfaces: BTreeMap<String, SurfaceHandle>,
    notification_stack: Option<NotificationStackHandle>,
    presentation_hidden: bool,
    calendar_sender: Option<Sender<CalendarMonthRequest>>,
    dismissals: Rc<RefCell<Dismissals>>,
    context_dirty: Rc<Cell<bool>>,
    suggestions: Rc<RefCell<SuggestionEngine>>,
    notification_control: Option<NotificationControl>,
}

impl Default for SurfaceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SurfaceRegistry {
    pub fn new() -> Self {
        Self {
            surfaces: BTreeMap::new(),
            notification_stack: None,
            presentation_hidden: false,
            calendar_sender: None,
            dismissals: Rc::new(RefCell::new(Dismissals::default())),
            context_dirty: Rc::new(Cell::new(false)),
            suggestions: Rc::new(RefCell::new(SuggestionEngine::default())),
            notification_control: None,
        }
    }

    pub fn with_calendar_sender(sender: Sender<CalendarMonthRequest>) -> Self {
        Self::with_suggestion_engine(Some(sender), SuggestionEngine::default())
    }

    pub fn with_suggestion_engine(
        calendar_sender: Option<Sender<CalendarMonthRequest>>,
        suggestions: SuggestionEngine,
    ) -> Self {
        Self::with_services(calendar_sender, suggestions, None)
    }

    pub fn with_services(
        calendar_sender: Option<Sender<CalendarMonthRequest>>,
        suggestions: SuggestionEngine,
        notification_control: Option<NotificationControl>,
    ) -> Self {
        Self {
            surfaces: BTreeMap::new(),
            notification_stack: None,
            presentation_hidden: false,
            calendar_sender,
            dismissals: Rc::new(RefCell::new(Dismissals::default())),
            context_dirty: Rc::new(Cell::new(false)),
            suggestions: Rc::new(RefCell::new(suggestions)),
            notification_control,
        }
    }

    pub fn take_context_dirty(&self) -> bool {
        self.context_dirty.replace(false)
    }

    pub fn presentation_hidden(&self) -> bool {
        self.presentation_hidden
    }

    pub fn set_presentation_hidden(&mut self, hidden: bool) {
        if self.presentation_hidden == hidden {
            return;
        }
        self.presentation_hidden = hidden;
        if hidden {
            self.clear();
        }
        self.context_dirty.set(true);
    }

    fn reconcile_toasts(
        &mut self,
        application: &gtk::Application,
        snapshot: &BarSnapshot,
        monitors: &BTreeMap<String, gdk::Monitor>,
    ) {
        let notifications = snapshot
            .system
            .notifications
            .visible_ids
            .iter()
            .filter_map(|id| {
                snapshot
                    .system
                    .notifications
                    .items
                    .iter()
                    .find(|item| item.id == *id)
                    .cloned()
            })
            .collect::<Vec<_>>();
        let focused_output = snapshot
            .focused_output
            .as_deref()
            .filter(|output| monitors.contains_key(*output))
            .or_else(|| monitors.keys().next().map(String::as_str));
        let Some(focused_output) = focused_output else {
            return;
        };
        let Some(monitor) = monitors.get(focused_output) else {
            return;
        };
        let monitor_width = monitor.geometry().width();
        let width = notification_stack_width(monitor_width);
        let island = match self.surfaces.get(focused_output) {
            Some(SurfaceHandle::Primary(surface)) => surface.island_rect(),
            _ => None,
        };
        let origin = stage::toast_origin(island, monitor_width, width);

        // Output or width changes rebuild the stack without motion.
        let stale = self
            .notification_stack
            .as_ref()
            .is_some_and(|stack| stack.output != focused_output || stack.width != width);
        if stale && let Some(stack) = self.notification_stack.take() {
            stack.window.close();
        }
        if notifications.is_empty() && self.notification_stack.is_none() {
            return;
        }
        let animate = !stale;
        let stack = self.notification_stack.get_or_insert_with(|| {
            new_notification_stack(application, monitor, width, focused_output)
        });

        let changes = toast_changes(&stack.notifications, &notifications);
        for id in &changes.removed {
            remove_toast(stack, *id, origin, animate);
        }
        for notification in &notifications {
            if changes.inserted.contains(&notification.id) {
                let morph = MorphBin::new();
                morph.set_follow(false, true);
                morph.set_start_collapsed(animate);
                morph.set_radius(stage::CARD_RADIUS);
                morph.set_shadow(Some(MorphShadow {
                    dy: 8.0,
                    blur: 24.0,
                    alpha: 0.30,
                }));
                morph.set_child(Some(&toast_widget(
                    notification,
                    self.notification_control.as_ref(),
                    width,
                )));
                stack.content.append(&morph);
                if animate {
                    morph.expand_from(MorphOrigin::Window(origin), false, || {});
                }
                stack.rows.borrow_mut().insert(
                    notification.id,
                    ToastRow {
                        morph,
                        notification: notification.clone(),
                    },
                );
            } else if changes.replaced.contains(&notification.id)
                && let Some(row) = stack.rows.borrow_mut().get_mut(&notification.id)
            {
                let widget = toast_widget(notification, self.notification_control.as_ref(), width);
                row.morph.crossfade(|| row.morph.set_child(Some(&widget)));
                row.notification = notification.clone();
            }
        }
        // Order live rows like `notifications`; leaving rows stay where they are unless the
        // live rows themselves are out of order (a new row goes to the top).
        let desired = notifications
            .iter()
            .filter_map(|notification| {
                stack
                    .rows
                    .borrow()
                    .get(&notification.id)
                    .map(|row| row.morph.clone())
            })
            .collect::<Vec<_>>();
        let mut current = Vec::new();
        let mut child = stack.content.first_child();
        while let Some(widget) = child {
            if desired
                .iter()
                .any(|morph| morph.upcast_ref::<gtk::Widget>() == &widget)
            {
                current.push(widget.clone());
            }
            child = widget.next_sibling();
        }
        let in_order = current.len() == desired.len()
            && current
                .iter()
                .zip(&desired)
                .all(|(widget, morph)| widget == morph.upcast_ref::<gtk::Widget>());
        if !in_order {
            let mut previous: Option<gtk::Widget> = None;
            for morph in &desired {
                stack.content.reorder_child_after(morph, previous.as_ref());
                previous = Some(morph.clone().upcast());
            }
        }
        stack.notifications = notifications;
        if !stack.notifications.is_empty() && !stack.window.is_visible() {
            stack.window.set_monitor(Some(monitor));
            stack.window.present();
        }
    }

    pub fn reconcile(
        &mut self,
        application: &gtk::Application,
        snapshot: &BarSnapshot,
        config: &AppConfig,
        action_sender: &Sender<ActionRequest>,
    ) {
        if self.presentation_hidden {
            self.clear();
            return;
        }
        let suggestions = self.suggestions.borrow_mut().suggestions(
            snapshot,
            config,
            snapshot.system.clock.epoch_seconds,
        );
        let desired_specs = surface_specs_with_suggestions(
            snapshot,
            config,
            &self.dismissals.borrow(),
            &suggestions,
        );
        let Some(display) = gdk::Display::default() else {
            return;
        };

        let monitors = monitors_by_connector(&display);
        self.reconcile_toasts(application, snapshot, &monitors);
        let desired_connectors = desired_specs
            .iter()
            .filter(|spec| monitors.contains_key(&spec.output_name))
            .map(|spec| spec.output_name.clone())
            .collect::<BTreeSet<_>>();

        let stale = self
            .surfaces
            .keys()
            .filter(|connector| !desired_connectors.contains(*connector))
            .cloned()
            .collect::<Vec<_>>();
        for connector in stale {
            if let Some(surface) = self.surfaces.remove(&connector) {
                surface.close();
            }
        }

        for spec in desired_specs {
            let Some(monitor) = monitors.get(&spec.output_name) else {
                continue;
            };

            let recreate = self
                .surfaces
                .get(&spec.output_name)
                .is_some_and(|surface| surface.role() != spec.role);

            if recreate && let Some(surface) = self.surfaces.remove(&spec.output_name) {
                surface.close();
            }

            match self.surfaces.get_mut(&spec.output_name) {
                Some(surface) => surface.render(&spec),
                None => {
                    let surface = SurfaceHandle::new(
                        application,
                        monitor,
                        &spec,
                        SurfaceRuntime {
                            action_sender: action_sender.clone(),
                            calendar_sender: self.calendar_sender.clone(),
                            dismissals: self.dismissals.clone(),
                            context_dirty: self.context_dirty.clone(),
                            suggestions: self.suggestions.clone(),
                            notification_control: self.notification_control.clone(),
                        },
                    );
                    self.surfaces.insert(spec.output_name.clone(), surface);
                }
            }
        }
    }

    pub fn clear(&mut self) {
        for (_, surface) in std::mem::take(&mut self.surfaces) {
            surface.close();
        }
        if let Some(stack) = self.notification_stack.take() {
            stack.window.close();
        }
    }

    pub fn handle_completion(
        &mut self,
        completion: &ActionCompletion,
        snapshot: &BarSnapshot,
        config: &AppConfig,
        now_epoch: i64,
    ) -> bool {
        let learned = matches!(completion.result, crate::ActionResult::Completed)
            && self.suggestions.borrow_mut().record_success(
                &completion.origin,
                &completion.intent,
                snapshot,
                &config.routines,
                now_epoch,
            );
        let handled = self
            .surfaces
            .values_mut()
            .any(|surface| surface.handle_completion(completion));
        learned || handled
    }

    pub fn open_control_center(
        &self,
        context: DesktopContext,
        requested_output: Option<&str>,
    ) -> bool {
        self.open_control_center_page(context.into(), requested_output)
    }

    pub(super) fn open_control_center_page(
        &self,
        focus: ControlCenterFocus,
        requested_output: Option<&str>,
    ) -> bool {
        let Some((primary_output, SurfaceHandle::Primary(surface))) = self
            .surfaces
            .iter()
            .find(|(_, surface)| matches!(surface, SurfaceHandle::Primary(_)))
        else {
            return false;
        };
        let target_output = requested_output
            .filter(|output| self.surfaces.contains_key(*output))
            .unwrap_or(primary_output);
        if let Some(display) = gdk::Display::default()
            && let Some(monitor) = monitors_by_connector(&display).get(target_output)
        {
            surface.control_center.set_monitor(monitor);
        }
        surface.control_center.show_page(focus);
        let origin = (target_output == primary_output)
            .then(|| widget_monitor_rect(&surface.system_box))
            .flatten();
        surface.control_center.present_from(origin);
        true
    }

    pub fn toggle_control_center(
        &self,
        context: DesktopContext,
        requested_output: Option<&str>,
    ) -> bool {
        for surface in self.surfaces.values() {
            if let SurfaceHandle::Primary(surface) = surface {
                let center = &surface.control_center;
                let connector = center
                    .window()
                    .monitor()
                    .and_then(|monitor| monitor.connector());
                let same_output =
                    requested_output.is_none_or(|output| connector.as_deref() == Some(output));
                if center.is_visible()
                    && same_output
                    && (context == DesktopContext::Overview
                        || center.current_page() == context.into())
                {
                    center.dismiss();
                    return true;
                }
            }
        }
        self.open_control_center(context, requested_output)
    }

    pub fn close_control_center(&self) {
        for surface in self.surfaces.values() {
            if let SurfaceHandle::Primary(surface) = surface {
                surface.control_center.dismiss_now();
            }
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ToastChanges {
    pub removed: Vec<u32>,
    pub inserted: Vec<u32>,
    pub replaced: Vec<u32>,
}

fn toast_changes(previous: &[Notification], next: &[Notification]) -> ToastChanges {
    let find = |items: &[Notification], id: u32| items.iter().find(|item| item.id == id).cloned();
    ToastChanges {
        removed: previous
            .iter()
            .filter(|item| find(next, item.id).is_none())
            .map(|item| item.id)
            .collect(),
        inserted: next
            .iter()
            .filter(|item| find(previous, item.id).is_none())
            .map(|item| item.id)
            .collect(),
        replaced: next
            .iter()
            .filter(|item| {
                find(previous, item.id)
                    .is_some_and(|old| toast_render_key(&old) != toast_render_key(item))
            })
            .map(|item| item.id)
            .collect(),
    }
}

/// The fields a toast card renders: expiry refreshes (hover-leave) and receipt times do
/// not change the card, so they must not rebuild and crossfade it.
fn toast_render_key(notification: &Notification) -> Notification {
    Notification {
        received_at: 0,
        expires_at: None,
        ..notification.clone()
    }
}

fn new_notification_stack(
    application: &gtk::Application,
    monitor: &gdk::Monitor,
    width: i32,
    output: &str,
) -> NotificationStackHandle {
    let window = gtk::ApplicationWindow::builder()
        .application(application)
        .title("Vigil Notifications")
        .build();
    window.set_decorated(false);
    window.set_default_size(width, -1);
    window.add_css_class("notification-window");
    window.init_layer_shell();
    window.set_namespace(Some("vigil-notifications"));
    window.set_layer(Layer::Overlay);
    // A transparent, full-height stage centered under the island: toasts morph out of
    // the island within it, and the input region covers only the cards below the bar.
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Bottom, true);
    window.set_margin(Edge::Top, SURFACE_MARGIN);
    window.set_margin(Edge::Bottom, SURFACE_MARGIN);
    window.set_exclusive_zone(-1);
    window.set_monitor(Some(monitor));
    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    content.add_css_class("notification-stack");
    content.set_valign(gtk::Align::Start);
    content.set_margin_top(stage::toast_top_in_stage());
    content.set_size_request(width, -1);
    window.set_child(Some(&content));
    let rows: ToastRows = Rc::new(RefCell::new(BTreeMap::new()));
    {
        let content = content.downgrade();
        let window_ref = window.downgrade();
        stage::track_input(&window, move || {
            let (Some(content), Some(window)) = (content.upgrade(), window_ref.upgrade()) else {
                return Vec::new();
            };
            let mut shapes = Vec::new();
            let mut child = content.first_child();
            while let Some(widget) = child {
                if let Some(morph) = widget.downcast_ref::<MorphBin>()
                    && let Some(shape) = morph.shape_in(&window).and_then(stage::toast_input_shape)
                {
                    shapes.push(shape);
                }
                child = widget.next_sibling();
            }
            shapes
        });
    }
    window.present();
    NotificationStackHandle {
        window,
        content,
        rows,
        notifications: Vec::new(),
        width,
        output: output.to_string(),
    }
}

fn toast_widget(
    notification: &Notification,
    control: Option<&NotificationControl>,
    width: i32,
) -> gtk::Box {
    notification_row(
        notification,
        control,
        NotificationPresentation::Toast {
            stack_width: u32::try_from(width).unwrap_or(400),
        },
    )
}

/// Collapses a toast back into the island while its slot springs shut, then removes it.
fn remove_toast(stack: &NotificationStackHandle, id: u32, origin: MorphRect, animate: bool) {
    let Some(row) = stack.rows.borrow_mut().remove(&id) else {
        return;
    };
    // Weak captures: `finish` lives in the morph's settled callbacks, so strong ones
    // would keep the morph and the stack content alive through a cycle.
    let content = stack.content.downgrade();
    let window = stack.window.downgrade();
    let morph = row.morph;
    let finish = {
        let morph = morph.downgrade();
        move || {
            let (Some(content), Some(morph)) = (content.upgrade(), morph.upgrade()) else {
                return;
            };
            content.remove(&morph);
            if content.first_child().is_none()
                && let Some(window) = window.upgrade()
            {
                window.set_visible(false);
            }
        }
    };
    if !animate {
        finish();
        return;
    }
    let child = morph.child();
    morph.collapse_into(MorphOrigin::Window(origin), true, || {
        if let Some(child) = child {
            child.set_visible(false);
        }
    });
    morph.on_settled(finish);
}

fn notification_stack_width(monitor_width: i32) -> i32 {
    monitor_width.saturating_sub(24).clamp(1, 400)
}

fn monitors_by_connector(display: &gdk::Display) -> BTreeMap<String, gdk::Monitor> {
    display
        .monitors()
        .snapshot()
        .into_iter()
        .filter_map(|object| object.downcast::<gdk::Monitor>().ok())
        .filter_map(|monitor| {
            monitor
                .connector()
                .map(|connector| (connector.to_string(), monitor))
        })
        .collect()
}

enum SurfaceHandle {
    Primary(PrimarySurface),
    Reduced(ReducedSurface),
}

struct SurfaceRuntime {
    action_sender: Sender<ActionRequest>,
    calendar_sender: Option<Sender<CalendarMonthRequest>>,
    dismissals: Rc<RefCell<Dismissals>>,
    context_dirty: Rc<Cell<bool>>,
    suggestions: Rc<RefCell<SuggestionEngine>>,
    notification_control: Option<NotificationControl>,
}

impl SurfaceHandle {
    fn new(
        application: &gtk::Application,
        monitor: &gdk::Monitor,
        spec: &SurfaceSpec,
        runtime: SurfaceRuntime,
    ) -> Self {
        match spec.role {
            OutputRole::Primary => {
                Self::Primary(PrimarySurface::new(application, monitor, spec, runtime))
            }
            OutputRole::Reduced => Self::Reduced(ReducedSurface::new(
                application,
                monitor,
                spec,
                runtime.action_sender,
            )),
        }
    }

    fn role(&self) -> OutputRole {
        match self {
            Self::Primary(_) => OutputRole::Primary,
            Self::Reduced(_) => OutputRole::Reduced,
        }
    }

    fn render(&mut self, spec: &SurfaceSpec) {
        match self {
            Self::Primary(surface) => surface.render(spec),
            Self::Reduced(surface) => surface.render(spec),
        }
    }

    fn handle_completion(&mut self, completion: &ActionCompletion) -> bool {
        match self {
            Self::Primary(surface) => surface.handle_completion(completion),
            Self::Reduced(_) => false,
        }
    }

    fn close(self) {
        match self {
            Self::Primary(surface) => surface.close(),
            Self::Reduced(surface) => surface.close(),
        }
    }
}

pub struct PrimarySurface {
    window: gtk::ApplicationWindow,
    system_box: gtk::Box,
    workspaces: WorkspaceStrip,
    app_label: gtk::Label,
    title_label: gtk::Label,
    title_groups: Rc<RefCell<Vec<WindowGroupSpec>>>,
    context_stack: gtk::Stack,
    context_row: gtk::Box,
    context_icon: gtk::Image,
    context_label: gtk::Label,
    context_media_controls: gtk::Box,
    context_media_play: gtk::Button,
    current_context: Rc<RefCell<Option<ContextSpec>>>,
    suggestion_actions: gtk::Box,
    context_morph: MorphBin,
    center_slot: gtk::Box,
    suggestion_action: gtk::Button,
    current_suggestion: Rc<RefCell<Option<Suggestion>>>,
    system_items: gtk::Box,
    status_buttons: BTreeMap<SystemModuleId, StatusButtonView>,
    status_button_order: Vec<SystemModuleId>,
    control_center: Rc<ControlCenterView>,
    popover_coordinator: Rc<RefCell<PopoverCoordinator>>,
    popover_registry: PopoverRegistry,
    action_sender: Sender<ActionRequest>,
    notification_control: Option<NotificationControl>,
    current_spec: Option<SurfaceSpec>,
}

impl PrimarySurface {
    fn new(
        application: &gtk::Application,
        monitor: &gdk::Monitor,
        spec: &SurfaceSpec,
        runtime: SurfaceRuntime,
    ) -> Self {
        let SurfaceRuntime {
            action_sender,
            calendar_sender,
            dismissals,
            context_dirty,
            suggestions,
            notification_control,
        } = runtime;
        let window = base_window(application, monitor);
        window.add_css_class("bar-window");
        window.add_css_class("primary-bar");
        if monitor.geometry().width() < 1_600 {
            window.add_css_class("compact-bar");
        }
        let popover_coordinator = Rc::new(RefCell::new(PopoverCoordinator::default()));
        let popover_registry = Rc::new(RefCell::new(BTreeMap::new()));

        let root = gtk::CenterBox::new();
        root.add_css_class("bar-root");
        root.set_margin_start(12);
        root.set_margin_end(12);
        root.set_size_request(-1, BAR_HEIGHT);

        let left = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        left.add_css_class("bar-island");
        left.add_css_class("workspace-island");
        let center_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        center_slot.add_css_class("bar-island");
        center_slot.add_css_class("context-island");
        let right = gtk::Box::new(gtk::Orientation::Horizontal, 8);

        left.set_halign(gtk::Align::Start);
        center_slot.set_halign(gtk::Align::Center);
        center_slot.set_hexpand(false);
        right.set_halign(gtk::Align::End);

        left.append(&launcher_button(
            action_sender.clone(),
            spec.output_name.clone(),
            popover_coordinator.clone(),
            popover_registry.clone(),
        ));
        let workspaces = WorkspaceStrip::new(action_sender.clone(), spec.output_name.clone());
        left.append(workspaces.widget());
        let app_label = gtk::Label::new(None);
        app_label.add_css_class("app-label");
        app_label.set_max_width_chars(22);
        app_label.set_ellipsize(EllipsizeMode::End);
        left.append(&app_label);

        let title_button = gtk::Button::new();
        title_button.add_css_class("title-button");
        title_button.set_has_frame(false);
        title_button.set_hexpand(true);
        title_button.set_halign(gtk::Align::Fill);
        let title_label = gtk::Label::new(None);
        title_label.add_css_class("title-label");
        title_label.set_hexpand(false);
        title_label.set_max_width_chars(48);
        title_label.set_ellipsize(EllipsizeMode::End);
        title_label.set_xalign(0.5);
        let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        title_row.set_hexpand(false);
        title_row.set_halign(gtk::Align::Center);
        let title_icon = gtk::Image::from_icon_name("focus-windows-symbolic");
        title_icon.add_css_class("context-icon");
        title_icon.set_pixel_size(16);
        title_icon.set_size_request(16, 16);
        title_row.append(&title_icon);
        title_row.append(&title_label);
        title_button.set_child(Some(&title_row));

        let title_groups = Rc::new(RefCell::new(Vec::new()));
        install_title_interactions(
            &title_button,
            title_groups.clone(),
            action_sender.clone(),
            spec.output_name.clone(),
            popover_coordinator.clone(),
            popover_registry.clone(),
        );

        let context_stack = gtk::Stack::new();
        context_stack.set_halign(gtk::Align::Fill);
        context_stack.set_hexpand(true);
        context_stack.set_hhomogeneous(false);
        context_stack.set_vhomogeneous(true);
        context_stack.set_transition_type(gtk::StackTransitionType::None);
        let context_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        context_row.add_css_class("context-chip");
        context_row.set_hexpand(false);
        context_row.set_halign(gtk::Align::Center);
        context_row.set_accessible_role(gtk::AccessibleRole::Button);
        context_row.update_property(&[gtk::accessible::Property::Description(
            "Open this context; use secondary click to dismiss it",
        )]);
        let context_icon = gtk::Image::new();
        context_icon.add_css_class("context-icon");
        context_icon.set_pixel_size(16);
        context_icon.set_size_request(16, 16);
        let context_label = gtk::Label::new(None);
        context_label.set_hexpand(false);
        context_label.set_xalign(0.5);
        context_label.set_max_width_chars(CENTER_TEXT_MAX_CHARS);
        context_label.set_ellipsize(EllipsizeMode::End);
        context_row.append(&context_icon);
        context_row.append(&context_label);
        context_row.set_tooltip_text(Some("Click to open · Right-click to dismiss"));
        let context_media_controls = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        context_media_controls.add_css_class("context-media-controls");
        context_media_controls.set_visible(false);
        let context_media_previous = icon_button("media-skip-backward-symbolic", "Previous track");
        let context_media_play = icon_button("media-playback-start-symbolic", "Play or pause");
        let context_media_next = icon_button("media-skip-forward-symbolic", "Next track");
        context_media_controls.append(&context_media_previous);
        context_media_controls.append(&context_media_play);
        context_media_controls.append(&context_media_next);
        context_row.append(&context_media_controls);
        let suggestion_actions = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        suggestion_actions.add_css_class("suggestion-actions");
        let suggestion_action = gtk::Button::new();
        suggestion_action.add_css_class("suggestion-action");
        suggestion_action.set_has_frame(false);
        let suggestion_dismiss = icon_button("window-close-symbolic", "Dismiss this suggestion");
        suggestion_dismiss.add_css_class("suggestion-dismiss");
        suggestion_actions.append(&suggestion_action);
        suggestion_actions.append(&suggestion_dismiss);
        suggestion_actions.set_visible(false);
        context_row.append(&suggestion_actions);
        let current_context = Rc::new(RefCell::new(None::<ContextSpec>));
        let current_suggestion = Rc::new(RefCell::new(None::<Suggestion>));
        context_stack.add_named(&title_button, Some("title"));
        context_stack.add_named(&context_row, Some("context"));
        context_stack.set_visible_child_name("title");

        let context_morph = MorphBin::new();
        context_morph.set_follow(true, false);
        context_morph.set_child(Some(&context_stack));
        center_slot.append(&context_morph);

        let system_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        system_box.add_css_class("bar-island");
        system_box.add_css_class("system-cluster");
        right.append(&system_box);
        let system_items = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        system_items.add_css_class("system-items");
        system_box.append(&system_items);

        let initial_system = spec.system.as_ref().expect("primary system spec");
        let control_center = Rc::new(ControlCenterView::new(
            application,
            monitor,
            &spec.output_name,
            SURFACE_MARGIN,
            SURFACE_MARGIN,
            initial_system.control_center(),
            action_sender.clone(),
            calendar_sender,
            suggestions.clone(),
            context_dirty.clone(),
            notification_control.clone(),
        ));
        register_control_window(
            "control-center",
            control_center.clone(),
            popover_coordinator.clone(),
            popover_registry.clone(),
        );
        install_context_interactions(
            &context_row,
            current_context.clone(),
            current_suggestion.clone(),
            dismissals,
            context_dirty.clone(),
            action_sender.clone(),
            spec.output_name.clone(),
            control_center.clone(),
            popover_coordinator.clone(),
            popover_registry.clone(),
            suggestions.clone(),
        );
        {
            let current_suggestion = current_suggestion.clone();
            let action_sender = action_sender.clone();
            let control_center = control_center.clone();
            suggestion_action.connect_clicked(move |button| {
                let Some(suggestion) = current_suggestion.borrow().clone() else {
                    return;
                };
                match suggestion.action {
                    SuggestionAction::Intent(intent) => {
                        button.add_css_class("is-pending");
                        let _ = action_sender.send(ActionRequest {
                            origin: format!("suggestion-action|{}", suggestion.id),
                            intent,
                        });
                    }
                    SuggestionAction::OpenControlCenter(context) => {
                        control_center.show_page(context.into());
                        control_center.present();
                    }
                }
            });
        }
        {
            let current_suggestion = current_suggestion.clone();
            let suggestions = suggestions.clone();
            let context_dirty = context_dirty.clone();
            suggestion_dismiss.connect_clicked(move |_| {
                let Some(suggestion) = current_suggestion.borrow().clone() else {
                    return;
                };
                if suggestions
                    .borrow_mut()
                    .dismiss(&suggestion.id, current_epoch_seconds())
                {
                    context_dirty.set(true);
                }
            });
        }
        for (button, action) in [
            (&context_media_previous, MediaControlAction::Previous),
            (&context_media_play, MediaControlAction::PlayPause),
            (&context_media_next, MediaControlAction::Next),
        ] {
            let control_center = control_center.clone();
            let action_sender = action_sender.clone();
            button.connect_clicked(move |_| {
                let Some(player) = control_center.media_player() else {
                    return;
                };
                let _ = action_sender.send(ActionRequest {
                    origin: "context-media".to_string(),
                    intent: ActionIntent::ControlMedia { player, action },
                });
            });
        }
        let mut status_buttons = BTreeMap::new();
        for module in initial_system.modules() {
            let status_button = build_status_button(
                &module.button,
                &action_sender,
                control_center.clone(),
                popover_coordinator.clone(),
                popover_registry.clone(),
                notification_control.clone(),
            );
            system_items.append(&status_button.button);
            status_buttons.insert(module.button.id, status_button);
        }
        let status_button_order = initial_system
            .modules()
            .iter()
            .map(|module| module.button.id)
            .collect();

        root.set_start_widget(Some(&left));
        root.set_center_widget(Some(&center_slot));
        root.set_end_widget(Some(&right));

        window.set_child(Some(&root));
        install_escape_dismiss(
            &window,
            popover_coordinator.clone(),
            popover_registry.clone(),
        );
        window.present();
        flash_class(&root, "bar-entering", 620);

        let mut surface = Self {
            window,
            system_box,
            workspaces,
            app_label,
            title_label,
            title_groups,
            context_stack,
            context_row,
            context_icon,
            context_label,
            context_media_controls,
            context_media_play,
            current_context,
            suggestion_actions,
            context_morph,
            center_slot,
            suggestion_action,
            current_suggestion,
            system_items,
            status_buttons,
            status_button_order,
            control_center,
            popover_coordinator,
            popover_registry,
            action_sender,
            notification_control,
            current_spec: None,
        };
        surface.render(spec);
        surface
    }

    pub fn render(&mut self, spec: &SurfaceSpec) {
        let plan = RenderPlan::between(self.current_spec.as_ref(), spec);
        if plan == RenderPlan::default() {
            return;
        }

        if plan.workspaces {
            self.workspaces.render(&spec.workspaces);
        }
        if plan.title {
            self.app_label.set_label(&spec.title.app);
            self.title_label.set_label(&spec.title.text);
            *self.title_groups.borrow_mut() = spec.title.window_groups.clone();
        }
        if plan.system
            && let Some(system) = spec.system.as_ref()
        {
            self.render_system_modules(system);
        }
        if plan.suggestions {
            self.control_center
                .update_suggestions(&spec.control_suggestions);
        }
        if plan.motion {
            apply_motion_style(&self.window, spec.motion_style, spec.ambient_motion);
        }

        if plan.context {
            // Text and value refreshes (timer ticks, `now_epoch`) apply in place; the
            // width still springs through measure. Only new content crossfades.
            let crossfade = self.current_spec.as_ref().is_some_and(|previous| {
                context_identity(previous.context.as_ref(), previous.suggestion.as_ref())
                    != context_identity(spec.context.as_ref(), spec.suggestion.as_ref())
            });
            let apply = || {
                *self.current_context.borrow_mut() = spec.context.clone();
                *self.current_suggestion.borrow_mut() = spec.suggestion.clone();
                self.suggestion_action.remove_css_class("is-pending");
                self.suggestion_action.remove_css_class("is-success");
                self.suggestion_action.remove_css_class("is-failure");
                if let Some(suggestion) = spec.suggestion.as_ref() {
                    self.suggestion_action.set_label(&suggestion.action_label);
                    self.suggestion_action
                        .set_tooltip_text(Some(&suggestion.reason));
                    self.suggestion_actions.set_visible(true);
                    self.context_row.add_css_class("proactive");
                } else {
                    self.suggestion_action.set_label("");
                    self.suggestion_action.set_tooltip_text(None);
                    self.suggestion_actions.set_visible(false);
                    self.context_row.remove_css_class("proactive");
                }

                if let Some(context) = spec.context.as_ref() {
                    self.context_icon.set_icon_name(Some(&context.icon_name));
                    self.context_label.set_label(&context.text);
                    set_context_accessibility(&self.context_row, &context.text);
                    apply_tier_classes(&self.context_row, context.tier);
                    apply_context_motion_classes(&self.context_row, &context.card);
                    self.context_stack.set_visible_child_name("context");
                    if let ContextCard::Media { status, .. } = &context.card {
                        self.context_media_play
                            .set_icon_name(media_playback_icon(status));
                        self.context_media_controls.set_visible(true);
                    } else {
                        self.context_media_controls.set_visible(false);
                    }
                } else if let Some(suggestion) = spec.suggestion.as_ref() {
                    self.context_icon.set_icon_name(Some(&suggestion.icon_name));
                    self.context_label.set_label(&suggestion.title);
                    set_context_accessibility(&self.context_row, &suggestion.title);
                    clear_tier_classes(&self.context_row);
                    clear_context_motion_classes(&self.context_row);
                    self.context_row.add_css_class("suggestion");
                    self.context_stack.set_visible_child_name("context");
                    self.context_media_controls.set_visible(false);
                } else {
                    self.context_icon.set_icon_name(None);
                    self.context_label.set_label("");
                    clear_tier_classes(&self.context_row);
                    clear_context_motion_classes(&self.context_row);
                    self.context_stack.set_visible_child_name("title");
                    self.context_media_controls.set_visible(false);
                }
            };
            if crossfade {
                self.context_morph.crossfade(apply);
            } else {
                apply();
            }
        }

        self.current_spec = Some(spec.clone());
    }

    /// The center island in monitor coordinates, used as the origin of toast morphs.
    fn island_rect(&self) -> Option<MorphRect> {
        widget_monitor_rect(&self.center_slot)
    }

    pub fn handle_completion(&mut self, completion: &ActionCompletion) -> bool {
        let suggestion_handled = if let Some(id) =
            completion.origin.strip_prefix("suggestion-action|")
            && self
                .current_suggestion
                .borrow()
                .as_ref()
                .is_some_and(|suggestion| suggestion.id == id)
        {
            self.suggestion_action.remove_css_class("is-pending");
            let class_name = match completion.result {
                crate::ActionResult::Completed => "is-success",
                crate::ActionResult::Failed { .. } => "is-failure",
            };
            flash_class(&self.suggestion_action, class_name, 720);
            true
        } else {
            false
        };
        let control_handled = self.control_center.handle_completion(completion);
        suggestion_handled || control_handled
    }

    fn close(self) {
        destroy_popovers(&self.popover_registry);
        self.window.close();
    }

    fn render_system_modules(&mut self, system: &SystemCluster) {
        self.control_center.update(system.control_center());

        let new_order: Vec<SystemModuleId> = system
            .modules()
            .iter()
            .map(|module| module.button.id)
            .collect();
        if new_order != self.status_button_order {
            self.rebuild_system_items(system);
            return;
        }

        for module in system.modules() {
            if let Some(status_button) = self.status_buttons.get_mut(&module.button.id) {
                status_button.update(&module.button);
            }
        }
    }

    fn rebuild_system_items(&mut self, system: &SystemCluster) {
        while let Some(child) = self.system_items.first_child() {
            self.system_items.remove(&child);
        }
        self.status_buttons.clear();

        for module in system.modules() {
            let status_button = build_status_button(
                &module.button,
                &self.action_sender,
                self.control_center.clone(),
                self.popover_coordinator.clone(),
                self.popover_registry.clone(),
                self.notification_control.clone(),
            );
            self.system_items.append(&status_button.button);
            self.status_buttons.insert(module.button.id, status_button);
        }
        self.status_button_order = system
            .modules()
            .iter()
            .map(|module| module.button.id)
            .collect();
    }
}

pub struct ReducedSurface {
    window: gtk::ApplicationWindow,
    workspaces: WorkspaceStrip,
    app_label: gtk::Label,
    title_label: gtk::Label,
    title_groups: Rc<RefCell<Vec<WindowGroupSpec>>>,
    warning_label: gtk::Label,
    clock_label: gtk::Label,
    popover_registry: PopoverRegistry,
    current_spec: Option<SurfaceSpec>,
}

impl ReducedSurface {
    pub fn new(
        application: &gtk::Application,
        monitor: &gdk::Monitor,
        spec: &SurfaceSpec,
        action_sender: Sender<ActionRequest>,
    ) -> Self {
        let window = base_window(application, monitor);
        window.add_css_class("bar-window");
        window.add_css_class("reduced-bar");
        if monitor.geometry().width() < 1_600 {
            window.add_css_class("compact-bar");
        }
        let popover_coordinator = Rc::new(RefCell::new(PopoverCoordinator::default()));
        let popover_registry = Rc::new(RefCell::new(BTreeMap::new()));

        let root = gtk::Grid::new();
        root.add_css_class("bar-root");
        root.set_column_spacing(12);
        root.set_margin_start(12);
        root.set_margin_end(12);
        root.set_size_request(-1, BAR_HEIGHT);

        let left = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        left.add_css_class("bar-island");
        left.add_css_class("workspace-island");
        left.set_hexpand(true);
        left.set_halign(gtk::Align::Start);
        let right = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        right.add_css_class("bar-island");
        right.add_css_class("reduced-status-island");
        right.set_halign(gtk::Align::End);

        left.append(&launcher_button(
            action_sender.clone(),
            spec.output_name.clone(),
            popover_coordinator.clone(),
            popover_registry.clone(),
        ));
        let workspaces = WorkspaceStrip::new(action_sender.clone(), spec.output_name.clone());
        left.append(workspaces.widget());
        let app_label = gtk::Label::new(None);
        app_label.add_css_class("app-label");
        app_label.set_max_width_chars(18);
        app_label.set_ellipsize(EllipsizeMode::End);
        left.append(&app_label);
        let title_button = gtk::Button::new();
        title_button.add_css_class("title-button");
        title_button.set_has_frame(false);
        title_button.set_hexpand(true);
        title_button.set_halign(gtk::Align::Fill);
        let title_label = gtk::Label::new(None);
        title_label.add_css_class("title-label");
        title_label.set_hexpand(true);
        title_label.set_max_width_chars(42);
        title_label.set_ellipsize(EllipsizeMode::End);
        title_label.set_xalign(0.0);
        title_button.set_child(Some(&title_label));

        let title_groups = Rc::new(RefCell::new(Vec::new()));
        install_title_interactions(
            &title_button,
            title_groups.clone(),
            action_sender.clone(),
            spec.output_name.clone(),
            popover_coordinator.clone(),
            popover_registry.clone(),
        );

        let warning_label = gtk::Label::new(None);
        warning_label.add_css_class("warning-chip");
        warning_label.set_visible(false);
        warning_label.set_ellipsize(EllipsizeMode::End);

        let clock_label = gtk::Label::new(None);
        clock_label.add_css_class("clock-chip");

        left.append(&title_button);
        right.append(&warning_label);
        right.append(&clock_label);
        root.attach(&left, 0, 0, 1, 1);
        root.attach(&right, 1, 0, 1, 1);

        window.set_child(Some(&root));
        install_escape_dismiss(&window, popover_coordinator, popover_registry.clone());
        window.present();
        flash_class(&root, "bar-entering", 520);

        let mut surface = Self {
            window,
            workspaces,
            app_label,
            title_label,
            title_groups,
            warning_label,
            clock_label,
            popover_registry,
            current_spec: None,
        };
        surface.render(spec);
        surface
    }

    pub fn render(&mut self, spec: &SurfaceSpec) {
        let plan = RenderPlan::between(self.current_spec.as_ref(), spec);
        if plan == RenderPlan::default() {
            return;
        }

        if plan.workspaces {
            self.workspaces.render(&spec.workspaces);
        }
        if plan.title {
            self.app_label.set_label(&spec.title.app);
            self.title_label.set_label(&spec.title.text);
            *self.title_groups.borrow_mut() = spec.title.window_groups.clone();
        }
        if plan.clock {
            self.clock_label.set_label(&spec.clock_label);
        }

        if plan.warning {
            if let Some(warning) = spec.warning.as_ref() {
                self.warning_label.set_label(&warning.text);
                apply_tier_classes(&self.warning_label, warning.tier);
                self.warning_label.set_visible(true);
            } else {
                self.warning_label.set_label("");
                clear_tier_classes(&self.warning_label);
                self.warning_label.set_visible(false);
            }
        }
        if plan.motion {
            apply_motion_style(&self.window, spec.motion_style, spec.ambient_motion);
        }

        self.current_spec = Some(spec.clone());
    }

    fn close(self) {
        destroy_popovers(&self.popover_registry);
        self.window.close();
    }
}

fn base_window(application: &gtk::Application, monitor: &gdk::Monitor) -> gtk::ApplicationWindow {
    let width = bar_window_width_for_monitor_width(monitor.geometry().width());
    let window = gtk::ApplicationWindow::builder()
        .application(application)
        .title("Vigil")
        .build();
    window.set_decorated(false);
    window.set_resizable(false);
    window.set_default_size(width, BAR_HEIGHT);
    window.init_layer_shell();
    window.set_namespace(Some("vigil"));
    window.set_layer(Layer::Top);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Left, true);
    window.set_anchor(Edge::Right, true);
    window.set_exclusive_zone(BAR_HEIGHT);
    window.set_margin(Edge::Top, SURFACE_MARGIN);
    window.set_margin(Edge::Left, SURFACE_MARGIN);
    window.set_margin(Edge::Right, SURFACE_MARGIN);
    window.set_monitor(Some(monitor));
    window
}

fn bar_window_width_for_monitor_width(monitor_width: i32) -> i32 {
    monitor_width.saturating_sub(SURFACE_MARGIN * 2).max(1)
}

struct WorkspaceStrip {
    container: gtk::Box,
    views: BTreeMap<String, WorkspaceButtonView>,
    order: Vec<String>,
    action_sender: Sender<ActionRequest>,
    output_name: String,
}

struct WorkspaceButtonView {
    button: gtk::Button,
    spec: WorkspaceButtonSpec,
}

impl WorkspaceStrip {
    fn new(action_sender: Sender<ActionRequest>, output_name: String) -> Self {
        let container = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        container.add_css_class("workspace-strip");
        install_workspace_scroll(&container, action_sender.clone(), output_name.clone());
        Self {
            container,
            views: BTreeMap::new(),
            order: Vec::new(),
            action_sender,
            output_name,
        }
    }

    fn widget(&self) -> &gtk::Box {
        &self.container
    }

    fn render(&mut self, workspaces: &[WorkspaceButtonSpec]) {
        let next_order = workspaces
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect::<Vec<_>>();
        let desired = next_order.iter().cloned().collect::<BTreeSet<_>>();
        self.views.retain(|id, _| desired.contains(id));

        for spec in workspaces {
            if let Some(view) = self.views.get_mut(&spec.id) {
                view.update(spec);
                continue;
            }
            let view = WorkspaceButtonView::new(
                spec,
                self.action_sender.clone(),
                self.output_name.clone(),
            );
            flash_class(&view.button, "workspace-new", 420);
            self.views.insert(spec.id.clone(), view);
        }

        if self.order != next_order {
            while let Some(child) = self.container.first_child() {
                self.container.remove(&child);
            }
            for id in &next_order {
                if let Some(view) = self.views.get(id) {
                    self.container.append(&view.button);
                }
            }
            self.order = next_order;
        }
    }
}

impl WorkspaceButtonView {
    fn new(
        spec: &WorkspaceButtonSpec,
        action_sender: Sender<ActionRequest>,
        output_name: String,
    ) -> Self {
        let button = gtk::Button::with_label(&spec.label);
        button.add_css_class("workspace-button");
        button.set_size_request(WORKSPACE_BUTTON_MIN_WIDTH, WORKSPACE_BUTTON_MIN_HEIGHT);
        button.set_has_frame(spec.active || spec.urgent);
        let workspace_id = spec.id.clone();
        button.set_tooltip_text(Some(&format!("Switch to workspace {}", spec.label)));
        button.connect_clicked(move |_| {
            let _ = action_sender.send(ActionRequest {
                origin: format!("workspace:{output_name}:{workspace_id}"),
                intent: ActionIntent::SwitchWorkspace {
                    output: output_name.clone(),
                    workspace: workspace_id.clone(),
                },
            });
        });
        let mut view = Self {
            button,
            spec: spec.clone(),
        };
        view.update(spec);
        view
    }

    fn update(&mut self, spec: &WorkspaceButtonSpec) {
        let activated = !self.spec.active && spec.active;
        self.button.set_label(&spec.label);
        update_state_class(&self.button, "active", spec.active);
        update_state_class(&self.button, "urgent", spec.urgent);
        self.button.set_has_frame(spec.active || spec.urgent);
        if activated {
            flash_class(&self.button, "workspace-activated", 520);
        }
        self.spec = spec.clone();
    }
}

fn install_workspace_scroll(
    container: &gtk::Box,
    action_sender: Sender<ActionRequest>,
    output_name: String,
) {
    let scroll = gtk::EventControllerScroll::new(
        gtk::EventControllerScrollFlags::VERTICAL
            | gtk::EventControllerScrollFlags::HORIZONTAL
            | gtk::EventControllerScrollFlags::DISCRETE,
    );
    scroll.connect_scroll(move |_, dx, dy| {
        let Some(direction) = scroll_direction(dx, dy) else {
            return glib::Propagation::Proceed;
        };

        let _ = action_sender.send(ActionRequest {
            origin: format!("workspace-scroll:{output_name}"),
            intent: ActionIntent::CycleWorkspace {
                output: output_name.clone(),
                direction,
            },
        });
        glib::Propagation::Stop
    });
    container.add_controller(scroll);
}

fn launcher_button(
    action_sender: Sender<ActionRequest>,
    output_name: String,
    coordinator: Rc<RefCell<PopoverCoordinator>>,
    registry: PopoverRegistry,
) -> gtk::Button {
    let button = icon_button(
        "system-search-symbolic",
        "Search apps, files and actions · Super+Space\nRight-click for windows · Super+Tab",
    );
    button.add_css_class("launcher-button");
    button.update_property(&[gtk::accessible::Property::Label("Open desktop search")]);
    let sender = action_sender.clone();
    let output = output_name.clone();
    let click_coordinator = coordinator.clone();
    let click_registry = registry.clone();
    button.connect_clicked(move |_| {
        popdown_active_popover(&click_coordinator, &click_registry);
        let _ = sender.send(ActionRequest {
            origin: format!("launcher:{output}"),
            intent: ActionIntent::OpenLauncher {
                output: output.clone(),
            },
        });
    });
    let secondary = gtk::GestureClick::new();
    secondary.set_button(3);
    secondary.connect_pressed(move |gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        popdown_active_popover(&coordinator, &registry);
        let _ = action_sender.send(ActionRequest {
            origin: format!("launcher-windows:{output_name}"),
            intent: ActionIntent::OpenWindowSearch {
                output: output_name.clone(),
            },
        });
    });
    button.add_controller(secondary);
    button
}

fn install_title_interactions(
    button: &gtk::Button,
    groups: Rc<RefCell<Vec<WindowGroupSpec>>>,
    action_sender: Sender<ActionRequest>,
    output_name: String,
    coordinator: Rc<RefCell<PopoverCoordinator>>,
    registry: PopoverRegistry,
) {
    button.set_tooltip_text(Some("Open windows · Right-click to search · Super+Tab"));
    let popover = gtk::Popover::new();
    popover.set_has_arrow(false);
    popover.set_parent(button);
    let popover_id = format!("title:{output_name}");
    register_popover(&popover_id, &popover, coordinator.clone(), registry.clone());

    let groups_for_click = groups.clone();
    let sender_for_click = action_sender.clone();
    let popover_output = output_name.clone();
    let popover_id_for_click = popover_id.clone();
    button.connect_clicked(move |_| {
        rebuild_window_popover(
            &popover,
            &groups_for_click.borrow(),
            &sender_for_click,
            &popover_output,
        );
        show_managed_popover(
            &popover_id_for_click,
            &popover,
            coordinator.clone(),
            registry.clone(),
        );
    });

    let secondary = gtk::GestureClick::new();
    secondary.set_button(3);
    let secondary_output = output_name.clone();
    secondary.connect_pressed(move |_, _, _, _| {
        let _ = action_sender.send(ActionRequest {
            origin: format!("title-secondary:{secondary_output}"),
            intent: ActionIntent::OpenWindowSearch {
                output: secondary_output.clone(),
            },
        });
    });
    button.add_controller(secondary);
}

#[allow(clippy::too_many_arguments)]
fn install_context_interactions(
    context_row: &gtk::Box,
    current_context: Rc<RefCell<Option<ContextSpec>>>,
    current_suggestion: Rc<RefCell<Option<Suggestion>>>,
    dismissals: Rc<RefCell<Dismissals>>,
    context_dirty: Rc<Cell<bool>>,
    action_sender: Sender<ActionRequest>,
    output_name: String,
    control_center: Rc<ControlCenterView>,
    coordinator: Rc<RefCell<PopoverCoordinator>>,
    registry: PopoverRegistry,
    suggestions: Rc<RefCell<SuggestionEngine>>,
) {
    let primary = gtk::GestureClick::new();
    primary.set_button(1);
    let context_for_primary = current_context.clone();
    let context_row_for_origin = context_row.clone();
    primary.connect_pressed(move |_, _, _, _| {
        let Some(context) = context_for_primary.borrow().clone() else {
            return;
        };
        match context_action(&context.card) {
            Some(ContextAction::OpenControlCenter(desktop_context)) => {
                control_center.show_page(desktop_context.into());
                show_managed_window(
                    "control-center",
                    control_center.clone(),
                    coordinator.clone(),
                    registry.clone(),
                    widget_monitor_rect(&context_row_for_origin),
                );
            }
            Some(ContextAction::FocusWindow { output, window_id }) => {
                let _ = action_sender.send(ActionRequest {
                    origin: format!("context-click:{output_name}"),
                    intent: ActionIntent::FocusWindow { output, window_id },
                });
            }
            None => {}
        }
    });
    context_row.add_controller(primary);

    let secondary = gtk::GestureClick::new();
    secondary.set_button(3);
    secondary.connect_pressed(move |_, _, _, _| {
        if let Some(suggestion) = current_suggestion.borrow().clone() {
            if suggestions
                .borrow_mut()
                .dismiss(&suggestion.id, current_epoch_seconds())
            {
                context_dirty.set(true);
            }
            return;
        }
        let Some(context) = current_context.borrow().clone() else {
            return;
        };
        dismissals.borrow_mut().dismiss(
            &context.card,
            context.now_epoch,
            context.critical_snooze_seconds,
        );
        context_dirty.set(true);
    });
    context_row.add_controller(secondary);
}

fn icon_button(icon_name: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon_name);
    button.set_tooltip_text(Some(tooltip));
    button.set_has_frame(false);
    button
}

fn rebuild_window_popover(
    popover: &gtk::Popover,
    groups: &[WindowGroupSpec],
    action_sender: &Sender<ActionRequest>,
    origin_output: &str,
) {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
    column.set_margin_top(8);
    column.set_margin_bottom(8);
    column.set_margin_start(8);
    column.set_margin_end(8);

    for group in groups {
        let heading = gtk::Label::new(Some(&format!(
            "{} / {}",
            group.output_name, group.workspace_label
        )));
        heading.set_xalign(0.0);
        column.append(&heading);

        for item in &group.windows {
            let button = gtk::Button::with_label(&item.title);
            button.set_has_frame(false);
            button.set_halign(gtk::Align::Fill);
            let sender = action_sender.clone();
            let output = item.output_name.clone();
            let window_id = item.window_id.clone();
            let origin = format!("window-popover:{origin_output}:{window_id}");
            let popover = popover.clone();
            button.connect_clicked(move |_| {
                let _ = sender.send(ActionRequest {
                    origin: origin.clone(),
                    intent: ActionIntent::FocusWindow {
                        output: output.clone(),
                        window_id: window_id.clone(),
                    },
                });
                popover.popdown();
            });
            column.append(&button);
        }
    }

    popover.set_child(Some(&column));
}

fn apply_tier_classes(widget: &impl IsA<gtk::Widget>, tier: crate::ContextTier) {
    clear_tier_classes(widget);
    match tier {
        crate::ContextTier::Critical => widget.add_css_class("critical"),
        crate::ContextTier::Imminent => widget.add_css_class("warning"),
        crate::ContextTier::Work => widget.add_css_class("active"),
        crate::ContextTier::Ambient => {}
    }
}

fn clear_tier_classes(widget: &impl IsA<gtk::Widget>) {
    widget.remove_css_class("active");
    widget.remove_css_class("warning");
    widget.remove_css_class("critical");
}

fn apply_context_motion_classes(widget: &impl IsA<gtk::Widget>, card: &ContextCard) {
    clear_context_motion_classes(widget);
    match card {
        ContextCard::Activity {
            status: crate::ActivityStatus::Running,
            ..
        } => widget.add_css_class("is-running"),
        ContextCard::Media {
            status: crate::PlaybackStatus::Playing,
            ..
        } => widget.add_css_class("is-playing"),
        ContextCard::Timer {
            completed: true, ..
        } => widget.add_css_class("is-complete"),
        _ => {}
    }
    flash_class(widget, "context-arrived", 560);
}

fn clear_context_motion_classes(widget: &impl IsA<gtk::Widget>) {
    for class_name in [
        "suggestion",
        "is-running",
        "is-playing",
        "is-complete",
        "context-arrived",
    ] {
        widget.remove_css_class(class_name);
    }
}

fn apply_motion_style(
    widget: &impl IsA<gtk::Widget>,
    style: MotionStyle,
    ambient_motion: AmbientMotion,
) {
    morph::set_motion_style(style);
    widget.remove_css_class("motion-calm");
    widget.remove_css_class("motion-bold");
    widget.remove_css_class("ambient-off");
    widget.remove_css_class("ambient-meaningful");
    widget.remove_css_class("ambient-always");
    widget.add_css_class(match style {
        MotionStyle::Calm => "motion-calm",
        MotionStyle::Bold => "motion-bold",
    });
    widget.add_css_class(match ambient_motion {
        AmbientMotion::Off => "ambient-off",
        AmbientMotion::Meaningful => "ambient-meaningful",
        AmbientMotion::Always => "ambient-always",
    });
}

fn flash_class(widget: &impl IsA<gtk::Widget>, class_name: &str, duration_ms: u64) {
    if !gtk::Settings::default().is_some_and(|settings| settings.is_gtk_enable_animations()) {
        return;
    }
    let widget = widget.clone().upcast::<gtk::Widget>();
    let class_name = class_name.to_string();
    widget.remove_css_class(&class_name);
    widget.add_css_class(&class_name);
    glib::timeout_add_local_once(std::time::Duration::from_millis(duration_ms), move || {
        widget.remove_css_class(&class_name);
    });
}

fn current_epoch_seconds() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

fn update_state_class(widget: &impl IsA<gtk::Widget>, class_name: &str, present: bool) {
    if present {
        widget.add_css_class(class_name);
    } else {
        widget.remove_css_class(class_name);
    }
}

struct StatusButtonView {
    button: gtk::Button,
    icon: gtk::Image,
    label: gtk::Label,
    classes: Vec<String>,
}

impl StatusButtonView {
    fn update(&mut self, spec: &SystemButtonSpec) {
        let changed = self.icon.icon_name().as_deref() != Some(spec.icon_name.as_str())
            || ((self.label.is_visible() || spec.label.is_some())
                && spec.label.as_deref() != Some(self.label.label().as_str()));
        self.button.set_tooltip_text(Some(&spec.tooltip));
        self.button
            .update_property(&[gtk::accessible::Property::Label(&spec.tooltip)]);
        for class_name in std::mem::take(&mut self.classes) {
            self.button.remove_css_class(&class_name);
        }
        for class_name in &spec.classes {
            self.button.add_css_class(class_name);
        }
        self.classes = spec.classes.clone();
        self.icon.set_icon_name(Some(&spec.icon_name));
        if let Some(text) = spec.label.as_deref() {
            self.label.set_label(text);
            self.label.set_visible(true);
        } else {
            self.label.set_label("");
            self.label.set_visible(false);
        }
        if changed {
            flash_class(&self.button, "state-changed", 480);
        }
    }
}

/// A bar widget's bounds in monitor-logical coordinates.
fn widget_monitor_rect(widget: &impl IsA<gtk::Widget>) -> Option<MorphRect> {
    let root = widget.root()?;
    let bounds = widget.compute_bounds(&root)?;
    Some(stage::bar_to_monitor(MorphRect::new(
        bounds.x(),
        bounds.y(),
        bounds.width(),
        bounds.height(),
    )))
}

fn set_context_accessibility(widget: &gtk::Box, label: &str) {
    widget.update_property(&[gtk::accessible::Property::Label(label)]);
}

fn build_status_button(
    button_spec: &SystemButtonSpec,
    action_sender: &Sender<ActionRequest>,
    control_center: Rc<ControlCenterView>,
    coordinator: Rc<RefCell<PopoverCoordinator>>,
    registry: PopoverRegistry,
    notification_control: Option<NotificationControl>,
) -> StatusButtonView {
    let button = gtk::Button::new();
    button.set_has_frame(false);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let icon = gtk::Image::new();
    let label = gtk::Label::new(None);
    row.append(&icon);
    row.append(&label);
    button.set_child(Some(&row));

    let focus = control_focus(button_spec.id);
    let center_for_click = control_center.clone();
    button.connect_clicked(move |button| {
        if center_for_click.is_visible() && center_for_click.current_page() == focus {
            center_for_click.dismiss();
            return;
        }
        if !center_for_click.is_visible()
            && let Some(window) = button.root().and_downcast::<gtk::Window>()
            && let Some(monitor) = window.monitor()
        {
            center_for_click.set_monitor(&monitor);
        }
        center_for_click.show_page(focus);
        show_managed_window(
            "control-center",
            center_for_click.clone(),
            coordinator.clone(),
            registry.clone(),
            widget_monitor_rect(button),
        );
    });

    if button_spec.id == SystemModuleId::Notifications
        && let Some(control) = notification_control
    {
        let right_click = gtk::GestureClick::new();
        right_click.set_button(3);
        right_click.connect_released(move |gesture, _, _, _| {
            if let Ok(state) = control.snapshot() {
                let _ = control.set_dnd(!state.dnd);
            }
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
        button.add_controller(right_click);
    }

    if button_spec.id == SystemModuleId::Audio {
        install_media_scroll(
            &button,
            action_sender.clone(),
            control_center,
            format!("scroll:{}", button_spec.id.as_str()),
        );
    } else if let Some((previous, next)) = scroll_actions(button_spec.id) {
        install_action_scroll(
            &button,
            action_sender.clone(),
            previous,
            next,
            format!("scroll:{}", button_spec.id.as_str()),
        );
    }

    let mut view = StatusButtonView {
        button,
        icon,
        label,
        classes: Vec::new(),
    };
    view.update(button_spec);
    view
}

fn control_focus(module_id: SystemModuleId) -> ControlCenterFocus {
    match module_id {
        SystemModuleId::Keyboard => ControlCenterFocus::Keyboard,
        SystemModuleId::Resources => ControlCenterFocus::Resources,
        SystemModuleId::AiUsage => ControlCenterFocus::AiUsage,
        SystemModuleId::Network => ControlCenterFocus::Network,
        SystemModuleId::Bluetooth => ControlCenterFocus::Bluetooth,
        SystemModuleId::Audio => ControlCenterFocus::Audio,
        SystemModuleId::Power => ControlCenterFocus::Power,
        SystemModuleId::Notifications => ControlCenterFocus::Notifications,
        SystemModuleId::Clock => ControlCenterFocus::Clock,
    }
}

fn install_action_scroll(
    widget: &impl IsA<gtk::Widget>,
    action_sender: Sender<ActionRequest>,
    previous: ActionIntent,
    next: ActionIntent,
    origin: String,
) {
    install_direction_scroll(widget, action_sender, origin, move |direction| {
        Some(match direction {
            Direction::Previous => previous.clone(),
            Direction::Next => next.clone(),
        })
    });
}

fn install_media_scroll(
    widget: &impl IsA<gtk::Widget>,
    action_sender: Sender<ActionRequest>,
    control_center: Rc<ControlCenterView>,
    origin: String,
) {
    install_direction_scroll(widget, action_sender, origin, move |direction| {
        control_center
            .media_player()
            .map(|player| ActionIntent::ControlMedia {
                player,
                action: match direction {
                    Direction::Previous => MediaControlAction::Previous,
                    Direction::Next => MediaControlAction::Next,
                },
            })
    });
}

fn install_direction_scroll<F>(
    widget: &impl IsA<gtk::Widget>,
    action_sender: Sender<ActionRequest>,
    origin: String,
    intent_for_direction: F,
) where
    F: Fn(Direction) -> Option<ActionIntent> + 'static,
{
    let scroll = gtk::EventControllerScroll::new(
        gtk::EventControllerScrollFlags::VERTICAL
            | gtk::EventControllerScrollFlags::HORIZONTAL
            | gtk::EventControllerScrollFlags::DISCRETE,
    );
    scroll.connect_scroll(move |_, dx, dy| {
        let Some(direction) = scroll_direction(dx, dy) else {
            return glib::Propagation::Proceed;
        };
        let Some(intent) = intent_for_direction(direction) else {
            return glib::Propagation::Proceed;
        };
        let _ = action_sender.send(ActionRequest {
            origin: origin.clone(),
            intent,
        });
        glib::Propagation::Stop
    });
    widget.add_controller(scroll);
}

fn register_popover(
    popover_id: &str,
    popover: &gtk::Popover,
    coordinator: Rc<RefCell<PopoverCoordinator>>,
    registry: PopoverRegistry,
) {
    registry.borrow_mut().insert(
        popover_id.to_string(),
        ManagedOverlay::Popover(popover.clone()),
    );
    let popover_id = popover_id.to_string();
    popover.connect_closed(move |_| {
        coordinator.borrow_mut().close(&popover_id);
    });
}

fn register_control_window(
    overlay_id: &str,
    control_center: Rc<ControlCenterView>,
    coordinator: Rc<RefCell<PopoverCoordinator>>,
    registry: PopoverRegistry,
) {
    registry.borrow_mut().insert(
        overlay_id.to_string(),
        ManagedOverlay::ControlCenter(control_center.clone()),
    );
    let overlay_id = overlay_id.to_string();
    control_center
        .window()
        .connect_visible_notify(move |window| {
            if !window.is_visible() {
                coordinator.borrow_mut().close(&overlay_id);
            }
        });
}

fn show_managed_popover(
    popover_id: &str,
    popover: &gtk::Popover,
    coordinator: Rc<RefCell<PopoverCoordinator>>,
    registry: PopoverRegistry,
) {
    if let Some(previous) = coordinator.borrow_mut().open(popover_id)
        && previous != popover_id
        && let Some(active) = registry.borrow().get(&previous).cloned()
    {
        active.hide();
    }
    if !popover.is_visible() {
        popover.popup();
    }
}

fn show_managed_window(
    overlay_id: &str,
    control_center: Rc<ControlCenterView>,
    coordinator: Rc<RefCell<PopoverCoordinator>>,
    registry: PopoverRegistry,
    origin: Option<MorphRect>,
) {
    if let Some(previous) = coordinator.borrow_mut().open(overlay_id)
        && previous != overlay_id
        && let Some(active) = registry.borrow().get(&previous).cloned()
    {
        active.hide();
    }
    control_center.present_from(origin);
}

fn popdown_active_popover(
    coordinator: &Rc<RefCell<PopoverCoordinator>>,
    registry: &PopoverRegistry,
) {
    if let Some(active) = coordinator.borrow_mut().clear_active()
        && let Some(overlay) = registry.borrow().get(&active).cloned()
    {
        overlay.hide();
    }
}

fn install_escape_dismiss(
    window: &gtk::ApplicationWindow,
    coordinator: Rc<RefCell<PopoverCoordinator>>,
    registry: PopoverRegistry,
) {
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            popdown_active_popover(&coordinator, &registry);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    window.add_controller(keys);
}

fn destroy_popovers(registry: &PopoverRegistry) {
    let overlays = std::mem::take(&mut *registry.borrow_mut());
    for (_, overlay) in overlays {
        overlay.destroy();
    }
}

fn scroll_direction(dx: f64, dy: f64) -> Option<Direction> {
    let delta = if dy.abs() >= dx.abs() { dy } else { dx };
    if delta < 0.0 {
        Some(Direction::Previous)
    } else if delta > 0.0 {
        Some(Direction::Next)
    } else {
        None
    }
}

fn scroll_actions(module_id: SystemModuleId) -> Option<(ActionIntent, ActionIntent)> {
    match module_id {
        SystemModuleId::Keyboard => Some((
            ActionIntent::ToggleKeyboardLayout,
            ActionIntent::ToggleKeyboardLayout,
        )),
        SystemModuleId::Audio => None,
        SystemModuleId::Bluetooth => None,
        SystemModuleId::Power => Some((
            ActionIntent::CyclePowerProfile {
                direction: Direction::Previous,
            },
            ActionIntent::CyclePowerProfile {
                direction: Direction::Next,
            },
        )),
        SystemModuleId::Resources
        | SystemModuleId::AiUsage
        | SystemModuleId::Network
        | SystemModuleId::Notifications
        | SystemModuleId::Clock => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        ActionIntent, ActivityStatus, AppConfig, BarSnapshot, ClockState, CommandActivity,
        Dismissals, OutputRole, OutputState, PowerProfile, PowerState, SuggestionEngine,
        WindowState, WorkspaceState, reload_runtime_config,
    };

    use super::{RenderPlan, SurfaceRegistry, SurfaceSpec, surface_specs};

    #[test]
    fn surface_specs_choose_configured_primary_and_reduce_other_outputs() {
        let snapshot = snapshot([
            output(
                "DP-4",
                &[workspace("1", "1", "DP-4", true, false)],
                Some(window("notes", "Notes")),
                false,
            ),
            output(
                "DP-5",
                &[workspace("2", "2", "DP-5", true, false)],
                Some(window("editor", "Editor")),
                false,
            ),
            output(
                "HDMI-A-2",
                &[workspace("3", "3", "HDMI-A-2", true, false)],
                Some(window("chat", "Chat")),
                false,
            ),
        ]);

        let specs = surface_specs(&snapshot, &AppConfig::default(), &Dismissals::default());

        assert_eq!(
            specs
                .iter()
                .map(|spec| (&spec.output_name, spec.role))
                .collect::<Vec<_>>(),
            vec![
                (&"DP-5".to_string(), OutputRole::Primary),
                (&"DP-4".to_string(), OutputRole::Reduced),
                (&"HDMI-A-2".to_string(), OutputRole::Reduced),
            ]
        );
    }

    #[test]
    fn surface_specs_fall_back_to_focused_output_when_primary_is_missing() {
        let snapshot = snapshot([
            output(
                "DP-4",
                &[workspace("1", "1", "DP-4", true, false)],
                Some(window("notes", "Notes")),
                false,
            ),
            output(
                "HDMI-A-2",
                &[workspace("3", "3", "HDMI-A-2", true, false)],
                Some(window("chat", "Chat")),
                false,
            ),
        ]);

        let specs = surface_specs(&snapshot, &AppConfig::default(), &Dismissals::default());

        assert_eq!(specs[0].output_name, "DP-4");
        assert_eq!(specs[0].role, OutputRole::Primary);
    }

    #[test]
    fn surface_specs_restore_configured_primary_when_it_returns() {
        let without_primary = snapshot([
            output(
                "DP-4",
                &[workspace("1", "1", "DP-4", true, false)],
                Some(window("notes", "Notes")),
                false,
            ),
            output(
                "HDMI-A-2",
                &[workspace("3", "3", "HDMI-A-2", true, false)],
                Some(window("chat", "Chat")),
                false,
            ),
        ]);
        let restored = snapshot([
            output(
                "DP-4",
                &[workspace("1", "1", "DP-4", true, false)],
                Some(window("notes", "Notes")),
                false,
            ),
            output(
                "DP-5",
                &[workspace("2", "2", "DP-5", true, false)],
                Some(window("editor", "Editor")),
                false,
            ),
            output(
                "HDMI-A-2",
                &[workspace("3", "3", "HDMI-A-2", true, false)],
                Some(window("chat", "Chat")),
                false,
            ),
        ]);

        assert_eq!(
            surface_specs(
                &without_primary,
                &AppConfig::default(),
                &Dismissals::default()
            )[0]
            .output_name,
            "DP-4"
        );
        assert_eq!(
            surface_specs(&restored, &AppConfig::default(), &Dismissals::default())[0].output_name,
            "DP-5"
        );
    }

    #[test]
    fn surface_specs_recalculate_primary_role_after_runtime_reload() {
        let snapshot = snapshot([
            output(
                "DP-4",
                &[workspace("1", "1", "DP-4", true, false)],
                Some(window("notes", "Notes")),
                false,
            ),
            output(
                "DP-5",
                &[workspace("2", "2", "DP-5", true, false)],
                Some(window("editor", "Editor")),
                false,
            ),
        ]);
        let current = AppConfig::default();
        let mut next = current.clone();
        next.primary_output = Some("DP-4".to_string());

        let reloaded = reload_runtime_config(&current, next);
        let specs = surface_specs(&snapshot, &reloaded.config, &Dismissals::default());

        assert_eq!(specs[0].output_name, "DP-4");
        assert_eq!(specs[0].role, OutputRole::Primary);
    }

    #[test]
    fn surface_specs_use_output_local_workspaces_and_title() {
        let snapshot = snapshot([
            output(
                "DP-4",
                &[
                    workspace("1", "web", "DP-4", false, false),
                    workspace("2", "term", "DP-4", true, true),
                ],
                Some(window("terminal", "cargo test")),
                false,
            ),
            output(
                "DP-5",
                &[workspace("3", "edit", "DP-5", true, false)],
                Some(window("editor", "nvim")),
                false,
            ),
        ]);

        let specs = surface_specs(&snapshot, &AppConfig::default(), &Dismissals::default());
        let reduced = specs
            .iter()
            .find(|spec| spec.output_name == "DP-4")
            .expect("reduced output spec");

        assert_eq!(
            reduced
                .workspaces
                .iter()
                .map(|workspace| workspace.label.as_str())
                .collect::<Vec<_>>(),
            vec!["web", "term"]
        );
        assert_eq!(reduced.title.text, "cargo test");
        assert_eq!(reduced.title.app, "terminal");
        assert!(reduced.workspaces[1].active);
        assert!(reduced.workspaces[1].urgent);
    }

    #[test]
    fn surface_specs_keep_reduced_outputs_free_of_system_modules() {
        let snapshot = snapshot([
            output(
                "DP-4",
                &[workspace("1", "web", "DP-4", false, false)],
                Some(window("terminal", "cargo test")),
                false,
            ),
            output(
                "DP-5",
                &[workspace("2", "edit", "DP-5", true, false)],
                Some(window("editor", "nvim")),
                false,
            ),
        ]);

        let specs = surface_specs(&snapshot, &AppConfig::default(), &Dismissals::default());
        let primary = specs
            .iter()
            .find(|spec| spec.output_name == "DP-5")
            .expect("primary output spec");
        let reduced = specs
            .iter()
            .find(|spec| spec.output_name == "DP-4")
            .expect("reduced output spec");

        assert!(primary.system.is_some());
        assert_eq!(reduced.system, None);
    }

    #[test]
    fn surface_specs_hide_reduced_warning_when_context_is_not_critical() {
        let snapshot = snapshot([
            output(
                "DP-4",
                &[workspace("1", "1", "DP-4", true, false)],
                Some(window("notes", "Notes")),
                false,
            ),
            output(
                "DP-5",
                &[workspace("2", "2", "DP-5", true, false)],
                Some(window("editor", "Editor")),
                false,
            ),
        ]);

        let specs = surface_specs(&snapshot, &AppConfig::default(), &Dismissals::default());
        let reduced = specs
            .iter()
            .find(|spec| spec.output_name == "DP-4")
            .expect("reduced output spec");

        assert!(reduced.warning.is_none());
    }

    #[test]
    fn surface_specs_propagate_critical_warning_to_reduced_bars() {
        let mut snapshot = snapshot([
            output(
                "DP-4",
                &[workspace("1", "1", "DP-4", true, false)],
                Some(window("notes", "Notes")),
                false,
            ),
            output(
                "DP-5",
                &[workspace("2", "2", "DP-5", true, false)],
                Some(window("editor", "Editor")),
                false,
            ),
        ]);
        snapshot.system.power = PowerState {
            battery_present: true,
            battery_percent: Some(6),
            charging: false,
            profile: PowerProfile::Balanced,
            changed_at: 0,
        };

        let specs = surface_specs(&snapshot, &AppConfig::default(), &Dismissals::default());
        let reduced = specs
            .iter()
            .find(|spec| spec.output_name == "DP-4")
            .expect("reduced output spec");

        assert_eq!(
            reduced.warning.as_ref().map(|warning| warning.tier),
            Some(crate::ContextTier::Critical)
        );
        assert_eq!(
            reduced
                .warning
                .as_ref()
                .map(|warning| warning.text.as_str()),
            Some("Battery 6%")
        );
    }

    #[test]
    fn surface_specs_hides_dismissed_context_card() {
        let mut snapshot = snapshot([output(
            "DP-4",
            &[workspace("1", "1", "DP-4", true, false)],
            Some(window("notes", "Notes")),
            false,
        )]);
        snapshot.system.power = PowerState {
            battery_present: true,
            battery_percent: Some(6),
            charging: false,
            profile: PowerProfile::Balanced,
            changed_at: 0,
        };

        let specs = surface_specs(&snapshot, &AppConfig::default(), &Dismissals::default());
        let primary = specs
            .iter()
            .find(|spec| spec.output_name == "DP-4")
            .expect("primary output spec");
        let context = primary.context.as_ref().expect("battery context card");

        let mut dismissals = Dismissals::default();
        dismissals.dismiss(
            &context.card,
            context.now_epoch,
            context.critical_snooze_seconds,
        );

        let specs = surface_specs(&snapshot, &AppConfig::default(), &dismissals);
        let primary = specs
            .iter()
            .find(|spec| spec.output_name == "DP-4")
            .expect("primary output spec");

        assert!(primary.context.is_none());
    }

    #[test]
    fn related_live_suggestion_enriches_an_imminent_context_card() {
        let mut snapshot = snapshot([output(
            "DP-4",
            &[workspace("1", "1", "DP-4", true, false)],
            Some(window("notes", "Notes")),
            false,
        )]);
        snapshot.system.power = PowerState {
            battery_present: true,
            battery_percent: Some(12),
            charging: false,
            profile: PowerProfile::Balanced,
            changed_at: 7,
        };
        let config = AppConfig::default();
        let mut engine = SuggestionEngine::default();
        let suggestions =
            engine.suggestions(&snapshot, &config, snapshot.system.clock.epoch_seconds);

        let specs = super::surface_specs_with_suggestions(
            &snapshot,
            &config,
            &Dismissals::default(),
            &suggestions,
        );
        let primary = &specs[0];
        assert!(primary.context.is_some());
        assert_eq!(
            primary
                .suggestion
                .as_ref()
                .map(|suggestion| suggestion.id.as_str()),
            Some("live-power-saver")
        );
        assert!(matches!(
            primary
                .suggestion
                .as_ref()
                .map(|suggestion| &suggestion.action),
            Some(crate::SuggestionAction::Intent(
                ActionIntent::SetPowerProfile {
                    profile: PowerProfile::PowerSaver
                }
            ))
        ));
    }

    #[test]
    fn unrelated_suggestion_does_not_displace_running_work() {
        let mut snapshot = snapshot([output(
            "DP-4",
            &[workspace("1", "1", "DP-4", true, false)],
            Some(window("notes", "Notes")),
            false,
        )]);
        snapshot.system.network.wifi_available = true;
        snapshot.system.network.wifi_enabled = Some(false);
        snapshot.activities.items.insert(
            "build".to_string(),
            CommandActivity {
                id: "build".to_string(),
                label: "Cargo test".to_string(),
                cwd: "/tmp/project".into(),
                status: ActivityStatus::Running,
                started_at: snapshot.system.clock.epoch_seconds,
                finished_at: None,
                exit_code: None,
            },
        );
        let config = AppConfig::default();
        let mut engine = SuggestionEngine::default();
        let suggestions =
            engine.suggestions(&snapshot, &config, snapshot.system.clock.epoch_seconds);

        let specs = super::surface_specs_with_suggestions(
            &snapshot,
            &config,
            &Dismissals::default(),
            &suggestions,
        );
        assert!(specs[0].context.is_some());
        assert!(specs[0].suggestion.is_none());
    }

    #[test]
    fn render_plan_skips_workspace_rebuilds_for_clock_only_changes() {
        let previous = SurfaceSpec {
            output_name: "DP-5".to_string(),
            role: OutputRole::Primary,
            workspaces: vec![super::WorkspaceButtonSpec {
                id: "1".to_string(),
                label: "1".to_string(),
                active: true,
                urgent: false,
            }],
            title: super::TitleSpec {
                app: "editor".to_string(),
                text: "Editor".to_string(),
                window_groups: Vec::new(),
            },
            context: None,
            warning: None,
            suggestion: None,
            control_suggestions: Vec::new(),
            system: None,
            clock_label: "12:00".to_string(),
            motion_style: crate::MotionStyle::Bold,
            ambient_motion: crate::AmbientMotion::Meaningful,
        };
        let next = SurfaceSpec {
            clock_label: "12:01".to_string(),
            ..previous.clone()
        };

        let plan = RenderPlan::between(Some(&previous), &next);

        assert!(!plan.workspaces);
        assert!(!plan.title);
        assert!(plan.clock);
    }

    #[test]
    fn bar_window_width_matches_monitor_width_minus_layer_margins() {
        assert_eq!(super::bar_window_width_for_monitor_width(3_840), 3_830);
        assert_eq!(super::bar_window_width_for_monitor_width(1), 1);
    }

    #[test]
    fn toast_changes_are_keyed_by_notification_id() {
        use super::{ToastChanges, toast_changes};

        let make = |id: u32, summary: &str| crate::Notification {
            id,
            summary: summary.to_string(),
            ..crate::Notification::default()
        };
        let previous = vec![make(1, "a"), make(2, "b"), make(3, "c")];
        let next = vec![make(4, "d"), make(2, "b2"), make(1, "a")];
        assert_eq!(
            toast_changes(&previous, &next),
            ToastChanges {
                removed: vec![3],
                inserted: vec![4],
                replaced: vec![2],
            }
        );
        assert_eq!(toast_changes(&next, &next), ToastChanges::default());
    }

    #[test]
    fn toast_expiry_refreshes_are_not_replacements() {
        use super::{ToastChanges, toast_changes};

        let previous = vec![crate::Notification {
            id: 7,
            summary: "Build finished".to_string(),
            received_at: 100,
            expires_at: Some(105),
            ..crate::Notification::default()
        }];
        let next = vec![crate::Notification {
            received_at: 101,
            expires_at: Some(112),
            ..previous[0].clone()
        }];
        assert_eq!(toast_changes(&previous, &next), ToastChanges::default());
    }

    fn timer_context(id: &str, remaining_seconds: u64, now_epoch: i64) -> super::ContextSpec {
        super::ContextSpec {
            card: crate::ContextCard::Timer {
                id: id.to_string(),
                label: "Tea".to_string(),
                remaining_seconds,
                target_epoch: Some(now_epoch + i64::try_from(remaining_seconds).unwrap()),
                completed: false,
                tier: crate::ContextTier::Work,
            },
            icon_name: "alarm-symbolic".to_string(),
            text: format!("Tea {remaining_seconds}s"),
            tier: crate::ContextTier::Work,
            now_epoch,
            critical_snooze_seconds: 300,
        }
    }

    #[test]
    fn timer_ticks_do_not_crossfade_the_island() {
        use super::context_identity;

        let previous = timer_context("tea", 60, 1_000);
        let next = timer_context("tea", 59, 1_001);
        assert_eq!(
            context_identity(Some(&previous), None),
            context_identity(Some(&next), None)
        );
    }

    #[test]
    fn showing_or_switching_context_crossfades_the_island() {
        use super::context_identity;

        let timer = timer_context("tea", 60, 1_000);
        assert_ne!(
            context_identity(None, None),
            context_identity(Some(&timer), None)
        );
        let battery = super::ContextSpec {
            card: crate::ContextCard::Battery {
                percent: 9,
                charging: false,
                tier: crate::ContextTier::Critical,
            },
            ..timer.clone()
        };
        assert_ne!(
            context_identity(Some(&timer), None),
            context_identity(Some(&battery), None)
        );
        let other_timer = timer_context("pasta", 60, 1_000);
        assert_ne!(
            context_identity(Some(&timer), None),
            context_identity(Some(&other_timer), None)
        );
    }

    #[test]
    fn notification_stack_width_is_compact_and_monitor_bounded() {
        assert_eq!(super::notification_stack_width(3_840), 400);
        assert_eq!(super::notification_stack_width(1_920), 400);
        assert_eq!(super::notification_stack_width(360), 336);
        assert_eq!(super::notification_stack_width(200), 176);
        assert_eq!(super::notification_stack_width(20), 1);
    }

    #[test]
    fn presentation_visibility_is_stateful_and_requests_reconcile() {
        let mut registry = SurfaceRegistry::new();
        assert!(!registry.presentation_hidden());

        registry.set_presentation_hidden(true);
        assert!(registry.presentation_hidden());
        assert!(registry.take_context_dirty());

        registry.set_presentation_hidden(false);
        assert!(!registry.presentation_hidden());
        assert!(registry.take_context_dirty());
    }

    fn snapshot<const N: usize>(outputs: [OutputState; N]) -> BarSnapshot {
        let outputs = outputs
            .into_iter()
            .map(|output| (output.name.clone(), output))
            .collect::<BTreeMap<_, _>>();

        BarSnapshot {
            outputs,
            focused_output: Some("DP-4".to_string()),
            system: crate::SystemState {
                clock: ClockState {
                    epoch_seconds: 1_800_000_000,
                    label: "12:00".to_string(),
                },
                ..crate::SystemState::default()
            },
            ..BarSnapshot::default()
        }
    }

    fn output(
        name: &str,
        workspaces: &[WorkspaceState],
        focused_window: Option<WindowState>,
        urgent: bool,
    ) -> OutputState {
        OutputState {
            name: name.to_string(),
            workspaces: workspaces.to_vec(),
            windows: focused_window.iter().cloned().collect(),
            focused_window,
            urgent,
            changed_at: 0,
        }
    }

    fn workspace(
        id: &str,
        label: &str,
        output: &str,
        active: bool,
        urgent: bool,
    ) -> WorkspaceState {
        WorkspaceState {
            id: id.to_string(),
            label: label.to_string(),
            output: output.to_string(),
            active,
            urgent,
            changed_at: 0,
        }
    }

    fn window(id: &str, title: &str) -> WindowState {
        WindowState {
            id: id.to_string(),
            app_id: Some(id.to_string()),
            title: title.to_string(),
            urgent: false,
            workspace_id: None,
            changed_at: 0,
        }
    }
}
