use super::*;

pub(super) fn audio_output_control_spec(output: &AudioOutputState) -> AudioOutputControlSpec {
    let alias = output
        .alias
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let port = output
        .port_description
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let label = alias.or(port).unwrap_or(&output.description).to_string();
    let detail = if output.description != label {
        output.description.clone()
    } else if let Some(port) = port.filter(|port| *port != label) {
        port.to_string()
    } else {
        match output.bus.as_deref() {
            Some("bluetooth") => "Bluetooth audio".to_string(),
            Some("usb") => "USB audio".to_string(),
            Some("pci") => "Built-in audio".to_string(),
            _ => "Audio output".to_string(),
        }
    };
    let kind = output
        .port_type
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    let name = output.name.to_lowercase();
    let icon_name = if kind.contains("headphone")
        || kind.contains("headset")
        || output.bus.as_deref() == Some("bluetooth")
        || name.contains("bluez")
    {
        "audio-headphones-symbolic"
    } else if kind.contains("hdmi") || kind.contains("display") || name.contains("hdmi") {
        "video-display-symbolic"
    } else if kind.contains("speaker") || kind.contains("line") {
        "audio-speakers-symbolic"
    } else {
        "audio-card-symbolic"
    };
    AudioOutputControlSpec {
        name: output.name.clone(),
        label,
        detail,
        icon_name: icon_name.to_string(),
        selected: output.is_default,
    }
}

#[derive(Clone)]
pub(super) struct MediaWidgets {
    pub(super) root: gtk::Overlay,
    pub(super) artwork: gtk::Picture,
    pub(super) title: gtk::Label,
    pub(super) artist: gtk::Label,
    pub(super) play: gtk::Button,
}

pub(super) struct ArtworkController {
    pub(super) request_tx: Sender<ArtworkRequest>,
    pub(super) requested: Rc<RefCell<Option<String>>>,
    pub(super) track: RefCell<Option<(String, String)>>,
    pub(super) generation: Rc<Cell<u64>>,
    pub(super) best_pixels: Rc<Cell<u64>>,
    pub(super) pending: Rc<RefCell<Option<glib::SourceId>>>,
}

impl ArtworkController {
    pub(super) fn new(window: &gtk::ApplicationWindow, widgets: &[MediaWidgets]) -> Self {
        let (request_tx, result_rx) = spawn_artwork_loader();
        let requested = Rc::new(RefCell::new(None::<String>));
        let requested_for_poll = requested.clone();
        let generation = Rc::new(Cell::new(0_u64));
        let generation_for_poll = generation.clone();
        let best_pixels = Rc::new(Cell::new(0_u64));
        let best_pixels_for_poll = best_pixels.clone();
        let widgets_for_poll = widgets.to_vec();
        let window = window.downgrade();

        glib::timeout_add_local(Duration::from_millis(100), move || {
            if window.upgrade().is_none() {
                return glib::ControlFlow::Break;
            }

            while let Ok(result) = result_rx.try_recv() {
                if !result_is_current(generation_for_poll.get(), result.generation) {
                    continue;
                }
                let result_uri = result.uri;
                let bytes = match result.bytes {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        warn!("media artwork unavailable: {error}");
                        if requested_for_poll.borrow().as_deref() == Some(result_uri.as_str()) {
                            requested_for_poll.borrow_mut().take();
                        }
                        continue;
                    }
                };
                let bytes = glib::Bytes::from_owned(bytes);
                match gtk::gdk::Texture::from_bytes(&bytes) {
                    Ok(texture) => {
                        let candidate_pixels = u64::try_from(texture.width())
                            .unwrap_or_default()
                            .saturating_mul(u64::try_from(texture.height()).unwrap_or_default());
                        if !prefer_artwork_candidate(best_pixels_for_poll.get(), candidate_pixels) {
                            continue;
                        }
                        best_pixels_for_poll.set(candidate_pixels);
                        for widget in &widgets_for_poll {
                            widget.artwork.set_paintable(Some(&texture));
                            widget.root.remove_css_class("missing-art");
                            widget.root.add_css_class("has-art");
                        }
                    }
                    Err(error) => {
                        warn!("media artwork could not be decoded: {error}");
                        if requested_for_poll.borrow().as_deref() == Some(result_uri.as_str()) {
                            requested_for_poll.borrow_mut().take();
                        }
                    }
                }
            }

            glib::ControlFlow::Continue
        });

