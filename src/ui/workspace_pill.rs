//! The sliding active-workspace pill behind a workspace strip.
//!
//! While a trackpad swipe is in flight the pill sits wherever the compositor's
//! swipe progress puts it, frame by frame. Otherwise it springs to the active
//! workspace.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::swipe::{SwipeFeed, WorkspaceSwipe};

use super::morph::{MorphRect, Spring, SpringParams};

/// Critically damped regardless of motion style: the pill glides into place
/// and never overshoots or wobbles.
const PILL_SPRING: SpringParams = SpringParams {
    damping_ratio: 1.0,
    angular_frequency: 22.0,
};

thread_local! {
    static PILLS: RefCell<Vec<Weak<PillState>>> = const { RefCell::new(Vec::new()) };
}

/// Resumes every idle pill; called on the main thread when a swipe starts.
pub fn wake_all() {
    PILLS.with(|pills| {
        pills.borrow_mut().retain(|pill| match pill.upgrade() {
            Some(pill) => {
                pill.ensure_ticking();
                true
            }
            None => false,
        });
    });
}

pub struct WorkspacePill {
    root: gtk::Overlay,
    state: Rc<PillState>,
}

struct PillState {
    output_name: String,
    strip: gtk::Box,
    layer: gtk::Fixed,
    pill: gtk::Box,
    buttons: RefCell<Vec<(String, gtk::Button)>>,
    active: RefCell<Option<String>>,
    motion: RefCell<Option<PillMotion>>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
    last_frame_us: Cell<Option<i64>>,
}

struct PillMotion {
    x: Spring,
    width: Spring,
    tracking: bool,
}

impl WorkspacePill {
    /// Wraps `strip` so the pill renders beneath its buttons.
    pub fn new(strip: &gtk::Box, output_name: String) -> Self {
        let layer = gtk::Fixed::new();
        layer.set_can_target(false);
        let pill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pill.add_css_class("workspace-pill");
        pill.set_can_target(false);
        pill.set_visible(false);
        layer.put(&pill, 0.0, 0.0);

        let root = gtk::Overlay::new();
        root.set_child(Some(&layer));
        root.add_overlay(strip);
        root.set_measure_overlay(strip, true);
        strip.add_css_class("has-pill");

        let state = Rc::new(PillState {
            output_name,
            strip: strip.clone(),
            layer,
            pill,
            buttons: RefCell::new(Vec::new()),
            active: RefCell::new(None),
            motion: RefCell::new(None),
            tick: RefCell::new(None),
            last_frame_us: Cell::new(None),
        });
        PILLS.with(|pills| pills.borrow_mut().push(Rc::downgrade(&state)));
        Self { root, state }
    }

    pub fn widget(&self) -> &gtk::Overlay {
        &self.root
    }

    /// Records the strip's buttons in display order and the active workspace.
    pub fn sync(&self, buttons: Vec<(String, gtk::Button)>, active: Option<String>) {
        *self.state.buttons.borrow_mut() = buttons;
        *self.state.active.borrow_mut() = active;
        self.state.ensure_ticking();
    }
}

impl PillState {
    fn ensure_ticking(self: &Rc<Self>) {
        if self.tick.borrow().is_some() {
            return;
        }
        self.last_frame_us.set(None);
        let weak = Rc::downgrade(self);
        let id = self.layer.add_tick_callback(move |_, clock| {
            let Some(state) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let now = clock.frame_time();
            let seconds = state
                .last_frame_us
                .replace(Some(now))
                .map_or(0.0, |last| (now - last) as f64 / 1_000_000.0);
            if state.frame(seconds) {
                glib::ControlFlow::Continue
            } else {
                state.tick.take();
                glib::ControlFlow::Break
            }
        });
        *self.tick.borrow_mut() = Some(id);
    }

