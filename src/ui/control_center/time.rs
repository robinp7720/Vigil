use super::*;

pub(super) struct TimerRowWidgets {
    pub(super) root: gtk::Box,
    pub(super) title: gtk::Label,
    pub(super) remaining: gtk::Label,
    pub(super) primary: gtk::Button,
    pub(super) cancel: gtk::Button,
    pub(super) current: Rc<RefCell<TimerControlSpec>>,
}

pub(super) type TimerWidgets = BTreeMap<String, TimerRowWidgets>;

#[derive(Clone)]
pub(super) struct TimePage {
    pub(super) root: gtk::Box,
    pub(super) clock: gtk::Label,
    pub(super) date: gtk::Label,
    pub(super) view_stack: gtk::Stack,
    pub(super) calendar_tab: gtk::ToggleButton,
    pub(super) calendar: gtk::Calendar,
    pub(super) agenda_title: gtk::Label,
    pub(super) agenda_list: gtk::Box,
    pub(super) agenda_empty: gtk::Label,
    pub(super) agenda_status: gtk::Label,
    pub(super) requested_month: Rc<Cell<Option<(i32, u32)>>>,
    pub(super) rendered_agenda: Rc<RefCell<Option<AgendaRenderKey>>>,
    pub(super) expanded_event: Rc<RefCell<Option<String>>>,
    pub(super) agenda_details: Rc<RefCell<BTreeMap<String, gtk::Box>>>,
    pub(super) latest_spec: Rc<RefCell<ControlCenterSpec>>,
    pub(super) calendar_action: ActionHandle,
    pub(super) calendar_sender: Option<Sender<CalendarMonthRequest>>,
    pub(super) timer_list: gtk::Box,
    pub(super) timer_empty: gtk::Label,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AgendaRenderKey {
    pub(super) year: i32,
    pub(super) month: u32,
    pub(super) day: i32,
    pub(super) loaded: bool,
    pub(super) events: Vec<CalendarAgendaEvent>,
    pub(super) error: Option<String>,
}

pub(super) fn build_time_page(
    spec: &ControlCenterSpec,
    action_sender: Sender<ActionRequest>,
    errors: Rc<RefCell<ControlCenterErrors>>,
    navigation: NavigationUi,
    calendar_sender: Option<Sender<CalendarMonthRequest>>,
) -> TimePage {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.add_css_class("control-page");
    root.add_css_class("time-page");

    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    hero.add_css_class("time-hero");
    let icon = gtk::Image::from_icon_name("preferences-system-time-symbolic");
    icon.add_css_class("time-hero-icon");
    icon.set_pixel_size(26);
    hero.append(&icon);
    let hero_text = gtk::Box::new(gtk::Orientation::Vertical, 1);
    hero_text.set_hexpand(true);
    let clock = gtk::Label::new(Some(&spec.clock));
    clock.add_css_class("time-hero-clock");
    clock.set_xalign(0.0);
    let date = gtk::Label::new(None);
    date.add_css_class("time-hero-date");
    date.set_xalign(0.0);
    hero_text.append(&clock);
    hero_text.append(&date);
    hero.append(&hero_text);
    let today = gtk::Button::with_label("Today");
    today.add_css_class("compact-action");
    hero.append(&today);
    root.append(&hero);

    let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tabs.add_css_class("time-tabs");
    tabs.set_homogeneous(true);
    let calendar_tab = gtk::ToggleButton::with_label("Calendar");
    let timers_tab = gtk::ToggleButton::with_label("Timers");
    calendar_tab.add_css_class("time-tab");
    timers_tab.add_css_class("time-tab");
    timers_tab.set_group(Some(&calendar_tab));
    calendar_tab.set_active(true);
    tabs.append(&calendar_tab);
    tabs.append(&timers_tab);
    root.append(&tabs);

    let view_stack = gtk::Stack::new();
    view_stack.add_css_class("time-view-stack");
    view_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    view_stack.set_transition_duration(160);
    view_stack.set_hhomogeneous(true);
    view_stack.set_vhomogeneous(false);

    let calendar_view = gtk::Box::new(gtk::Orientation::Vertical, 8);
    calendar_view.add_css_class("time-calendar-view");
    let calendar = gtk::Calendar::new();
    calendar.add_css_class("time-calendar");
    calendar.set_show_day_names(true);
    calendar.set_show_heading(true);
    calendar.set_show_week_numbers(false);
    calendar_view.append(&calendar);

    let agenda_heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let agenda_title = gtk::Label::new(Some("Selected day"));
    agenda_title.add_css_class("section-title");
    agenda_title.set_xalign(0.0);
    agenda_title.set_hexpand(true);
    let open_calendar = gtk::Button::with_label("Open calendar");
    open_calendar.add_css_class("compact-action");
    agenda_heading.append(&agenda_title);
    agenda_heading.append(&open_calendar);
    calendar_view.append(&agenda_heading);

    let agenda_list = gtk::Box::new(gtk::Orientation::Vertical, 6);
    agenda_list.add_css_class("agenda-list");
    let agenda_empty = gtk::Label::new(Some("No events on this day"));
    agenda_empty.add_css_class("empty-state");
    agenda_empty.set_xalign(0.0);
    let agenda_status = gtk::Label::new(Some("Loading calendar…"));
    agenda_status.add_css_class("calendar-status");
    agenda_status.set_xalign(0.0);
    let agenda_scroll = gtk::ScrolledWindow::new();
    agenda_scroll.add_css_class("agenda-scroll");
    agenda_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    agenda_scroll.set_min_content_height(150);
    agenda_scroll.set_max_content_height(190);
    agenda_scroll.set_propagate_natural_height(true);
    let agenda_content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    agenda_content.append(&agenda_status);
    agenda_content.append(&agenda_empty);
    agenda_content.append(&agenda_list);
    agenda_scroll.set_child(Some(&agenda_content));
    calendar_view.append(&agenda_scroll);
    view_stack.add_named(&calendar_view, Some("calendar"));

    let timers_view = gtk::Box::new(gtk::Orientation::Vertical, 9);
    timers_view.add_css_class("time-timers-view");
    timers_view.append(&section_eyebrow("QUICK START"));
    let presets = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    presets.add_css_class("timer-presets");
    presets.set_homogeneous(true);
    let timer_action = ActionHandle {
        sender: action_sender.clone(),
        errors: errors.clone(),
        navigation: navigation.clone(),
        focus: ControlCenterFocus::Clock,
    };
    for minutes in [5_u64, 15, 25, 45] {
        let button = gtk::Button::with_label(&format!("{minutes} min"));
        button.add_css_class("timer-preset");
        let handle = timer_action.clone();
        button.connect_clicked(move |_| {
            handle.send(
                "start-timer",
                ActionIntent::StartTimer {
                    label: format!("{minutes} minute timer"),
                    duration_seconds: minutes * 60,
                },
            );
        });
        presets.append(&button);
    }
    timers_view.append(&presets);
    timers_view.append(&section_eyebrow("CUSTOM TIMER"));
    let composer = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    composer.add_css_class("timer-composer");
    let timer_label = gtk::Entry::new();
    timer_label.set_placeholder_text(Some("Timer label"));
    timer_label.set_hexpand(true);
    let duration = gtk::SpinButton::with_range(1.0, 720.0, 1.0);
    duration.set_value(25.0);
    duration.set_tooltip_text(Some("Duration in minutes"));
    duration.set_width_chars(4);
    let minutes_label = gtk::Label::new(Some("min"));
    minutes_label.add_css_class("supporting-text");
    let start = gtk::Button::with_label("Start");
    start.add_css_class("primary-action");
    composer.append(&timer_label);
    composer.append(&duration);
    composer.append(&minutes_label);
    composer.append(&start);
    timers_view.append(&composer);

    let active_heading = gtk::Label::new(Some("Active timers"));
    active_heading.add_css_class("section-title");
    active_heading.set_xalign(0.0);
    timers_view.append(&active_heading);
    let timer_list = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let timer_empty = gtk::Label::new(Some("No active timers"));
    timer_empty.add_css_class("empty-state");
    timer_empty.set_xalign(0.0);
    timer_list.append(&timer_empty);
    let timer_scroll = gtk::ScrolledWindow::new();
    timer_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    timer_scroll.set_min_content_height(170);
    timer_scroll.set_max_content_height(250);
    timer_scroll.set_propagate_natural_height(true);
    timer_scroll.set_child(Some(&timer_list));
    timers_view.append(&timer_scroll);
    view_stack.add_named(&timers_view, Some("timers"));
    view_stack.set_visible_child_name("calendar");
    root.append(&view_stack);

    let calendar_action = ActionHandle {
        sender: action_sender,
        errors,
        navigation,
        focus: ControlCenterFocus::Clock,
    };
    let open_handle = calendar_action.clone();
    open_calendar.connect_clicked(move |_| {
        open_handle.send("open-calendar", ActionIntent::OpenCalendar);
    });

    let stack_for_calendar = view_stack.clone();
    calendar_tab.connect_toggled(move |button| {
        if button.is_active() {
            stack_for_calendar.set_visible_child_name("calendar");
        }
    });
    let stack_for_timers = view_stack.clone();
    timers_tab.connect_toggled(move |button| {
        if button.is_active() {
            stack_for_timers.set_visible_child_name("timers");
        }
    });

    let custom_handle = timer_action;
    let label_for_start = timer_label.clone();
    let duration_for_start = duration.clone();
    start.connect_clicked(move |_| {
        let minutes = u64::try_from(duration_for_start.value_as_int())
            .unwrap_or(1)
            .clamp(1, 720);
        let raw_label = label_for_start.text();
        let label = if raw_label.trim().is_empty() {
            "Timer".to_string()
        } else {
            raw_label.trim().to_string()
        };
        custom_handle.send(
            "start-timer",
            ActionIntent::StartTimer {
                label,
                duration_seconds: minutes * 60,
            },
        );
        label_for_start.set_text("");
    });

    let page = TimePage {
        root,
        clock,
        date,
        view_stack,
        calendar_tab,
        calendar,
        agenda_title,
        agenda_list,
        agenda_empty,
        agenda_status,
        requested_month: Rc::new(Cell::new(None)),
        rendered_agenda: Rc::new(RefCell::new(None)),
        expanded_event: Rc::new(RefCell::new(None)),
        agenda_details: Rc::new(RefCell::new(BTreeMap::new())),
        latest_spec: Rc::new(RefCell::new(spec.clone())),
        calendar_action,
        calendar_sender,
        timer_list,
        timer_empty,
    };

    connect_time_calendar(&page);
    let page_for_today = page.clone();
    today.connect_clicked(move |_| reset_time_calendar(&page_for_today));
    begin_calendar_load(&page);
    page
}

pub(super) fn connect_time_calendar(page: &TimePage) {
    let page_for_day = page.clone();
    page.calendar.connect_day_selected(move |calendar| {
        // Clicking a greyed-out day from a neighbouring month switches month.
        if page_for_day.requested_month.get() != Some(displayed_month(calendar)) {
            begin_calendar_load(&page_for_day);
        }
        render_agenda(&page_for_day, &page_for_day.latest_spec.borrow());
    });

    macro_rules! connect_load {
        ($signal:ident) => {{
            let page = page.clone();
            page.calendar
                .clone()
                .$signal(move |_| begin_calendar_load(&page));
        }};
    }
    connect_load!(connect_next_month);
    connect_load!(connect_prev_month);
    connect_load!(connect_next_year);
    connect_load!(connect_prev_year);
}

pub(super) fn begin_calendar_load(page: &TimePage) {
    let (year, month) = displayed_month(&page.calendar);
    page.requested_month.set(Some((year, month)));
    set_day_event_levels(&page.calendar, &BTreeMap::new());
    page.rendered_agenda.borrow_mut().take();
    if let Some(sender) = &page.calendar_sender {
        let _ = sender.send(CalendarMonthRequest { year, month });
    }
    // Render straight away: the store may already hold this month, and a
    // remembered month arrives without another snapshot change.
    let spec = page.latest_spec.borrow().clone();
    update_calendar_marks(&page.calendar, spec.calendar_agenda.as_ref());
    render_agenda(page, &spec);
}

pub(super) fn reset_time_page(page: &TimePage) {
    page.calendar_tab.set_active(true);
    page.view_stack.set_visible_child_name("calendar");
    reset_time_calendar(page);
}

pub(super) fn reset_time_calendar(page: &TimePage) {
    if let Ok(today) = glib::DateTime::now_local() {
        page.calendar.set_date(&today);
    }
    begin_calendar_load(page);
}

pub(super) fn update_time_page(page: &TimePage, spec: &ControlCenterSpec) {
    *page.latest_spec.borrow_mut() = spec.clone();
    page.clock.set_label(&spec.clock);
    page.date.set_label(&local_date_label(spec.clock_epoch));
    update_calendar_marks(&page.calendar, spec.calendar_agenda.as_ref());
    render_agenda(page, spec);
}

pub(super) fn local_date_label(epoch: i64) -> String {
    glib::DateTime::from_unix_local(epoch)
        .and_then(|date| date.format("%A, %-e %B"))
        .map(|label| label.to_string())
        .unwrap_or_else(|_| "Local time".to_string())
}

/// The displayed year and month as 1–12; `gtk::Calendar::month()` counts from zero.
pub(super) fn displayed_month(calendar: &gtk::Calendar) -> (i32, u32) {
    (calendar.year(), calendar_month_from_gtk(calendar.month()))
}

pub(super) fn calendar_month_from_gtk(month: i32) -> u32 {
    u32::try_from(month.clamp(0, 11) + 1).unwrap_or(1)
}

pub(super) fn update_calendar_marks(calendar: &gtk::Calendar, agenda: Option<&CalendarAgenda>) {
    let (year, month) = displayed_month(calendar);
    let levels = agenda
        .filter(|agenda| agenda.year == year && agenda.month == month)
        .map(|agenda| day_event_levels(&agenda.events, year, month))
        .unwrap_or_default();
    set_day_event_levels(calendar, &levels);
}

const DAY_EVENT_LEVEL_CLASSES: [&str; 3] = ["events-1", "events-2", "events-3"];

/// How busy each day of the month is: 1 for all-day or a single event,
/// 2 for two timed events, 3 for three or more.
pub(super) fn day_event_levels(
    events: &[CalendarAgendaEvent],
    year: i32,
    month: u32,
) -> BTreeMap<u32, u8> {
    (1..=31)
        .filter_map(|day| {
            let events = events_for_local_day(events, year, month, day);
            let timed = events.iter().filter(|event| !event.all_day).count();
            let level = match timed {
                0 if events.is_empty() => return None,
                0 | 1 => 1,
                2 => 2,
                _ => 3,
            };
            Some((u32::try_from(day).unwrap_or_default(), level))
        })
        .collect()
}

/// GtkCalendar only offers a single "marked" state, so tag its day labels
/// with a density class instead.
pub(super) fn set_day_event_levels(calendar: &gtk::Calendar, levels: &BTreeMap<u32, u8>) {
    let mut stack = vec![calendar.clone().upcast::<gtk::Widget>()];
    while let Some(widget) = stack.pop() {
        let mut child = widget.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            stack.push(current.clone());
            let Ok(label) = current.downcast::<gtk::Label>() else {
                continue;
            };
            if !label.has_css_class("day-number") {
                continue;
            }
            for class in DAY_EVENT_LEVEL_CLASSES {
                label.remove_css_class(class);
            }
            if label.has_css_class("other-month") {
                continue;
            }
            let level = label
                .label()
                .parse::<u32>()
                .ok()
                .and_then(|day| levels.get(&day));
            if let Some(&level) = level {
                label.add_css_class(DAY_EVENT_LEVEL_CLASSES[usize::from(level.clamp(1, 3) - 1)]);
            }
        }
    }
}

