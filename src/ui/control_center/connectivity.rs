use super::*;

pub(super) fn bluetooth_device_detail(device: &BluetoothDeviceState) -> String {
    let state = match device.operation {
        Some(BluetoothDeviceOperation::Connecting) => "Connecting…".to_string(),
        Some(BluetoothDeviceOperation::Disconnecting) => "Disconnecting…".to_string(),
        Some(BluetoothDeviceOperation::Pairing) => "Pairing…".to_string(),
        Some(BluetoothDeviceOperation::Forgetting) => "Forgetting…".to_string(),
        None if device.connected => "Connected".to_string(),
        None if device.paired => "Saved".to_string(),
        None => match device.rssi {
            Some(rssi) if rssi >= -55 => "Select to pair · Strong signal".to_string(),
            Some(rssi) if rssi >= -72 => "Select to pair · Good signal".to_string(),
            Some(_) => "Select to pair · Weak signal".to_string(),
            None => "Select to pair".to_string(),
        },
    };
    match device.battery_percent {
        Some(percent) if device.connected => format!("{state} · {percent}% battery"),
        _ => state,
    }
}

pub(super) fn network_control_spec(network: &crate::NetworkState) -> QuickControlSpec {
    if network.wifi_available {
        let enabled = network.wifi_enabled.unwrap_or(false);
        let wired_connection = network
            .icon_hint
            .as_deref()
            .is_some_and(|icon| icon.contains("wired"));
        let detail = if !enabled {
            "Off".to_string()
        } else if wired_connection {
            "On · Ethernet active".to_string()
        } else {
            network
                .label
                .clone()
                .unwrap_or_else(|| "Not connected".to_string())
        };
        return QuickControlSpec {
            icon_name: network
                .icon_hint
                .clone()
                .filter(|icon| icon.contains("wireless"))
                .unwrap_or_else(|| "network-wireless-symbolic".to_string()),
            label: "Wi-Fi".to_string(),
            detail,
            enabled,
            available: true,
            toggle_available: true,
        };
    }

    if network.ethernet_available {
        let enabled = network.connectivity == ConnectivityState::Connected;
        let detail = if enabled {
            network
                .label
                .clone()
                .filter(|label| !label.trim().is_empty())
                .unwrap_or_else(|| "Connected".to_string())
        } else {
            match network.connectivity {
                ConnectivityState::Connecting => "Connecting".to_string(),
                _ => "Disconnected".to_string(),
            }
        };
        return QuickControlSpec {
            icon_name: "network-wired-symbolic".to_string(),
            label: "Ethernet".to_string(),
            detail,
            enabled,
            available: true,
            toggle_available: false,
        };
    }

    QuickControlSpec {
        icon_name: "network-offline-symbolic".to_string(),
        label: "Network".to_string(),
        detail: "Unavailable".to_string(),
        enabled: false,
        available: false,
        toggle_available: false,
    }
}

pub(super) struct ConnectivityPage {
    pub(super) root: gtk::Box,
    pub(super) icon: gtk::Image,
    pub(super) eyebrow: gtk::Label,
    pub(super) state: gtk::Label,
    pub(super) detail: gtk::Label,
    pub(super) toggle_row: gtk::Box,
    pub(super) toggle_label: gtk::Label,
    pub(super) toggle: gtk::Switch,
}

pub(super) struct BluetoothPage {
    pub(super) root: gtk::Box,
    pub(super) icon: gtk::Image,
    pub(super) state: gtk::Label,
    pub(super) detail: gtk::Label,
    pub(super) toggle: gtk::Switch,
    pub(super) list: gtk::Box,
    pub(super) previous: RefCell<Option<BluetoothManagerSpec>>,
}

pub(super) struct NetworkPage {
    pub(super) connectivity: ConnectivityPage,
    pub(super) traffic_root: gtk::Box,
    pub(super) interface: gtk::Label,
    pub(super) download: TrafficGraph,
    pub(super) upload: TrafficGraph,
}

