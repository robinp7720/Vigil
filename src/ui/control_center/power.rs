use super::*;

pub(super) fn power_profile_label(profile: &PowerProfile) -> &'static str {
    match profile {
        PowerProfile::Performance => "Performance",
        PowerProfile::Balanced => "Balanced",
        PowerProfile::PowerSaver => "Power saver",
    }
}

pub(super) fn power_profile_icon(profile: &PowerProfile) -> &'static str {
    match profile {
        PowerProfile::Performance => "power-profile-performance-symbolic",
        PowerProfile::Balanced => "power-profile-balanced-symbolic",
        PowerProfile::PowerSaver => "power-profile-power-saver-symbolic",
    }
}

const POWER_PROFILE_ORDER: [PowerProfile; 3] = [
    PowerProfile::PowerSaver,
    PowerProfile::Balanced,
    PowerProfile::Performance,
];

pub(super) fn power_profile_description(profile: &PowerProfile) -> &'static str {
    match profile {
        PowerProfile::Performance => "Holds clocks high for sustained heavy work",
        PowerProfile::Balanced => "Full speed on demand, frugal when idle",
        PowerProfile::PowerSaver => "Cooler and quieter, trades speed for efficiency",
    }
}

pub(super) fn performance_degraded_label(reason: &str) -> String {
    match reason {
        "lap-detected" => "Performance limited · device is on a lap".to_string(),
        "high-operating-temperature" => "Performance limited · running hot".to_string(),
        other => format!("Performance limited · {}", other.replace('-', " ")),
    }
}

pub(super) fn power_page_spec(power: &crate::PowerState) -> PowerPageSpec {
    let stats = &power.stats;
    let (hero_icon, hero_eyebrow, hero_title, hero_detail) = if power.battery_present {
        (
            battery_level_icon(power.battery_percent, power.charging),
            "Battery",
            power
                .battery_percent
                .map(|percent| format!("{percent}%"))
                .unwrap_or_else(|| "Reading battery".to_string()),
            battery_hero_detail(power),
        )
    } else {
        (
            power_profile_icon(&power.profile),
            "Power profile",
            power_profile_label(&power.profile).to_string(),
            "Running on mains power".to_string(),
        )
    };

    let profiles = POWER_PROFILE_ORDER
        .iter()
        .map(|profile| PowerProfileOptionSpec {
            profile: profile.clone(),
            // An empty list means the daemon could not be asked; offer all.
            available: power.available_profiles.is_empty()
                || power.available_profiles.contains(profile),
            selected: power.profile == *profile,
        })
        .collect();
    let (profile_note, performance_degraded) = match power.performance_degraded.as_deref() {
        Some(reason) => (performance_degraded_label(reason), true),
        None => (power_profile_description(&power.profile).to_string(), false),
    };

    let mut stat_specs = Vec::new();
    if power.battery_present {
        if let Some(rate) = stats.energy_rate_mw {
            stat_specs.push(PowerStatSpec {
                id: "rate",
                label: if power.charging {
                    "Charging at"
                } else {
                    "Draw"
                },
                value: format_watts(rate),
                caption: stats
                    .energy_now_mwh
                    .map(|now| format!("{} stored", format_watt_hours(now))),
                level: None,
            });
        }
        if let Some((full, design)) = stats
            .energy_full_mwh
            .zip(stats.energy_full_design_mwh)
            .filter(|(_, design)| *design > 0)
        {
            let health = (u64::from(full) * 100 + u64::from(design) / 2) / u64::from(design);
            stat_specs.push(PowerStatSpec {
                id: "health",
                label: "Health",
                value: format!("{}%", health.min(100)),
                caption: Some(format!(
                    "{} of {}",
                    format_watt_hours(full).trim_end_matches(" Wh"),
                    format_watt_hours(design)
                )),
                level: (health < 60).then_some("warning"),
            });
        }
        if let Some(cycles) = stats.cycle_count {
            stat_specs.push(PowerStatSpec {
                id: "cycles",
                label: "Cycles",
                value: cycles.to_string(),
                caption: Some("full charges".to_string()),
                level: None,
            });
        }
    }
    if let Some(temp) = stats.cpu_temp_celsius {
        stat_specs.push(PowerStatSpec {
            id: "cpu-temp",
            label: "CPU temp",
            value: format!("{temp} °C"),
            caption: Some("package".to_string()),
            level: if temp >= 90 {
                Some("critical")
            } else if temp >= 80 {
                Some("warning")
            } else {
                None
            },
        });
    }
    if let Some(freq) = stats.cpu_freq_mhz {
        stat_specs.push(PowerStatSpec {
            id: "cpu-clock",
            label: "CPU clock",
            value: format_gigahertz(freq),
            caption: stats
                .cpu_max_freq_mhz
                .map(|max| format!("of {} max", format_gigahertz(max))),
            level: None,
        });
    }
    if let Some(preference) = stats.cpu_energy_preference.as_deref() {
        let (value, caption) = match preference {
            "performance" => ("Speed", "energy preference"),
            "balance_performance" => ("Balanced", "leans to speed"),
            "balance_power" => ("Balanced", "leans to efficiency"),
            "power" => ("Efficiency", "energy preference"),
            "default" => ("Default", "energy preference"),
            other => (other, "energy preference"),
        };
        stat_specs.push(PowerStatSpec {
            id: "cpu-bias",
            label: "CPU bias",
            value: value.to_string(),
            caption: Some(caption.to_string()),
            level: None,
        });
    }

    PowerPageSpec {
        hero_icon,
        hero_eyebrow,
        hero_title,
        hero_detail,
        profiles,
        profile_note,
        performance_degraded,
        stats: stat_specs,
    }
}