pub(super) fn selected_day_heading(selected: (i32, u32, i32), today: (i32, u32, i32)) -> String {
    let local_day = |(year, month, day): (i32, u32, i32)| {
        glib::DateTime::from_local(year, i32::try_from(month).unwrap_or(1), day, 0, 0, 0.0)
    };
    let (Ok(selected_date), Ok(today_date)) = (local_day(selected), local_day(today)) else {
        return "Selected day".to_string();
    };
    let format = if selected.0 == today.0 {
        "%A, %-e %B"
    } else {
        "%A, %-e %B %Y"
    };
    let label = selected_date
        .format(format)
        .map(|label| label.to_string())
        .unwrap_or_else(|_| "Selected day".to_string());
    // Round so a daylight-saving change does not shift the offset.
    let days = (selected_date.difference(&today_date).as_seconds() as f64 / 86_400.0).round();
    match days as i64 {
        0 => format!("Today · {label}"),
        1 => format!("Tomorrow · {label}"),
        -1 => format!("Yesterday · {label}"),
        _ => label,
    }
}

pub(super) fn render_agenda(page: &TimePage, spec: &ControlCenterSpec) {
    let (year, month) = displayed_month(&page.calendar);
    let day = page.calendar.day();
    let today = glib::DateTime::from_unix_local(spec.clock_epoch)
        .ok()
        .filter(|_| spec.clock_epoch > 0)
        .or_else(|| glib::DateTime::now_local().ok());
    if let Some(today) = today {
        let today = (
            today.year(),
            u32::try_from(today.month()).unwrap_or(1),
            today.day_of_month(),
        );
        page.agenda_title
            .set_label(&selected_day_heading((year, month, day), today));
    }

    let matching = spec
        .calendar_agenda
        .as_ref()
        .filter(|agenda| agenda.year == year && agenda.month == month);
    let events = matching
        .map(|agenda| events_for_local_day(&agenda.events, year, month, day))
        .unwrap_or_default();
    let key = AgendaRenderKey {
        year,
        month,
        day,
        loaded: matching.is_some(),
        events: events.clone(),
        error: spec.calendar_agenda_error.clone(),
    };
    if page.rendered_agenda.borrow().as_ref() == Some(&key) {
        return;
    }
    *page.rendered_agenda.borrow_mut() = Some(key);
    clear_box(&page.agenda_list);
    page.agenda_details.borrow_mut().clear();
    page.expanded_event.borrow_mut().take();

    if matching.is_none() {
        page.agenda_status
            .set_label(if page.calendar_sender.is_none() {
                "Calendar integration unavailable"
            } else {
                spec.calendar_agenda_error
                    .as_deref()
                    .unwrap_or("Loading calendar…")
            });
        page.agenda_status.set_visible(true);
        page.agenda_empty.set_visible(false);
        return;
    }
    page.agenda_status.set_visible(false);
    page.agenda_empty.set_visible(events.is_empty());
    for event in events {
        page.agenda_list.append(&agenda_event_row(
            &event,
            &page.expanded_event,
            &page.agenda_details,
            &page.calendar_action,
        ));
    }
}