pub(super) struct TrafficGraph {
    pub(super) root: gtk::Box,
    pub(super) value: gtk::Label,
    pub(super) area: gtk::DrawingArea,
    pub(super) history: Rc<RefCell<Vec<u64>>>,
}

pub(super) fn network_page() -> NetworkPage {
    let connectivity = connectivity_page("network-wireless-symbolic", "Wi-Fi");
    let traffic_root = gtk::Box::new(gtk::Orientation::Vertical, 7);
    traffic_root.add_css_class("network-traffic");
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = section_eyebrow("LIVE TRAFFIC");
    title.set_hexpand(true);
    let interface = gtk::Label::new(None);
    interface.add_css_class("network-interface");
    interface.set_xalign(1.0);
    heading.append(&title);
    heading.append(&interface);
    let graphs = gtk::Box::new(gtk::Orientation::Horizontal, 7);
    graphs.add_css_class("network-graphs");
    graphs.set_homogeneous(true);
    let download = traffic_graph("DOWNLOAD", "download");
    let upload = traffic_graph("UPLOAD", "upload");
    graphs.append(&download.root);
    graphs.append(&upload.root);
    traffic_root.append(&heading);
    traffic_root.append(&graphs);
    connectivity.root.append(&traffic_root);
    NetworkPage {
        connectivity,
        traffic_root,
        interface,
        download,
        upload,
    }
}

pub(super) fn traffic_graph(title: &str, class_name: &str) -> TrafficGraph {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 5);
    root.add_css_class("network-graph-card");
    root.add_css_class(class_name);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let title = gtk::Label::new(Some(title));
    title.add_css_class("network-graph-title");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    let value = gtk::Label::new(Some("--"));
    value.add_css_class("network-graph-value");
    value.set_xalign(1.0);
    header.append(&title);
    header.append(&value);
    let area = gtk::DrawingArea::new();
    area.add_css_class("network-graph");
    area.set_content_height(54);
    area.set_hexpand(true);
    let history = Rc::new(RefCell::new(Vec::<u64>::new()));
    let history_for_draw = history.clone();
    area.set_draw_func(move |area, context, width, height| {
        draw_traffic_graph(area, context, width, height, &history_for_draw.borrow());
    });
    root.append(&header);
    root.append(&area);
    TrafficGraph {
        root,
        value,
        area,
        history,
    }
}

pub(super) fn draw_traffic_graph(
    area: &gtk::DrawingArea,
    context: &gtk::cairo::Context,
    width: i32,
    height: i32,
    history: &[u64],
) {
    if width <= 0 || height <= 0 {
        return;
    }
    let width = f64::from(width);
    let height = f64::from(height);
    let bottom = height - 3.0;
    let color = area.color();

    context.set_line_width(1.0);
    context.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        0.10,
    );
    for fraction in [1.0 / 3.0, 2.0 / 3.0] {
        context.move_to(0.0, height * fraction);
        context.line_to(width, height * fraction);
    }
    let _ = context.stroke();

    if history.is_empty() {
        return;
    }
    let peak = history.iter().copied().max().unwrap_or_default().max(1) as f64;
    let step = width / (NETWORK_GRAPH_SAMPLES.saturating_sub(1) as f64);
    let start_x = width - step * history.len().saturating_sub(1) as f64;
    let points = history
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            let x = start_x + step * index as f64;
            let y = bottom - ((*sample as f64 / peak) * (height - 8.0));
            (x, y)
        })
        .collect::<Vec<_>>();

    context.move_to(points[0].0, bottom);
    for (x, y) in &points {
        context.line_to(*x, *y);
    }
    context.line_to(points.last().map(|point| point.0).unwrap_or(width), bottom);
    context.close_path();
    context.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        0.13,
    );
    let _ = context.fill();

    context.move_to(points[0].0, points[0].1);
    for (x, y) in points.iter().skip(1) {
        context.line_to(*x, *y);
    }
    context.set_line_width(2.0);
    context.set_line_join(gtk::cairo::LineJoin::Round);
    context.set_line_cap(gtk::cairo::LineCap::Round);
    context.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        0.92,
    );
    let _ = context.stroke();
}