pub(super) fn battery_hero_detail(power: &crate::PowerState) -> String {
    let stats = &power.stats;
    let held = stats
        .charge_limit_percent
        .zip(power.battery_percent)
        .filter(|(limit, percent)| percent >= limit);
    if power.charging {
        if let Some((limit, _)) = held {
            format!("Plugged in · held at {limit}%")
        } else if let Some(seconds) = stats.time_to_full_seconds {
            format!("Charging · {} to full", format_duration(seconds))
        } else if power.battery_percent == Some(100) {
            "Fully charged".to_string()
        } else {
            "Charging".to_string()
        }
    } else if let Some(seconds) = stats.time_to_empty_seconds {
        format!("On battery · {} left", format_duration(seconds))
    } else {
        "On battery".to_string()
    }
}

pub(super) fn battery_level_icon(percent: Option<u8>, charging: bool) -> &'static str {
    const DISCHARGING: [&str; 11] = [
        "battery-level-0-symbolic",
        "battery-level-10-symbolic",
        "battery-level-20-symbolic",
        "battery-level-30-symbolic",
        "battery-level-40-symbolic",
        "battery-level-50-symbolic",
        "battery-level-60-symbolic",
        "battery-level-70-symbolic",
        "battery-level-80-symbolic",
        "battery-level-90-symbolic",
        "battery-level-100-symbolic",
    ];
    const CHARGING: [&str; 11] = [
        "battery-level-0-charging-symbolic",
        "battery-level-10-charging-symbolic",
        "battery-level-20-charging-symbolic",
        "battery-level-30-charging-symbolic",
        "battery-level-40-charging-symbolic",
        "battery-level-50-charging-symbolic",
        "battery-level-60-charging-symbolic",
        "battery-level-70-charging-symbolic",
        "battery-level-80-charging-symbolic",
        "battery-level-90-charging-symbolic",
        "battery-level-100-charged-symbolic",
    ];
    let Some(percent) = percent else {
        return "battery-missing-symbolic";
    };
    let step = usize::from((percent.min(100) + 5) / 10);
    if charging {
        CHARGING[step]
    } else {
        DISCHARGING[step]
    }
}