pub(super) fn events_for_local_day(
    events: &[CalendarAgendaEvent],
    year: i32,
    month: u32,
    day: i32,
) -> Vec<CalendarAgendaEvent> {
    let Ok(start) =
        glib::DateTime::from_local(year, i32::try_from(month).unwrap_or(1), day, 0, 0, 0.0)
    else {
        return Vec::new();
    };
    let Ok(end) = start.add_days(1) else {
        return Vec::new();
    };
    let start_epoch = start.to_unix();
    let end_epoch = end.to_unix();
    let mut matching = events
        .iter()
        .filter(|event| {
            let event_end = if event.end_epoch == event.start_epoch {
                event.start_epoch.saturating_add(1)
            } else {
                event.end_epoch
            };
            event.start_epoch < end_epoch && event_end > start_epoch
        })
        .cloned()
        .collect::<Vec<_>>();
    matching.sort_by(|left, right| {
        (!left.all_day, left.start_epoch, &left.title).cmp(&(
            !right.all_day,
            right.start_epoch,
            &right.title,
        ))
    });
    matching
}

pub(super) fn agenda_event_row(
    event: &CalendarAgendaEvent,
    expanded: &Rc<RefCell<Option<String>>>,
    details: &Rc<RefCell<BTreeMap<String, gtk::Box>>>,
    action: &ActionHandle,
) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("agenda-event");
    let header = gtk::Button::new();
    header.add_css_class("agenda-event-header");
    header.set_has_frame(false);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let time = gtk::Label::new(Some(&event_time_label(event)));
    time.add_css_class("agenda-event-time");
    time.set_width_chars(10);
    time.set_xalign(0.0);
    let title = gtk::Label::new(Some(&event.title));
    title.add_css_class("agenda-event-title");
    title.set_hexpand(true);
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let arrow = gtk::Image::from_icon_name("go-down-symbolic");
    arrow.add_css_class("agenda-event-arrow");
    row.append(&time);
    row.append(&title);
    row.append(&arrow);
    header.set_child(Some(&row));
    root.append(&header);

    let detail = gtk::Box::new(gtk::Orientation::Vertical, 5);
    detail.add_css_class("agenda-event-detail");
    detail.set_visible(false);
    let full_time = gtk::Label::new(Some(&event_full_time_label(event)));
    full_time.set_xalign(0.0);
    detail.append(&full_time);
    if let Some(location) = event.location.as_deref() {
        let location = gtk::Label::new(Some(&format!("Location · {location}")));
        location.add_css_class("supporting-text");
        location.set_xalign(0.0);
        location.set_wrap(true);
        detail.append(&location);
    }
    if let Some(calendar) = event.calendar.as_deref() {
        let calendar = gtk::Label::new(Some(&format!("Calendar · {calendar}")));
        calendar.add_css_class("supporting-text");
        calendar.set_xalign(0.0);
        detail.append(&calendar);
    }
    let open = gtk::Button::with_label("Open in Evolution");
    open.add_css_class("compact-action");
    open.set_halign(gtk::Align::Start);
    let open_handle = action.clone();
    open.connect_clicked(move |_| open_handle.send("open-calendar", ActionIntent::OpenCalendar));
    detail.append(&open);
    root.append(&detail);
    details
        .borrow_mut()
        .insert(event.id.clone(), detail.clone());

    let event_id = event.id.clone();
    let expanded = expanded.clone();
    let details = details.clone();
    header.connect_clicked(move |_| {
        let next =
            (expanded.borrow().as_deref() != Some(event_id.as_str())).then_some(event_id.clone());
        for (id, detail) in details.borrow().iter() {
            detail.set_visible(next.as_deref() == Some(id.as_str()));
        }
        *expanded.borrow_mut() = next;
    });
    root
}

