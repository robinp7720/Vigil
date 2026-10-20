//! A transient on-screen display for volume, microphone, and brightness keys,
//! and the countdown hypridle shows before an idle lock.
//!
//! It is a small, click-through overlay near the bottom of the focused output.
//! Hyprland animates its arrival (namespace `vigil-osd`), so it never steals
//! focus or pointer input from the window underneath.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk4::{self as gtk, cairo, gdk};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::{OsdContent, OsdKind};

use super::surface::monitors_by_connector;

const LEVEL_VISIBLE_FOR: Duration = Duration::from_millis(1400);
const COUNTDOWN_TICK: Duration = Duration::from_secs(1);
const OSD_BOTTOM_MARGIN: i32 = 96;
const OSD_WIDTH: i32 = 320;

#[derive(Clone, Debug, PartialEq)]
struct Presentation {
    icon: &'static str,
    title: String,
    fraction: f64,
    value: String,
    muted: bool,
}

fn level_presentation(kind: OsdKind, percent: u8, muted: bool) -> Presentation {
    let (icon, title) = match kind {
        OsdKind::Volume if muted => ("audio-volume-muted-symbolic", "Volume muted"),
        OsdKind::Volume if percent == 0 => ("audio-volume-muted-symbolic", "Volume"),
        OsdKind::Volume if percent < 34 => ("audio-volume-low-symbolic", "Volume"),
        OsdKind::Volume if percent < 67 => ("audio-volume-medium-symbolic", "Volume"),
        OsdKind::Volume => ("audio-volume-high-symbolic", "Volume"),
        OsdKind::Microphone if muted => {
            ("microphone-sensitivity-muted-symbolic", "Microphone muted")
        }
        OsdKind::Microphone => ("audio-input-microphone-symbolic", "Microphone"),
        OsdKind::Brightness => ("display-brightness-symbolic", "Brightness"),
    };
    Presentation {
        icon,
        title: title.to_string(),
        fraction: f64::from(percent.min(100)) / 100.0,
        value: format!("{percent}%"),
        muted,
    }
}

fn countdown_presentation(remaining: u32, total: u32) -> Presentation {
    Presentation {
        icon: "system-lock-screen-symbolic",
        title: format!("Locking in {remaining}s"),
        fraction: if total == 0 {
            0.0
        } else {
            f64::from(remaining) / f64::from(total)
        },
        value: "Move to stay".to_string(),
        muted: false,
    }
}

struct Widgets {
    window: gtk::ApplicationWindow,
    icon: gtk::Image,
    title: gtk::Label,
    bar: gtk::ProgressBar,
    value: gtk::Label,
    output: Option<String>,
}

#[derive(Default)]
pub(super) struct Osd {
    widgets: Rc<RefCell<Option<Widgets>>>,
    timer: Rc<RefCell<Option<glib::SourceId>>>,
}

impl Osd {
    /// Shows `content` on `output`, or the first monitor when none is known.
    pub fn show(&mut self, content: &OsdContent, output: Option<&str>) -> bool {
        let Some((connector, monitor)) = pick_monitor(output) else {
            return false;
        };
        if !self.ensure_window(&connector, &monitor) {
            return false;
        }
        self.cancel_timer();
        match *content {
            OsdContent::Level {
                kind,
                percent,
                muted,
            } => {
                self.render(&level_presentation(kind, percent, muted), false);
                self.schedule_hide(LEVEL_VISIBLE_FOR);
            }
            OsdContent::LockCountdown { seconds } => {
                self.render(&countdown_presentation(seconds, seconds), true);
                self.start_countdown(seconds);
            }
        }
        true
    }

    pub fn hide(&mut self) {
        self.cancel_timer();
        hide_window(&self.widgets);
    }

    fn ensure_window(&mut self, connector: &str, monitor: &gdk::Monitor) -> bool {
        let mut widgets = self.widgets.borrow_mut();
        if widgets.is_none() {
            let Some(application) = gtk::gio::Application::default()
                .and_then(|application| application.downcast::<gtk::Application>().ok())
            else {
                return false;
            };
            *widgets = Some(build_widgets(&application));
        }
        let widgets = widgets.as_mut().expect("widgets were just built");
        if widgets.output.as_deref() != Some(connector) {
            // Layer surfaces bind to an output when mapped, so remap to move it.
            widgets.window.set_visible(false);
            widgets.window.set_monitor(Some(monitor));
            widgets.output = Some(connector.to_string());
        }
        true
    }

    fn render(&self, presentation: &Presentation, countdown: bool) {
        let widgets = self.widgets.borrow();
        let Some(widgets) = widgets.as_ref() else {
            return;
        };
        apply_presentation(widgets, presentation);
        set_class(&widgets.window, "osd-countdown", countdown);
        widgets.window.present();
    }

    fn schedule_hide(&self, after: Duration) {
        let widgets = Rc::clone(&self.widgets);
        let timer = Rc::clone(&self.timer);
        let source = glib::timeout_add_local_once(after, move || {
            timer.borrow_mut().take();
            hide_window(&widgets);
        });
        *self.timer.borrow_mut() = Some(source);
    }