pub(super) fn connectivity_page(icon_name: &str, title: &str) -> ConnectivityPage {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    page.add_css_class("control-page");

    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    hero.add_css_class("detail-hero");
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.add_css_class("detail-hero-icon");
    hero.append(&icon);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
    text.set_hexpand(true);
    let eyebrow = gtk::Label::new(Some(title));
    eyebrow.add_css_class("detail-eyebrow");
    eyebrow.set_xalign(0.0);
    let state = gtk::Label::new(None);
    state.add_css_class("detail-hero-title");
    state.set_xalign(0.0);
    let detail = gtk::Label::new(None);
    detail.add_css_class("supporting-text");
    detail.set_xalign(0.0);
    detail.set_max_width_chars(40);
    detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&eyebrow);
    text.append(&state);
    text.append(&detail);
    hero.append(&text);
    page.append(&hero);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("detail-action-row");
    let label = gtk::Label::new(Some(&format!("Enable {title}")));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    let toggle = gtk::Switch::new();
    toggle.set_valign(gtk::Align::Center);
    row.append(&label);
    row.append(&toggle);
    page.append(&row);
    ConnectivityPage {
        root: page,
        icon,
        eyebrow,
        state,
        detail,
        toggle_row: row,
        toggle_label: label,
        toggle,
    }
}

pub(super) fn bluetooth_page() -> BluetoothPage {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    page.add_css_class("control-page");

    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    hero.add_css_class("detail-hero");
    hero.add_css_class("bluetooth-hero");
    let icon = gtk::Image::from_icon_name("bluetooth-symbolic");
    icon.add_css_class("detail-hero-icon");
    hero.append(&icon);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
    text.set_hexpand(true);
    let eyebrow = gtk::Label::new(Some("Bluetooth"));
    eyebrow.add_css_class("detail-eyebrow");
    eyebrow.set_xalign(0.0);
    let state = gtk::Label::new(None);
    state.add_css_class("detail-hero-title");
    state.set_xalign(0.0);
    let detail = gtk::Label::new(None);
    detail.add_css_class("supporting-text");
    detail.set_xalign(0.0);
    text.append(&eyebrow);
    text.append(&state);
    text.append(&detail);
    let toggle = gtk::Switch::new();
    toggle.set_valign(gtk::Align::Center);
    toggle.set_tooltip_text(Some("Turn Bluetooth on or off"));
    hero.append(&text);
    hero.append(&toggle);
    page.append(&hero);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 7);
    list.add_css_class("bluetooth-device-list");
    let scroll = gtk::ScrolledWindow::new();
    scroll.add_css_class("bluetooth-device-scroll");
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroll.set_min_content_height(120);
    scroll.set_max_content_height(700);
    scroll.set_propagate_natural_height(true);
    scroll.set_child(Some(&list));
    page.append(&scroll);

    BluetoothPage {
        root: page,
        icon,
        state,
        detail,
        toggle,
        list,
        previous: RefCell::new(None),
    }
}

pub(super) fn update_connectivity_page(page: &ConnectivityPage, spec: &QuickControlSpec) {
    page.icon.set_icon_name(Some(&spec.icon_name));
    page.eyebrow.set_label(&spec.label);
    page.state.set_label(if spec.toggle_available {
        if spec.enabled { "On" } else { "Off" }
    } else if spec.enabled {
        "Connected"
    } else {
        "Disconnected"
    });
    page.detail.set_label(&spec.detail);
    page.toggle_row.set_visible(spec.toggle_available);
    page.toggle_label
        .set_label(&format!("Enable {}", spec.label));
    page.toggle.set_active(spec.enabled);
    page.toggle.set_sensitive(spec.toggle_available);
}