pub(super) fn event_time_label(event: &CalendarAgendaEvent) -> String {
    if event.all_day {
        return "All day".to_string();
    }
    glib::DateTime::from_unix_local(event.start_epoch)
        .and_then(|date| date.format("%H:%M"))
        .map(|label| label.to_string())
        .unwrap_or_else(|_| "Timed".to_string())
}

pub(super) fn event_full_time_label(event: &CalendarAgendaEvent) -> String {
    let Ok(start) = glib::DateTime::from_unix_local(event.start_epoch) else {
        return event_time_label(event);
    };
    if event.all_day {
        return start
            .format("%A, %e %B · All day")
            .map(|label| label.to_string())
            .unwrap_or_else(|_| "All day".to_string());
    }
    let start_label = start
        .format("%A, %e %B · %H:%M")
        .map(|label| label.to_string())
        .unwrap_or_else(|_| event_time_label(event));
    let end_label = glib::DateTime::from_unix_local(event.end_epoch)
        .and_then(|date| date.format("%H:%M"))
        .map(|label| label.to_string())
        .unwrap_or_default();
    if end_label.is_empty() {
        start_label
    } else {
        format!("{start_label}–{end_label}")
    }
}

pub(super) fn reconcile_timers(
    list: &gtk::Box,
    empty: &gtk::Label,
    widgets: &mut TimerWidgets,
    timers: &[TimerControlSpec],
    sender: Sender<ActionRequest>,
    errors: Rc<RefCell<ControlCenterErrors>>,
    navigation: NavigationUi,
) {
    let stale = widgets
        .keys()
        .filter(|id| !timers.iter().any(|timer| timer.id == **id))
        .cloned()
        .collect::<Vec<_>>();
    for id in stale {
        if let Some(row) = widgets.remove(&id) {
            list.remove(&row.root);
        }
    }

    for timer in timers {
        if !widgets.contains_key(&timer.id) {
            let row = timer_row(timer, sender.clone(), errors.clone(), navigation.clone());
            list.append(&row.root);
            widgets.insert(timer.id.clone(), row);
        }
        update_timer_row(widgets.get(&timer.id).expect("timer row"), timer);
    }
    empty.set_visible(timers.is_empty());
}