    /// Advances one frame; returns whether the pill needs another.
    fn frame(&self, seconds: f64) -> bool {
        let Some(strip_origin) = self
            .strip
            .compute_point(&self.layer, &gtk::graphene::Point::zero())
        else {
            return true;
        };
        let rects = self
            .buttons
            .borrow()
            .iter()
            .filter_map(|(id, button)| {
                // Allocated size and the transformed center: CSS hover and arrival
                // transforms scale about the center, so the pill ignores them.
                let (width, height) = (button.width() as f32, button.height() as f32);
                if width <= 0.0 || height <= 0.0 {
                    return None;
                }
                let bounds = button.compute_bounds(&self.layer)?;
                let center_x = bounds.x() + bounds.width() * 0.5;
                let rect = MorphRect::new(center_x - width * 0.5, strip_origin.y(), width, height);
                Some((id.clone(), rect))
            })
            .collect::<Vec<_>>();
        if rects.len() < self.buttons.borrow().len() {
            // Not laid out yet; try again next frame.
            return true;
        }

        let swipe = SwipeFeed::global().sample(&self.output_name);
        let Some(target) = pill_target(
            &rects,
            self.active.borrow().as_deref(),
            swipe.as_ref(),
            self.strip.spacing() as f32,
        ) else {
            self.pill.set_visible(false);
            self.motion.take();
            update_class(&self.strip, "pill-moving", false);
            return swipe.is_some();
        };

        let params = PILL_SPRING;
        let mut motion = self.motion.borrow_mut();
        let motion = motion.get_or_insert_with(|| PillMotion {
            x: Spring::pixels(params, f64::from(target.rect.x)),
            width: Spring::pixels(params, f64::from(target.rect.width)),
            tracking: false,
        });

        let x = f64::from(target.rect.x);
        let width = f64::from(target.rect.width);
        if target.tracking {
            // Follow the fingers exactly. The release starts at rest: carried
            // momentum would overshoot the target.
            motion.x.snap(x);
            motion.width.snap(width);
        } else {
            motion.x.retarget(x);
            motion.width.retarget(width);
            motion.x.advance(seconds);
            motion.width.advance(seconds);
        }
        motion.tracking = target.tracking;

        let rect = MorphRect::new(
            motion.x.value() as f32,
            target.rect.y,
            motion.width.value().max(0.0) as f32,
            target.rect.height,
        );
        self.layer
            .move_(&self.pill, f64::from(rect.x), f64::from(rect.y));
        self.pill
            .set_size_request(rect.width.round() as i32, rect.height.round() as i32);
        self.pill.set_visible(true);

        let moving = motion.tracking || !motion.x.is_settled() || !motion.width.is_settled();
        update_class(&self.strip, "pill-moving", moving);
        let under = moving
            .then(|| nearest_button(&rects, rect.center_x()))
            .flatten();
        for (id, button) in self.buttons.borrow().iter() {
            update_class(button, "under-pill", under == Some(id.as_str()));
        }
        moving
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PillTarget {
    rect: MorphRect,
    tracking: bool,
}

/// Where the pill belongs this frame. During a swipe it interpolates from the
/// starting workspace toward its neighbour; a neighbour the strip does not show
/// yet (a workspace about to be created) is assumed one button pitch away.
fn pill_target(
    rects: &[(String, MorphRect)],
    active: Option<&str>,
    swipe: Option<&WorkspaceSwipe>,
    spacing: f32,
) -> Option<PillTarget> {
    let rect_of = |id: &str| {
        rects
            .iter()
            .find(|(candidate, _)| candidate == id)
            .map(|(_, rect)| *rect)
    };

    if let Some(swipe) = swipe
        && let Some(from) = rect_of(&swipe.from)
    {
        let progress = swipe.progress as f32;
        let to = swipe.to.as_deref().and_then(rect_of).unwrap_or_else(|| {
            let pitch = from.width + spacing;
            from.translated(pitch * progress.signum(), 0.0)
        });
        return Some(PillTarget {
            rect: from.lerp(to, progress.abs()),
            tracking: true,
        });
    }

    active.and_then(rect_of).map(|rect| PillTarget {
        rect,
        tracking: false,
    })
}

fn nearest_button(rects: &[(String, MorphRect)], center_x: f32) -> Option<&str> {
    rects
        .iter()
        .min_by(|(_, a), (_, b)| {
            (a.center_x() - center_x)
                .abs()
                .total_cmp(&(b.center_x() - center_x).abs())
        })
        .map(|(id, _)| id.as_str())
}

fn update_class(widget: &impl IsA<gtk::Widget>, class_name: &str, present: bool) {
    if widget.has_css_class(class_name) != present {
        if present {
            widget.add_css_class(class_name);
        } else {
            widget.remove_css_class(class_name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip() -> Vec<(String, MorphRect)> {
        vec![
            ("1".to_string(), MorphRect::new(0.0, 0.0, 32.0, 28.0)),
            ("2".to_string(), MorphRect::new(34.0, 0.0, 32.0, 28.0)),
            ("3".to_string(), MorphRect::new(68.0, 0.0, 40.0, 28.0)),
        ]
    }

    fn swipe(from: &str, to: Option<&str>, progress: f64) -> WorkspaceSwipe {
        WorkspaceSwipe {
            from: from.to_string(),
            to: to.map(str::to_string),
            progress,
        }
    }

    #[test]
    fn rests_on_the_active_workspace_without_a_swipe() {
        let target = pill_target(&strip(), Some("2"), None, 2.0).expect("target");
        assert_eq!(target.rect, MorphRect::new(34.0, 0.0, 32.0, 28.0));
        assert!(!target.tracking);
        assert_eq!(pill_target(&strip(), None, None, 2.0), None);
    }

    #[test]
    fn interpolates_toward_the_neighbour_during_a_swipe() {
        let forward = pill_target(&strip(), Some("2"), Some(&swipe("2", Some("3"), 0.5)), 2.0)
            .expect("target");
        assert_eq!(forward.rect, MorphRect::new(51.0, 0.0, 36.0, 28.0));
        assert!(forward.tracking);

        let back = pill_target(
            &strip(),
            Some("2"),
            Some(&swipe("2", Some("1"), -0.25)),
            2.0,
        )
        .expect("target");
        assert_eq!(back.rect, MorphRect::new(25.5, 0.0, 32.0, 28.0));
    }

    #[test]
    fn extrapolates_toward_a_workspace_the_strip_does_not_show() {
        let target = pill_target(&strip(), Some("3"), Some(&swipe("3", Some("4"), 0.5)), 2.0)
            .expect("target");
        assert_eq!(target.rect, MorphRect::new(89.0, 0.0, 40.0, 28.0));
    }

    #[test]
    fn ignores_swipes_from_workspaces_on_other_strips() {
        let target = pill_target(&strip(), Some("1"), Some(&swipe("9", Some("10"), 0.5)), 2.0)
            .expect("target");
        assert_eq!(target.rect, MorphRect::new(0.0, 0.0, 32.0, 28.0));
        assert!(!target.tracking);
    }

    #[test]
    fn nearest_button_follows_the_pill_center() {
        assert_eq!(nearest_button(&strip(), 40.0), Some("2"));
        assert_eq!(nearest_button(&strip(), 100.0), Some("3"));
    }
}