pub(super) fn update_bluetooth_page(
    page: &BluetoothPage,
    spec: &BluetoothManagerSpec,
    sender: Sender<ActionRequest>,
) {
    page.icon.set_icon_name(Some(if spec.powered {
        "bluetooth-active-symbolic"
    } else {
        "bluetooth-disabled-symbolic"
    }));
    page.state
        .set_label(if spec.powered { "On" } else { "Off" });
    page.detail.set_label(if !spec.powered {
        "Turn on Bluetooth to find devices"
    } else if spec.discovering {
        match spec.connected_count {
            0 => "Scanning for nearby devices…",
            1 => "1 connected · Scanning…",
            count => {
                return page
                    .detail
                    .set_label(&format!("{count} connected · Scanning…"));
            }
        }
    } else {
        match spec.connected_count {
            0 => "No devices connected",
            1 => "1 device connected",
            count => return page.detail.set_label(&format!("{count} devices connected")),
        }
    });
    page.toggle.set_active(spec.powered);
    page.toggle.set_sensitive(spec.available);

    if page.previous.borrow().as_ref() == Some(spec) {
        return;
    }
    *page.previous.borrow_mut() = Some(spec.clone());
    while let Some(child) = page.list.first_child() {
        page.list.remove(&child);
    }

    if let Some(prompt) = spec.prompt.as_ref() {
        page.list.append(&bluetooth_pairing_prompt(prompt, sender));
        return;
    }
    if !spec.powered {
        let empty = gtk::Label::new(Some("Bluetooth is off"));
        empty.add_css_class("empty-state");
        empty.set_xalign(0.0);
        page.list.append(&empty);
        return;
    }
    if let Some(error) = spec.error.as_deref() {
        let error = gtk::Label::new(Some(error));
        error.add_css_class("bluetooth-error");
        error.set_wrap(true);
        error.set_xalign(0.0);
        page.list.append(&error);
    }

    let connected = spec
        .devices
        .iter()
        .filter(|device| device.connected)
        .cloned()
        .collect::<Vec<_>>();
    let saved = spec
        .devices
        .iter()
        .filter(|device| device.paired && !device.connected)
        .cloned()
        .collect::<Vec<_>>();
    let nearby = spec
        .devices
        .iter()
        .filter(|device| !device.paired && !device.connected)
        .cloned()
        .collect::<Vec<_>>();
    append_bluetooth_section(&page.list, "CONNECTED", &connected, sender.clone());
    append_bluetooth_section(&page.list, "SAVED DEVICES", &saved, sender.clone());
    if !nearby.is_empty() {
        append_bluetooth_section(&page.list, "NEARBY", &nearby, sender);
    } else {
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 7);
        heading.add_css_class("bluetooth-section-heading");
        let label = section_eyebrow("NEARBY");
        label.set_hexpand(true);
        heading.append(&label);
        if spec.discovering {
            let spinner = gtk::Spinner::new();
            spinner.add_css_class("bluetooth-scan-spinner");
            spinner.start();
            heading.append(&spinner);
        }
        page.list.append(&heading);
        let empty = gtk::Label::new(Some(if spec.discovering {
            "Put a device in pairing mode to find it here"
        } else {
            "No nearby devices found"
        }));
        empty.add_css_class("empty-state");
        empty.set_xalign(0.0);
        page.list.append(&empty);
    }
}

pub(super) fn append_bluetooth_section(
    list: &gtk::Box,
    title: &str,
    devices: &[BluetoothDeviceControlSpec],
    sender: Sender<ActionRequest>,
) {
    if devices.is_empty() {
        return;
    }
    list.append(&section_eyebrow(title));
    for device in devices {
        list.append(&bluetooth_device_row(device, sender.clone()));
    }
}