pub(super) fn timer_row(
    timer: &TimerControlSpec,
    sender: Sender<ActionRequest>,
    errors: Rc<RefCell<ControlCenterErrors>>,
    navigation: NavigationUi,
) -> TimerRowWidgets {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    root.add_css_class("timer-row");
    let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
    text.set_hexpand(true);
    let title = gtk::Label::new(None);
    title.set_xalign(0.0);
    title.set_max_width_chars(32);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let remaining = gtk::Label::new(None);
    remaining.add_css_class("supporting-text");
    remaining.set_xalign(0.0);
    text.append(&title);
    text.append(&remaining);
    root.append(&text);

    let current = Rc::new(RefCell::new(timer.clone()));
    let primary = icon_button("media-playback-pause-symbolic", "Pause timer");
    let primary_handle = ActionHandle {
        sender: sender.clone(),
        errors: errors.clone(),
        navigation: navigation.clone(),
        focus: ControlCenterFocus::Clock,
    };
    let current_for_primary = current.clone();
    primary.connect_clicked(move |_| {
        let timer = current_for_primary.borrow();
        let id = timer.id.clone();
        let (action, intent) = match timer_action_state(&timer) {
            TimerActionState::Clear => ("cancel-timer", ActionIntent::CancelTimer { id }),
            TimerActionState::Resume => ("resume-timer", ActionIntent::ResumeTimer { id }),
            TimerActionState::Pause => ("pause-timer", ActionIntent::PauseTimer { id }),
        };
        drop(timer);
        primary_handle.send(action, intent);
    });
    root.append(&primary);

    let cancel = icon_button("window-close-symbolic", "Cancel timer");
    let cancel_handle = ActionHandle {
        sender,
        errors,
        navigation,
        focus: ControlCenterFocus::Clock,
    };
    let current_for_cancel = current.clone();
    cancel.connect_clicked(move |_| {
        let id = current_for_cancel.borrow().id.clone();
        cancel_handle.send("cancel-timer", ActionIntent::CancelTimer { id });
    });
    root.append(&cancel);

    TimerRowWidgets {
        root,
        title,
        remaining,
        primary,
        cancel,
        current,
    }
}