        Self {
            request_tx,
            requested,
            track: RefCell::new(None),
            generation,
            best_pixels,
            pending: Rc::new(RefCell::new(None)),
        }
    }

    pub(super) fn show(&self, media: Option<&MediaControlSpec>, widgets: &[MediaWidgets]) {
        let next_track = media.map(|media| (media.title.clone(), media.artist.clone()));
        let track_changed = self.track.borrow().as_ref() != next_track.as_ref();
        if track_changed {
            *self.track.borrow_mut() = next_track;
            *self.requested.borrow_mut() = None;
            self.generation.set(self.generation.get().wrapping_add(1));
            self.best_pixels.set(0);
            if let Some(source) = self.pending.borrow_mut().take() {
                source.remove();
            }
            clear_artwork(widgets);
        }

        let uri = media.and_then(|media| media.art_url.as_deref());
        if !track_changed && uri.is_none() {
            return;
        }
        if self.requested.borrow().as_deref() == uri {
            return;
        }

        *self.requested.borrow_mut() = uri.map(str::to_string);
        if let Some(source) = self.pending.borrow_mut().take() {
            source.remove();
        }

        if let Some(uri) = uri {
            let uri = uri.to_string();
            let generation = self.generation.get();
            let request_tx = self.request_tx.clone();
            let requested = self.requested.clone();
            let pending = self.pending.clone();
            if self.best_pixels.get() == 0 {
                let _ = request_tx.send(ArtworkRequest { uri, generation });
                return;
            }
            *self.pending.borrow_mut() = Some(glib::timeout_add_local_once(
                Duration::from_millis(300),
                move || {
                    pending.borrow_mut().take();
                    if requested.borrow().as_deref() == Some(uri.as_str()) {
                        let _ = request_tx.send(ArtworkRequest { uri, generation });
                    }
                },
            ));
        } else if track_changed {
            clear_artwork(widgets);
        }
    }
}

pub(super) fn clear_artwork(widgets: &[MediaWidgets]) {
    for widget in widgets {
        widget
            .artwork
            .set_paintable(Option::<&gtk::gdk::Paintable>::None);
        widget.root.remove_css_class("has-art");
        widget.root.add_css_class("missing-art");
    }
}

pub(super) struct SliderWidgets {
    pub(super) row: gtk::Box,
    pub(super) scale: gtk::Scale,
    pub(super) value: gtk::Label,
}

pub(super) struct VolumeSliderWidgets {
    pub(super) slider: SliderWidgets,
    pub(super) button: gtk::Button,
}

pub(super) fn slider_row(icon_name: &str, tooltip: &str) -> SliderWidgets {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("control-slider-row");
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.add_css_class("control-slider-icon");
    icon.set_tooltip_text(Some(tooltip));
    row.append(&icon);
    let scale = control_scale();
    scale.set_tooltip_text(Some(tooltip));
    let value = gtk::Label::new(None);
    value.add_css_class("slider-value");
    value.set_xalign(0.5);
    row.append(&scale);
    row.append(&value);
    SliderWidgets { row, scale, value }
}

pub(super) fn volume_slider_row() -> VolumeSliderWidgets {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("control-slider-row");
    let mute = icon_button("audio-volume-high-symbolic", "Mute");
    mute.add_css_class("control-slider-icon");
    mute.add_css_class("volume-mute-button");
    row.append(&mute);
    let scale = control_scale();
    scale.set_tooltip_text(Some("Volume"));
    let value = gtk::Label::new(None);
    value.add_css_class("slider-value");
    value.set_xalign(0.5);
    row.append(&scale);
    row.append(&value);
    VolumeSliderWidgets {
        slider: SliderWidgets { row, scale, value },
        button: mute,
    }
}

pub(super) fn volume_icon_name(muted: bool, percent: Option<u8>) -> &'static str {
    if muted {
        return "audio-volume-muted-symbolic";
    }
    match percent.unwrap_or_default().min(100) {
        0 => "audio-volume-muted-symbolic",
        1..=33 => "audio-volume-low-symbolic",
        34..=66 => "audio-volume-medium-symbolic",
        _ => "audio-volume-high-symbolic",
    }
}

pub(super) fn reconcile_audio_outputs(
    list: &gtk::Box,
    previous: &mut Vec<AudioOutputControlSpec>,
    outputs: &[AudioOutputControlSpec],
    sender: Sender<ActionRequest>,
    errors: Rc<RefCell<ControlCenterErrors>>,
    navigation: NavigationUi,
) {
    if previous == outputs {
        return;
    }
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    *previous = outputs.to_vec();

    if outputs.is_empty() {
        let empty = gtk::Label::new(Some("No audio outputs available"));
        empty.add_css_class("empty-state");
        empty.set_xalign(0.0);
        list.append(&empty);
        return;
    }

    let handle = ActionHandle {
        sender,
        errors,
        navigation,
        focus: ControlCenterFocus::Audio,
    };
    for output in outputs {
        let row = gtk::Button::new();
        row.add_css_class("audio-output-row");
        row.set_has_frame(false);
        row.set_hexpand(true);
        row.set_tooltip_text(Some(&format!("Use {} as audio output", output.label)));
        if output.selected {
            row.add_css_class("selected");
        }

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let icon = gtk::Image::from_icon_name(&output.icon_name);
        icon.add_css_class("audio-output-icon");
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        let label = gtk::Label::new(Some(&output.label));
        label.add_css_class("audio-output-title");
        label.set_xalign(0.0);
        label.set_max_width_chars(36);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let detail = gtk::Label::new(Some(&output.detail));
        detail.add_css_class("audio-output-detail");
        detail.set_xalign(0.0);
        detail.set_max_width_chars(42);
        detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.append(&label);
        text.append(&detail);
        let selected = gtk::Image::from_icon_name("object-select-symbolic");
        selected.add_css_class("audio-output-selected");
        selected.set_visible(output.selected);
        content.append(&icon);
        content.append(&text);
        content.append(&selected);
        row.set_child(Some(&content));

        if !output.selected {
            let handle = handle.clone();
            let sink_name = output.name.clone();
            row.connect_clicked(move |_| {
                handle.send(
                    "set-output",
                    ActionIntent::SetAudioOutput {
                        sink_name: sink_name.clone(),
                    },
                );
            });
        }
        list.append(&row);
    }
}