    fn start_countdown(&self, total: u32) {
        let widgets = Rc::clone(&self.widgets);
        let timer = Rc::clone(&self.timer);
        let mut remaining = total;
        let source = glib::timeout_add_local(COUNTDOWN_TICK, move || {
            remaining = remaining.saturating_sub(1);
            if remaining == 0 {
                // hypridle locks now; the lock surface replaces this one.
                timer.borrow_mut().take();
                hide_window(&widgets);
                return glib::ControlFlow::Break;
            }
            if let Some(widgets) = widgets.borrow().as_ref() {
                apply_presentation(widgets, &countdown_presentation(remaining, total));
            }
            glib::ControlFlow::Continue
        });
        *self.timer.borrow_mut() = Some(source);
    }

    fn cancel_timer(&self) {
        if let Some(source) = self.timer.borrow_mut().take() {
            source.remove();
        }
    }
}

fn pick_monitor(output: Option<&str>) -> Option<(String, gdk::Monitor)> {
    let display = gdk::Display::default()?;
    let mut monitors = monitors_by_connector(&display);
    if let Some(output) = output
        && let Some(monitor) = monitors.remove(output)
    {
        return Some((output.to_string(), monitor));
    }
    monitors.into_iter().next()
}

fn build_widgets(application: &gtk::Application) -> Widgets {
    let window = gtk::ApplicationWindow::builder()
        .application(application)
        .title("Vigil On-Screen Display")
        .build();
    window.set_decorated(false);
    window.add_css_class("osd-window");
    window.init_layer_shell();
    window.set_namespace(Some("vigil-osd"));
    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::None);
    window.set_anchor(Edge::Bottom, true);
    window.set_margin(Edge::Bottom, OSD_BOTTOM_MARGIN);
    window.set_exclusive_zone(-1);
    window.connect_realize(|window| {
        // Click-through: the display is informational only.
        if let Some(surface) = window.surface() {
            surface.set_input_region(&cairo::Region::create());
        }
    });

    let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    card.add_css_class("osd-card");
    card.set_size_request(OSD_WIDTH, -1);

    let icon = gtk::Image::new();
    icon.add_css_class("osd-icon");
    icon.set_pixel_size(20);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.set_hexpand(true);
    column.set_valign(gtk::Align::Center);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = gtk::Label::new(None);
    title.add_css_class("osd-title");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    let value = gtk::Label::new(None);
    value.add_css_class("osd-value");
    value.set_xalign(1.0);
    header.append(&title);
    header.append(&value);

    let bar = gtk::ProgressBar::new();
    bar.add_css_class("osd-bar");

    column.append(&header);
    column.append(&bar);
    card.append(&icon);
    card.append(&column);
    window.set_child(Some(&card));

    Widgets {
        window,
        icon,
        title,
        bar,
        value,
        output: None,
    }
}

fn apply_presentation(widgets: &Widgets, presentation: &Presentation) {
    widgets.icon.set_icon_name(Some(presentation.icon));
    widgets.title.set_label(&presentation.title);
    widgets.value.set_label(&presentation.value);
    widgets.bar.set_fraction(presentation.fraction);
    set_class(&widgets.window, "osd-muted", presentation.muted);
}

fn hide_window(widgets: &Rc<RefCell<Option<Widgets>>>) {
    if let Some(widgets) = widgets.borrow().as_ref() {
        widgets.window.set_visible(false);
    }
}

fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, enabled: bool) {
    if enabled {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

#[cfg(test)]
mod tests {
    use super::{countdown_presentation, level_presentation};
    use crate::OsdKind;

    #[test]
    fn volume_icon_follows_level_and_mute() {
        assert_eq!(
            level_presentation(OsdKind::Volume, 20, false).icon,
            "audio-volume-low-symbolic"
        );
        assert_eq!(
            level_presentation(OsdKind::Volume, 50, false).icon,
            "audio-volume-medium-symbolic"
        );
        assert_eq!(
            level_presentation(OsdKind::Volume, 90, false).icon,
            "audio-volume-high-symbolic"
        );
        let muted = level_presentation(OsdKind::Volume, 90, true);
        assert_eq!(muted.icon, "audio-volume-muted-symbolic");
        assert_eq!(muted.title, "Volume muted");
        assert!(muted.muted);
    }

    #[test]
    fn level_fraction_is_capped_at_full() {
        let boosted = level_presentation(OsdKind::Volume, 130, false);
        assert_eq!(boosted.fraction, 1.0);
        assert_eq!(boosted.value, "130%");
        assert_eq!(
            level_presentation(OsdKind::Brightness, 45, false).fraction,
            0.45
        );
    }

    #[test]
    fn countdown_drains_toward_the_lock() {
        let start = countdown_presentation(30, 30);
        assert_eq!(start.title, "Locking in 30s");
        assert_eq!(start.fraction, 1.0);
        assert_eq!(countdown_presentation(15, 30).fraction, 0.5);
        assert_eq!(countdown_presentation(0, 0).fraction, 0.0);
    }
}