pub(super) fn update_timer_row(row: &TimerRowWidgets, timer: &TimerControlSpec) {
    *row.current.borrow_mut() = timer.clone();
    row.title.set_label(&timer.label);
    row.remaining.set_label(&timer_summary(timer));
    let (icon, tooltip) = match timer_action_state(timer) {
        TimerActionState::Clear => ("edit-delete-symbolic", "Clear timer"),
        TimerActionState::Resume => ("media-playback-start-symbolic", "Resume timer"),
        TimerActionState::Pause => ("media-playback-pause-symbolic", "Pause timer"),
    };
    row.primary.set_icon_name(icon);
    row.primary.set_tooltip_text(Some(tooltip));
    row.cancel.set_visible(!timer.completed);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TimerActionState {
    Clear,
    Resume,
    Pause,
}

pub(super) fn timer_action_state(timer: &TimerControlSpec) -> TimerActionState {
    if timer.completed {
        TimerActionState::Clear
    } else if timer.paused {
        TimerActionState::Resume
    } else {
        TimerActionState::Pause
    }
}

pub(super) fn timer_summary(timer: &TimerControlSpec) -> String {
    if timer.completed {
        "Complete".to_string()
    } else {
        let minutes = timer.remaining_seconds / 60;
        let seconds = timer.remaining_seconds % 60;
        let suffix = if timer.paused { " · paused" } else { "" };
        format!("{minutes}:{seconds:02}{suffix}")
    }
}