pub(super) fn bluetooth_device_row(
    device: &BluetoothDeviceControlSpec,
    sender: Sender<ActionRequest>,
) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    row.add_css_class("bluetooth-device-row");
    if device.connected {
        row.add_css_class("connected");
    }
    if device.error.is_some() {
        row.add_css_class("has-error");
    }

    let primary = gtk::Button::new();
    primary.add_css_class("bluetooth-device-primary");
    primary.set_has_frame(false);
    primary.set_hexpand(true);
    primary.set_sensitive(device.operation.is_none());
    let accessible_action = if device.connected {
        "Disconnect"
    } else if device.paired {
        "Connect"
    } else {
        "Pair"
    };
    let accessible_label = format!("{accessible_action} {}", device.name);
    primary.update_property(&[
        gtk::accessible::Property::Label(&accessible_label),
        gtk::accessible::Property::Description(&device.detail),
    ]);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let icon = gtk::Image::from_icon_name(&device.icon_name);
    icon.add_css_class("bluetooth-device-icon");
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    let name = gtk::Label::new(Some(&device.name));
    name.add_css_class("bluetooth-device-title");
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let detail_text = device.error.as_deref().unwrap_or(&device.detail);
    let detail = gtk::Label::new(Some(detail_text));
    detail.add_css_class("bluetooth-device-detail");
    detail.set_xalign(0.0);
    detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&name);
    text.append(&detail);
    content.append(&icon);
    content.append(&text);
    if device.operation.is_some() {
        let spinner = gtk::Spinner::new();
        spinner.start();
        spinner.add_css_class("bluetooth-device-spinner");
        content.append(&spinner);
    } else if device.connected {
        let connected = gtk::Image::from_icon_name("object-select-symbolic");
        connected.add_css_class("bluetooth-device-connected");
        content.append(&connected);
    }
    primary.set_child(Some(&content));

    let address = device.address.clone();
    let connected = device.connected;
    let paired = device.paired;
    let sender_for_primary = sender.clone();
    primary.connect_clicked(move |_| {
        let intent = if connected {
            ActionIntent::DisconnectBluetoothDevice {
                address: address.clone(),
            }
        } else if paired {
            ActionIntent::ConnectBluetoothDevice {
                address: address.clone(),
            }
        } else {
            ActionIntent::PairBluetoothDevice {
                address: address.clone(),
            }
        };
        send_bluetooth_action(&sender_for_primary, "device", intent);
    });
    row.append(&primary);

    if device.paired {
        let forget = icon_button("user-trash-symbolic", &format!("Forget {}", device.name));
        forget.add_css_class("bluetooth-forget-button");
        forget.set_sensitive(device.operation.is_none());
        let row_for_confirm = row.clone();
        let sender_for_forget = sender.clone();
        let address = device.address.clone();
        let name = device.name.clone();
        forget.connect_clicked(move |_| {
            while let Some(child) = row_for_confirm.first_child() {
                row_for_confirm.remove(&child);
            }
            row_for_confirm.add_css_class("confirming");
            let question = gtk::Label::new(Some(&format!("Forget {name}?")));
            question.set_xalign(0.0);
            question.set_hexpand(true);
            let cancel = gtk::Button::with_label("Cancel");
            cancel.add_css_class("compact-action");
            let forget_confirm = gtk::Button::with_label("Forget");
            forget_confirm.add_css_class("destructive-action");
            let row_for_cancel = row_for_confirm.clone();
            cancel.connect_clicked(move |_| {
                row_for_cancel.set_visible(false);
            });
            let sender = sender_for_forget.clone();
            let address = address.clone();
            forget_confirm.connect_clicked(move |_| {
                send_bluetooth_action(
                    &sender,
                    "forget",
                    ActionIntent::ForgetBluetoothDevice {
                        address: address.clone(),
                    },
                );
            });
            row_for_confirm.append(&question);
            row_for_confirm.append(&cancel);
            row_for_confirm.append(&forget_confirm);
        });
        row.append(&forget);
    }
    row
}