pub(super) fn media_card(
    sender: Sender<ActionRequest>,
    errors: Rc<RefCell<ControlCenterErrors>>,
    navigation: NavigationUi,
    current: Rc<RefCell<ControlCenterSpec>>,
) -> MediaWidgets {
    let root = gtk::Overlay::new();
    root.add_css_class("media-card");
    root.add_css_class("missing-art");
    root.set_size_request(-1, 140);
    root.set_vexpand(false);
    root.set_overflow(gtk::Overflow::Hidden);

    let artwork = gtk::Picture::new();
    artwork.add_css_class("media-artwork");
    artwork.set_content_fit(gtk::ContentFit::Cover);
    artwork.set_can_shrink(true);
    // Size the card independently of the image's intrinsic aspect ratio.
    // Artwork is a non-measuring overlay, cropped to the compact card bounds.
    let canvas = gtk::Box::new(gtk::Orientation::Vertical, 0);
    canvas.set_size_request(-1, 140);
    root.set_child(Some(&canvas));
    artwork.set_halign(gtk::Align::Fill);
    artwork.set_valign(gtk::Align::Fill);
    root.add_overlay(&artwork);
    root.set_measure_overlay(&artwork, false);
    root.set_clip_overlay(&artwork, true);

    let placeholder = gtk::Image::from_icon_name("audio-x-generic-symbolic");
    placeholder.add_css_class("media-artwork-placeholder");
    placeholder.set_halign(gtk::Align::End);
    placeholder.set_valign(gtk::Align::Start);
    root.add_overlay(&placeholder);

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.add_css_class("media-content");
    content.set_halign(gtk::Align::Fill);
    content.set_valign(gtk::Align::Fill);
    content.set_hexpand(true);
    content.set_vexpand(true);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
    text.add_css_class("media-copy");
    text.set_hexpand(true);
    text.set_valign(gtk::Align::End);
    let title = gtk::Label::new(None);
    title.add_css_class("media-title");
    title.set_xalign(0.0);
    title.set_max_width_chars(24);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let artist = gtk::Label::new(None);
    artist.add_css_class("media-artist");
    artist.set_xalign(0.0);
    artist.set_max_width_chars(24);
    artist.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&title);
    text.append(&artist);

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    controls.add_css_class("media-controls");
    controls.set_valign(gtk::Align::End);
    let previous = icon_button("media-skip-backward-symbolic", "Previous");
    previous.add_css_class("media-transport-secondary");
    let play = icon_button("media-playback-start-symbolic", "Play or pause");
    play.add_css_class("media-transport-primary");
    let next = icon_button("media-skip-forward-symbolic", "Next");
    next.add_css_class("media-transport-secondary");
    let handle = ActionHandle {
        sender,
        errors,
        navigation,
        focus: ControlCenterFocus::Audio,
    };
    for (button, action, intent) in [
        (&previous, "previous", MediaControlAction::Previous),
        (&play, "play-pause", MediaControlAction::PlayPause),
        (&next, "next", MediaControlAction::Next),
    ] {
        let handle = handle.clone();
        let current = current.clone();
        button.connect_clicked(move |_| {
            let Some(player) = current
                .borrow()
                .media
                .as_ref()
                .map(|media| media.player.clone())
            else {
                return;
            };
            handle.send(
                action,
                ActionIntent::ControlMedia {
                    player,
                    action: intent,
                },
            );
        });
    }
    controls.append(&previous);
    controls.append(&play);
    controls.append(&next);
    content.append(&text);
    content.append(&controls);
    root.add_overlay(&content);
    MediaWidgets {
        root,
        artwork,
        title,
        artist,
        play,
    }
}

pub(super) fn update_media(widgets: &MediaWidgets, spec: Option<&MediaControlSpec>) {
    if let Some(media) = spec {
        widgets.title.set_label(&media.title);
        widgets.artist.set_label(&media.artist);
        widgets.play.set_icon_name(if media.playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        });
        widgets.root.set_visible(true);
    } else {
        widgets.root.set_visible(false);
    }
}