pub(super) fn format_duration(seconds: u64) -> String {
    let minutes = (seconds + 30) / 60;
    if minutes < 60 {
        format!("{minutes} min")
    } else {
        format!("{} h {:02} min", minutes / 60, minutes % 60)
    }
}

pub(super) fn format_watts(milliwatts: u32) -> String {
    format!("{:.1} W", f64::from(milliwatts) / 1_000.0)
}

pub(super) fn format_watt_hours(milliwatt_hours: u32) -> String {
    format!("{:.1} Wh", f64::from(milliwatt_hours) / 1_000.0)
}

pub(super) fn format_gigahertz(megahertz: u32) -> String {
    format!("{:.2} GHz", f64::from(megahertz) / 1_000.0)
}

pub(super) struct PowerPage {
    pub(super) root: gtk::Box,
    pub(super) hero_icon: gtk::Image,
    pub(super) hero_eyebrow: gtk::Label,
    pub(super) hero_title: gtk::Label,
    pub(super) hero_detail: gtk::Label,
    pub(super) profile_buttons: Vec<(PowerProfile, gtk::Button)>,
    pub(super) profile_note: gtk::Label,
    pub(super) stats_eyebrow: gtk::Label,
    pub(super) stats: gtk::FlowBox,
    pub(super) stat_tiles: RefCell<Vec<(&'static str, PowerStatTile)>>,
}

pub(super) struct PowerStatTile {
    pub(super) root: gtk::Box,
    pub(super) label: gtk::Label,
    pub(super) value: gtk::Label,
    pub(super) caption: gtk::Label,
}

pub(super) fn power_page() -> PowerPage {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.add_css_class("control-page");
    root.add_css_class("power-page");

    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    hero.add_css_class("detail-hero");
    let hero_icon = gtk::Image::from_icon_name("battery-good-symbolic");
    hero_icon.add_css_class("detail-hero-icon");
    hero.append(&hero_icon);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
    text.set_hexpand(true);
    let hero_eyebrow = gtk::Label::new(None);
    hero_eyebrow.add_css_class("detail-eyebrow");
    hero_eyebrow.set_xalign(0.0);
    let hero_title = gtk::Label::new(None);
    hero_title.add_css_class("detail-hero-title");
    hero_title.set_xalign(0.0);
    let hero_detail = gtk::Label::new(None);
    hero_detail.add_css_class("supporting-text");
    hero_detail.set_xalign(0.0);
    hero_detail.set_max_width_chars(40);
    hero_detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&hero_eyebrow);
    text.append(&hero_title);
    text.append(&hero_detail);
    hero.append(&text);
    root.append(&hero);

    root.append(&section_eyebrow("PROFILE"));
    let switcher = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    switcher.add_css_class("power-profile-switcher");
    switcher.set_homogeneous(true);
    let profile_buttons = POWER_PROFILE_ORDER
        .iter()
        .map(|profile| {
            let button = gtk::Button::new();
            button.add_css_class("power-profile-option");
            button.set_has_frame(false);
            button.set_hexpand(true);
            let content = gtk::Box::new(gtk::Orientation::Vertical, 5);
            content.set_halign(gtk::Align::Center);
            let icon = gtk::Image::from_icon_name(power_profile_icon(profile));
            icon.add_css_class("power-profile-icon");
            let label = gtk::Label::new(Some(power_profile_label(profile)));
            label.add_css_class("power-profile-label");
            content.append(&icon);
            content.append(&label);
            button.set_child(Some(&content));
            switcher.append(&button);
            (profile.clone(), button)
        })
        .collect();
    root.append(&switcher);
    let profile_note = gtk::Label::new(None);
    profile_note.add_css_class("supporting-text");
    profile_note.add_css_class("power-profile-note");
    profile_note.set_xalign(0.0);
    profile_note.set_wrap(true);
    root.append(&profile_note);