pub(super) fn bluetooth_pairing_prompt(
    prompt: &BluetoothPairingPrompt,
    sender: Sender<ActionRequest>,
) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 10);
    card.add_css_class("bluetooth-pairing-card");
    let eyebrow = section_eyebrow("PAIRING REQUEST");
    let title = gtk::Label::new(Some(&prompt.device_name));
    title.add_css_class("bluetooth-pairing-title");
    title.set_xalign(0.0);
    let instruction = gtk::Label::new(None);
    instruction.add_css_class("supporting-text");
    instruction.set_wrap(true);
    instruction.set_xalign(0.0);
    card.append(&eyebrow);
    card.append(&title);
    card.append(&instruction);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 7);
    actions.set_halign(gtk::Align::End);
    let reject = gtk::Button::with_label("Cancel");
    reject.add_css_class("compact-action");
    let sender_for_reject = sender.clone();
    let address = prompt.address.clone();
    reject.connect_clicked(move |_| {
        send_bluetooth_action(
            &sender_for_reject,
            "cancel-pairing",
            ActionIntent::CancelBluetoothPairing {
                address: address.clone(),
            },
        );
    });
    actions.append(&reject);

    match &prompt.kind {
        BluetoothPairingPromptKind::ConfirmPasskey { passkey } => {
            instruction.set_label("Confirm that this code matches the device:");
            let code = gtk::Label::new(Some(&format!("{passkey:06}")));
            code.add_css_class("bluetooth-pairing-code");
            card.append(&code);
            let accept = gtk::Button::with_label("Pair");
            accept.add_css_class("primary-action");
            connect_pairing_response(&accept, sender, prompt.id, BluetoothPairingResponse::Accept);
            actions.append(&accept);
        }
        BluetoothPairingPromptKind::EnterPinCode => {
            instruction.set_label("Enter the PIN shown by the device.");
            let entry = gtk::Entry::new();
            entry.set_placeholder_text(Some("PIN"));
            entry.set_max_length(16);
            card.append(&entry);
            let accept = gtk::Button::with_label("Continue");
            accept.add_css_class("primary-action");
            accept.set_sensitive(false);
            let accept_for_change = accept.clone();
            entry.connect_changed(move |entry| {
                let value = entry.text();
                accept_for_change.set_sensitive(
                    !value.is_empty()
                        && value.len() <= 16
                        && value
                            .chars()
                            .all(|character| character.is_ascii_alphanumeric()),
                );
            });
            let sender = sender.clone();
            let entry_for_click = entry.clone();
            let prompt_id = prompt.id;
            accept.connect_clicked(move |_| {
                send_bluetooth_action(
                    &sender,
                    "pairing-response",
                    ActionIntent::RespondBluetoothPairing {
                        prompt_id,
                        response: BluetoothPairingResponse::PinCode(
                            entry_for_click.text().to_string(),
                        ),
                    },
                );
            });
            actions.append(&accept);
        }
        BluetoothPairingPromptKind::EnterPasskey => {
            instruction.set_label("Enter the six-digit passkey shown by the device.");
            let entry = gtk::Entry::new();
            entry.set_placeholder_text(Some("000000"));
            entry.set_max_length(6);
            entry.set_input_purpose(gtk::InputPurpose::Digits);
            card.append(&entry);
            let accept = gtk::Button::with_label("Continue");
            accept.add_css_class("primary-action");
            accept.set_sensitive(false);
            let accept_for_change = accept.clone();
            entry.connect_changed(move |entry| {
                let value = entry.text();
                accept_for_change.set_sensitive(
                    !value.is_empty() && value.chars().all(|character| character.is_ascii_digit()),
                );
            });
            let sender = sender.clone();
            let entry_for_click = entry.clone();
            let prompt_id = prompt.id;
            accept.connect_clicked(move |_| {
                if let Ok(passkey) = entry_for_click.text().parse::<u32>() {
                    send_bluetooth_action(
                        &sender,
                        "pairing-response",
                        ActionIntent::RespondBluetoothPairing {
                            prompt_id,
                            response: BluetoothPairingResponse::Passkey(passkey),
                        },
                    );
                }
            });
            actions.append(&accept);
        }
        BluetoothPairingPromptKind::DisplayPinCode { pin_code } => {
            instruction.set_label("Enter this PIN on the device, then wait for it to connect:");
            let code = gtk::Label::new(Some(pin_code));
            code.add_css_class("bluetooth-pairing-code");
            card.append(&code);
        }
        BluetoothPairingPromptKind::DisplayPasskey { passkey, entered } => {
            instruction.set_label(&format!(
                "Type this code on the device · {entered}/6 digits entered"
            ));
            let code = gtk::Label::new(Some(&format!("{passkey:06}")));
            code.add_css_class("bluetooth-pairing-code");
            card.append(&code);
        }
        BluetoothPairingPromptKind::Authorize => {
            instruction.set_label("Allow this device to pair with this computer?");
            let accept = gtk::Button::with_label("Allow");
            accept.add_css_class("primary-action");
            connect_pairing_response(&accept, sender, prompt.id, BluetoothPairingResponse::Accept);
            actions.append(&accept);
        }
    }
    card.append(&actions);
    card
}