    let stats_eyebrow = section_eyebrow("RIGHT NOW");
    root.append(&stats_eyebrow);
    let stats = gtk::FlowBox::new();
    stats.add_css_class("power-stats");
    stats.set_selection_mode(gtk::SelectionMode::None);
    stats.set_homogeneous(true);
    stats.set_min_children_per_line(3);
    stats.set_max_children_per_line(3);
    stats.set_column_spacing(6);
    stats.set_row_spacing(6);
    stats.set_can_focus(false);
    root.append(&stats);

    PowerPage {
        root,
        hero_icon,
        hero_eyebrow,
        hero_title,
        hero_detail,
        profile_buttons,
        profile_note,
        stats_eyebrow,
        stats,
        stat_tiles: RefCell::new(Vec::new()),
    }
}

pub(super) fn power_stat_tile() -> PowerStatTile {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 2);
    root.add_css_class("summary-tile");
    root.add_css_class("power-stat");
    let label = gtk::Label::new(None);
    label.add_css_class("summary-label");
    label.set_xalign(0.0);
    let value = gtk::Label::new(None);
    value.add_css_class("summary-value");
    value.add_css_class("power-stat-value");
    value.set_xalign(0.0);
    value.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let caption = gtk::Label::new(None);
    caption.add_css_class("power-stat-caption");
    caption.set_xalign(0.0);
    caption.set_ellipsize(gtk::pango::EllipsizeMode::End);
    root.append(&label);
    root.append(&value);
    root.append(&caption);
    PowerStatTile {
        root,
        label,
        value,
        caption,
    }
}

pub(super) fn update_power_page(page: &PowerPage, spec: &PowerPageSpec) {
    page.hero_icon.set_icon_name(Some(spec.hero_icon));
    page.hero_eyebrow.set_label(spec.hero_eyebrow);
    page.hero_title.set_label(&spec.hero_title);
    page.hero_detail.set_label(&spec.hero_detail);

    for (profile, button) in &page.profile_buttons {
        let Some(option) = spec
            .profiles
            .iter()
            .find(|option| option.profile == *profile)
        else {
            continue;
        };
        button.set_sensitive(option.available);
        button.set_tooltip_text(Some(if !option.available {
            "Not supported on this device"
        } else if option.selected {
            "Active profile"
        } else {
            power_profile_description(profile)
        }));
        if option.selected {
            button.add_css_class("selected");
        } else {
            button.remove_css_class("selected");
        }
    }
    page.profile_note.set_label(&spec.profile_note);
    if spec.performance_degraded {
        page.profile_note.add_css_class("warning");
    } else {
        page.profile_note.remove_css_class("warning");
    }

    // Rebuild the tiles only when the set of readings changes; otherwise the
    // five-second refresh just rewrites their labels.
    let mut tiles = page.stat_tiles.borrow_mut();
    let ids_match = tiles.len() == spec.stats.len()
        && tiles
            .iter()
            .zip(&spec.stats)
            .all(|((id, _), stat)| *id == stat.id);
    if !ids_match {
        page.stats.remove_all();
        tiles.clear();
        for stat in &spec.stats {
            let tile = power_stat_tile();
            page.stats.insert(&tile.root, -1);
            if let Some(child) = tile.root.parent() {
                child.set_can_focus(false);
            }
            tiles.push((stat.id, tile));
        }
    }
    for ((_, tile), stat) in tiles.iter().zip(&spec.stats) {
        tile.label.set_label(stat.label);
        tile.value.set_label(&stat.value);
        tile.caption
            .set_label(stat.caption.as_deref().unwrap_or_default());
        tile.caption.set_visible(stat.caption.is_some());
        for level in ["warning", "critical"] {
            if stat.level == Some(level) {
                tile.root.add_css_class(level);
            } else {
                tile.root.remove_css_class(level);
            }
        }
    }
    page.stats_eyebrow.set_visible(!spec.stats.is_empty());
    page.stats.set_visible(!spec.stats.is_empty());
}