pub(super) fn connect_pairing_response(
    button: &gtk::Button,
    sender: Sender<ActionRequest>,
    prompt_id: u64,
    response: BluetoothPairingResponse,
) {
    button.connect_clicked(move |_| {
        send_bluetooth_action(
            &sender,
            "pairing-response",
            ActionIntent::RespondBluetoothPairing {
                prompt_id,
                response: response.clone(),
            },
        );
    });
}

pub(super) fn send_bluetooth_action(
    sender: &Sender<ActionRequest>,
    action: &str,
    intent: ActionIntent,
) {
    let _ = sender.send(ActionRequest {
        origin: control_center_origin(ControlCenterFocus::Bluetooth, action),
        intent,
    });
}

pub(super) fn sync_bluetooth_discovery(
    sender: &Sender<ActionRequest>,
    current: &Rc<RefCell<ControlCenterSpec>>,
    active: &Rc<Cell<bool>>,
    page_visible: bool,
) {
    let spec = current.borrow();
    let desired =
        page_visible && spec.bluetooth_manager.available && spec.bluetooth_manager.powered;
    if !page_visible && let Some(prompt) = spec.bluetooth_manager.prompt.as_ref() {
        send_bluetooth_action(
            sender,
            "cancel-pairing",
            ActionIntent::CancelBluetoothPairing {
                address: prompt.address.clone(),
            },
        );
    }
    drop(spec);
    if active.get() == desired {
        return;
    }
    send_bluetooth_action(
        sender,
        if desired { "start-scan" } else { "stop-scan" },
        ActionIntent::SetBluetoothDiscovery { enabled: desired },
    );
    active.set(desired);
}

pub(super) fn update_network_traffic(page: &NetworkPage, spec: &NetworkTrafficSpec) {
    page.traffic_root.set_visible(spec.interface.is_some());
    page.interface
        .set_label(spec.interface.as_deref().unwrap_or_default());
    update_traffic_graph(
        &page.download,
        spec.download_bytes_per_second,
        &spec.download_history,
    );
    update_traffic_graph(
        &page.upload,
        spec.upload_bytes_per_second,
        &spec.upload_history,
    );
}

pub(super) fn update_traffic_graph(graph: &TrafficGraph, rate: Option<u64>, history: &[u64]) {
    graph.value.set_label(&format_network_rate(rate));
    *graph.history.borrow_mut() = history.to_vec();
    graph.area.queue_draw();
}

pub(super) fn format_network_rate(rate: Option<u64>) -> String {
    let Some(rate) = rate else {
        return "--".to_string();
    };
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * KIB;
    const GIB: f64 = 1024.0 * MIB;
    let rate = rate as f64;
    if rate < KIB {
        format!("{rate:.0} B/s")
    } else if rate < MIB {
        format_rate_unit(rate / KIB, "KB/s")
    } else if rate < GIB {
        format_rate_unit(rate / MIB, "MB/s")
    } else {
        format_rate_unit(rate / GIB, "GB/s")
    }
}

pub(super) fn format_rate_unit(value: f64, unit: &str) -> String {
    if value < 10.0 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.0} {unit}")
    }
}
