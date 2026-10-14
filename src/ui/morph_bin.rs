//! A single-child container whose geometry morphs on springs.

use std::cell::{Cell, RefCell};

use gtk4::{self as gtk, gdk, glib, graphene, gsk, prelude::*, subclass::prelude::*};

use super::morph::{self, MorphRect, Spring, SpringParams, content_scale, fade_in, fade_out};
use crate::MotionStyle;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MorphOrigin {
    Window(MorphRect),
    Scaled(f32),
    Local(MorphRect),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MorphShadow {
    pub dy: f32,
    pub blur: f32,
    pub alpha: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransformKind {
    Expand,
    Collapse,
}

struct Transform {
    kind: TransformKind,
    origin: MorphOrigin,
    /// `Window` origins resolved against a real root point (local coordinates).
    resolved: Option<MorphRect>,
    /// Shape on screen when this transform interrupted another one (local coordinates).
    from: Option<MorphRect>,
    capture: bool,
    progress: Spring,
}

struct Ghost {
    paintable: gdk::Paintable,
    rect: MorphRect,
}

pub fn animations_enabled() -> bool {
    gtk::Settings::default().is_some_and(|settings| settings.is_gtk_enable_animations())
}

fn params() -> SpringParams {
    SpringParams::for_style(morph::motion_style())
}

/// Width the child's height is measured for. Unconstrained probes (`for_size < 0`) after the
/// first allocation use the width the child is actually allocated at, so the height spring
/// tracks the real layout. `follow_width` carries the child's (minimum, natural) width when
/// the bin lets the child exceed its own width.
fn child_measure_width(for_size: i32, allocated: i32, follow_width: Option<(i32, i32)>) -> i32 {
    let base = if for_size >= 0 {
        for_size
    } else if allocated > 0 {
        allocated
    } else {
        return -1;
    };
    match follow_width {
        Some((min_w, nat_w)) => nat_w.max(base).max(min_w),
        None => base,
    }
}

fn to_graphene(rect: MorphRect) -> graphene::Rect {
    graphene::Rect::new(rect.x, rect.y, rect.width, rect.height)
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MorphBin {
        pub(super) child: RefCell<Option<gtk::Widget>>,
        pub(super) follow_width: Cell<bool>,
        pub(super) follow_height: Cell<bool>,
        pub(super) start_collapsed: Cell<bool>,
        pub(super) width: RefCell<Option<Spring>>,
        pub(super) height: RefCell<Option<Spring>>,
        pub(super) radius: Cell<f32>,
        pub(super) shadow: Cell<Option<MorphShadow>>,
        pub(super) transform: RefCell<Option<Transform>>,
        pub(super) ghost: RefCell<Option<Ghost>>,
        pub(super) crossfade: RefCell<Option<Spring>>,
        pub(super) tick: RefCell<Option<gtk::TickCallbackId>>,
        pub(super) last_frame_us: Cell<Option<i64>>,
        pub(super) settled: RefCell<Option<Box<dyn FnOnce()>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MorphBin {
        const NAME: &'static str = "VigilMorphBin";
        type Type = super::MorphBin;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for MorphBin {
        fn dispose(&self) {
            if let Some(tick) = self.tick.take() {
                tick.remove();
            }
            self.settled.take();
            self.transform.take();
            self.ghost.take();
            self.crossfade.take();
            if let Some(child) = self.child.take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for MorphBin {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.child.borrow().clone() else {
                return (0, 0, -1, -1);
            };
            let horizontal = orientation == gtk::Orientation::Horizontal;
            // The child is allocated at `max(natural, bin width)` when following width and
            // at the bin width otherwise (see `size_allocate`); measure its height for
            // that same width so measure and allocate agree.
            let allocated = self.obj().width();
            let child_for = if horizontal {
                for_size
            } else {
                let (min_w, nat_w, _, _) = if self.follow_width.get() {
                    child.measure(gtk::Orientation::Horizontal, -1)
                } else {
                    (0, 0, -1, -1)
                };
                child_measure_width(
                    for_size,
                    allocated,
                    self.follow_width.get().then_some((min_w, nat_w)),
                )
            };
            let (minimum, natural, _, _) = child.measure(orientation, child_for);
            let follows = if horizontal {
                self.follow_width.get()
            } else {
                self.follow_height.get()
            };
            if !follows {
                return (minimum, natural, -1, -1);
            }
            // Only the height measured for the width the child is actually allocated at
            // (the bin's current width) drives the height spring; other `for_size` probes
            // would flip the target. Before the first allocation only the unconstrained
            // measure is available.
            let tracks = horizontal || for_size < 0 || for_size == allocated;
            let slot = if horizontal {
                &self.width
            } else {
                &self.height
            };
            let target = f64::from(natural);
            let mut start_ticking = false;
            {
                let mut spring = slot.borrow_mut();
                match spring.as_mut() {
                    None => {
                        let collapsed = self.start_collapsed.get() && animations_enabled();
                        let mut created =
                            Spring::pixels(params(), if collapsed { 0.0 } else { target });
                        created.retarget(target);
                        start_ticking = collapsed;
                        *spring = Some(created);
                    }
                    Some(existing) if tracks && (existing.target() - target).abs() >= 0.5 => {
                        if animations_enabled() && self.obj().is_mapped() {
                            existing.retarget(target);
                            start_ticking = true;
                        } else {
                            existing.snap(target);
                        }
                    }
                    Some(_) => {}
                }
            }
            if start_ticking {
                self.ensure_ticking();
            }
            let current = slot
                .borrow()
                .as_ref()
                .map_or(natural, |spring| spring.value().round().max(0.0) as i32);
            (minimum.min(current), current, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(child) = self.child.borrow().clone() else {
                return;
            };
            let (child_width, x) = if self.follow_width.get() {
                let (minimum, natural, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
                let child_width = natural.max(width).max(minimum);
                (child_width, (width - child_width) / 2)
            } else {
                (width, 0)
            };
            let child_height = if self.follow_height.get() {
                let (minimum, natural, _, _) =
                    child.measure(gtk::Orientation::Vertical, child_width);
                natural.max(height).max(minimum)
            } else {
                height
            };
            let transform = gsk::Transform::new().translate(&graphene::Point::new(x as f32, 0.0));
            child.allocate(child_width, child_height, baseline, Some(transform));
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            if self.child.borrow().is_none() {
                return;
            }
            let bounds = MorphRect::new(0.0, 0.0, obj.width() as f32, obj.height() as f32);
            // Resolve (and possibly cache) before any transform borrow is held for drawing.
            let endpoints = self.endpoints(bounds);
            match (self.transform.borrow().as_ref(), endpoints) {
                (Some(transform), Some((from, to))) => {
                    self.snapshot_transform(snapshot, bounds, transform, from, to)
                }
                _ => self.clipped(snapshot, |snapshot| {
                    self.snapshot_crossfade(snapshot, bounds)
                }),
            }
        }
    }

    impl MorphBin {
        pub(super) fn is_resizing(&self) -> bool {
            [&self.width, &self.height].into_iter().any(|slot| {
                slot.borrow()
                    .as_ref()
                    .is_some_and(|spring| !spring.is_settled())
            })
        }

        pub(super) fn is_animating(&self) -> bool {
            self.is_resizing()
                || self
                    .transform
                    .borrow()
                    .as_ref()
                    .is_some_and(|transform| !transform.progress.is_settled())
                || self
                    .crossfade
                    .borrow()
                    .as_ref()
                    .is_some_and(|spring| !spring.is_settled())
        }

        pub(super) fn ensure_ticking(&self) {
            if self.tick.borrow().is_some() {
                return;
            }
            self.last_frame_us.set(None);
            let id = self.obj().add_tick_callback(|obj, clock| {
                let imp = obj.imp();
                let now = clock.frame_time();
                let seconds = imp
                    .last_frame_us
                    .replace(Some(now))
                    .map_or(0.0, |last| (now - last) as f64 / 1_000_000.0);
                imp.advance(seconds);
                if imp.is_animating() {
                    if imp.is_resizing() {
                        obj.queue_resize();
                    } else {
                        obj.queue_draw();
                    }
                    glib::ControlFlow::Continue
                } else {
                    imp.tick.take();
                    imp.complete();
                    obj.queue_resize();
                    glib::ControlFlow::Break
                }
            });
            *self.tick.borrow_mut() = Some(id);
        }

        fn advance(&self, seconds: f64) {
            let params = params();
            for slot in [&self.width, &self.height] {
                if let Some(spring) = slot.borrow_mut().as_mut() {
                    spring.set_params(params);
                    spring.advance(seconds);
                }
            }
            if let Some(transform) = self.transform.borrow_mut().as_mut() {
                transform.progress.set_params(params);
                transform.progress.advance(seconds);
            }
            if let Some(spring) = self.crossfade.borrow_mut().as_mut() {
                spring.advance(seconds);
            }
        }

        pub(super) fn complete(&self) {
            self.transform.take();
            self.ghost.take();
            self.crossfade.take();
            self.obj().queue_draw();
            if let Some(callback) = self.settled.take() {
                callback();
            }
        }

        pub(super) fn bounds(&self) -> MorphRect {
            let obj = self.obj();
            MorphRect::new(0.0, 0.0, obj.width() as f32, obj.height() as f32)
        }

        /// Origin/target rectangle of the active transform in local coordinates.
        /// `Scaled` follows the current bounds every frame; `Window` is cached once it
        /// has been resolved against a real root point, never while bounds are empty.
        fn resolve_origin(&self, bounds: MorphRect) -> Option<MorphRect> {
            let (origin, resolved) = {
                let transform = self.transform.borrow();
                let transform = transform.as_ref()?;
                (transform.origin, transform.resolved)
            };
            let fallback = bounds.scaled_about_center(0.92);
            Some(match origin {
                MorphOrigin::Scaled(factor) => bounds.scaled_about_center(factor),
                MorphOrigin::Local(rect) => rect,
                MorphOrigin::Window(rect) => {
                    if let Some(resolved) = resolved {
                        return Some(resolved);
                    }
                    if bounds.width <= 0.0 || bounds.height <= 0.0 {
                        return Some(fallback);
                    }
                    let obj = self.obj();
                    let point = obj
                        .root()
                        .and_then(|root| obj.compute_point(&root, &graphene::Point::zero()));
                    let Some(point) = point else {
                        return Some(fallback);
                    };
                    let rect = rect.translated(-point.x(), -point.y());
                    if let Some(transform) = self.transform.borrow_mut().as_mut() {
                        transform.resolved = Some(rect);
                    }
                    rect
                }
            })
        }

        /// `(from, to)` shapes of the active transform in local coordinates.
        fn endpoints(&self, bounds: MorphRect) -> Option<(MorphRect, MorphRect)> {
            let origin = self.resolve_origin(bounds)?;
            let transform = self.transform.borrow();
            let transform = transform.as_ref()?;
            Some(match transform.kind {
                TransformKind::Expand => (transform.from.unwrap_or(origin), bounds),
                TransformKind::Collapse => (transform.from.unwrap_or(bounds), origin),
            })
        }

        /// The currently visible shape in local coordinates.
        pub(super) fn current_shape(&self) -> MorphRect {
            let bounds = self.bounds();
            let Some((from, to)) = self.endpoints(bounds) else {
                return bounds;
            };
            let progress = self
                .transform
                .borrow()
                .as_ref()
                .map_or(1.0, |transform| transform.progress.value() as f32);
            from.lerp(to, progress)
        }

        /// Runs `draw` clipped to the bounds while the size is animating.
        fn clipped(&self, snapshot: &gtk::Snapshot, draw: impl FnOnce(&gtk::Snapshot)) {
            let resizing = self.is_resizing();
            if resizing {
                snapshot.push_clip(&to_graphene(self.bounds()));
            }
            draw(snapshot);
            if resizing {
                snapshot.pop();
            }
        }

        /// Stops all motion and snaps sizes to their targets. Keeps `settled` untouched.
        pub(super) fn halt(&self) {
            if let Some(tick) = self.tick.take() {
                tick.remove();
            }
            for slot in [&self.width, &self.height] {
                if let Some(spring) = slot.borrow_mut().as_mut() {
                    let target = spring.target();
                    spring.snap(target);
                }
            }
            self.transform.take();
            self.ghost.take();
            self.crossfade.take();
        }

        pub(super) fn capture(&self) -> Option<Ghost> {
            let obj = self.obj();
            let child = self.child.borrow().clone()?;
            if !obj.is_mapped() || child.width() == 0 || child.height() == 0 {
                return None;
            }
            let bounds = child.compute_bounds(&*obj)?;
            let paintable = gtk::WidgetPaintable::new(Some(&child)).current_image();
            Some(Ghost {
                paintable,
                rect: MorphRect::new(bounds.x(), bounds.y(), bounds.width(), bounds.height()),
            })
        }

        fn snapshot_child(&self, snapshot: &gtk::Snapshot) {
            if let Some(child) = self.child.borrow().as_ref() {
                self.obj().snapshot_child(child, snapshot);
            }
        }

        fn snapshot_ghost(&self, snapshot: &gtk::Snapshot, ghost: &Ghost, x: f32, y: f32) {
            snapshot.save();
            snapshot.translate(&graphene::Point::new(x, y));
            ghost.paintable.snapshot(
                snapshot,
                f64::from(ghost.rect.width),
                f64::from(ghost.rect.height),
            );
            snapshot.restore();
        }

        fn snapshot_crossfade(&self, snapshot: &gtk::Snapshot, bounds: MorphRect) {
            let progress = self.crossfade.borrow().as_ref().map(Spring::value);
            let ghost = self.ghost.borrow();
            match (ghost.as_ref(), progress) {
                (Some(ghost), Some(progress)) => {
                    snapshot.push_opacity(fade_out(progress, 0.0, 0.4));
                    let x = (bounds.width - ghost.rect.width) / 2.0;
                    self.snapshot_ghost(snapshot, ghost, x, ghost.rect.y);
                    snapshot.pop();
                    snapshot.push_opacity(fade_in(progress, 0.2, 1.0));
                    self.snapshot_child(snapshot);
                    snapshot.pop();
                }
                _ => self.snapshot_child(snapshot),
            }
        }

        fn snapshot_transform(
            &self,
            snapshot: &gtk::Snapshot,
            bounds: MorphRect,
            transform: &Transform,
            from: MorphRect,
            to: MorphRect,
        ) {
            let progress = transform.progress.value();
            let ghost = self.ghost.borrow();
            match transform.kind {
                TransformKind::Expand => {
                    if let Some(ghost) = ghost.as_ref() {
                        self.clipped(snapshot, |snapshot| {
                            snapshot.push_opacity(fade_out(progress, 0.0, 0.4));
                            self.snapshot_ghost(snapshot, ghost, ghost.rect.x, ghost.rect.y);
                            snapshot.pop();
                        });
                    }
                    let origin = from;
                    let shape = from.lerp(to, progress as f32);
                    self.snapshot_shaped(
                        snapshot,
                        shape,
                        bounds,
                        origin,
                        fade_in(progress, 0.0, 0.3),
                        |snapshot| self.snapshot_child(snapshot),
                    );
                }
                TransformKind::Collapse => {
                    let origin = to;
                    let shape = from.lerp(to, progress as f32);
                    let opacity = fade_out(progress, 0.5, 1.0);
                    if transform.capture {
                        self.clipped(snapshot, |snapshot| {
                            snapshot.push_opacity(fade_in(progress, 0.0, 0.6));
                            self.snapshot_child(snapshot);
                            snapshot.pop();
                        });
                        if let Some(ghost) = ghost.as_ref() {
                            let full = ghost.rect;
                            self.snapshot_shaped(
                                snapshot,
                                shape,
                                full,
                                origin,
                                opacity,
                                |snapshot| self.snapshot_ghost(snapshot, ghost, full.x, full.y),
                            );
                        }
                    } else {
                        self.snapshot_shaped(
                            snapshot,
                            shape,
                            bounds,
                            origin,
                            opacity,
                            |snapshot| self.snapshot_child(snapshot),
                        );
                    }
                }
            }
        }

        /// Draws `content` (laid out at `full`) scaled into `shape` with a rounded clip.
        fn snapshot_shaped(
            &self,
            snapshot: &gtk::Snapshot,
            shape: MorphRect,
            full: MorphRect,
            origin: MorphRect,
            opacity: f64,
            content: impl FnOnce(&gtk::Snapshot),
        ) {
            let end_radius = self.radius.get();
            let start_radius = end_radius.min(origin.height / 2.0);
            let span = (full.width - origin.width).abs().max(1.0);
            let progress = ((shape.width - origin.width).abs() / span).clamp(0.0, 1.0);
            let radius = start_radius + (end_radius - start_radius) * progress;
            let outline = gsk::RoundedRect::from_rect(to_graphene(shape), radius);
            if let Some(shadow) = self.shadow.get() {
                snapshot.append_outset_shadow(
                    &outline,
                    &gdk::RGBA::new(0.0, 0.0, 0.0, shadow.alpha * opacity as f32),
                    0.0,
                    shadow.dy,
                    0.0,
                    shadow.blur,
                );
            }
            // The resize clip applies to the content only, never to the outset shadow.
            self.clipped(snapshot, |snapshot| {
                snapshot.push_opacity(opacity);
                snapshot.push_rounded_clip(&outline);
                snapshot.save();
                snapshot.translate(&graphene::Point::new(shape.x, shape.y));
                let scale = content_scale(shape, full);
                snapshot.scale(scale, scale);
                snapshot.translate(&graphene::Point::new(-full.x, -full.y));
                content(snapshot);
                snapshot.restore();
                snapshot.pop();
                snapshot.pop();
            });
        }
    }
}

glib::wrapper! {
    pub struct MorphBin(ObjectSubclass<imp::MorphBin>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for MorphBin {
    fn default() -> Self {
        Self::new()
    }
}

impl MorphBin {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_child(&self, child: Option<&impl IsA<gtk::Widget>>) {
        let imp = self.imp();
        if let Some(previous) = imp.child.take() {
            previous.unparent();
        }
        if let Some(child) = child {
            child.set_parent(self);
            *imp.child.borrow_mut() = Some(child.clone().upcast());
        }
        self.queue_resize();
    }

    pub fn child(&self) -> Option<gtk::Widget> {
        self.imp().child.borrow().clone()
    }

    pub fn set_follow(&self, width: bool, height: bool) {
        self.imp().follow_width.set(width);
        self.imp().follow_height.set(height);
        self.queue_resize();
    }

    pub fn set_start_collapsed(&self, collapsed: bool) {
        self.imp().start_collapsed.set(collapsed);
    }

    pub fn set_radius(&self, radius: f32) {
        self.imp().radius.set(radius);
    }

    pub fn set_shadow(&self, shadow: Option<MorphShadow>) {
        self.imp().shadow.set(shadow);
    }

    pub fn is_animating(&self) -> bool {
        self.imp().is_animating()
    }

    pub fn crossfade(&self, mutate: impl FnOnce()) {
        let imp = imp_with_cleared_callback(self);
        if !animations_enabled() {
            imp.halt();
            mutate();
            self.queue_resize();
            return;
        }
        let ghost = imp.capture();
        mutate();
        let Some(ghost) = ghost else {
            return;
        };
        *imp.ghost.borrow_mut() = Some(ghost);
        let mut spring = Spring::progress(SpringParams::for_style(MotionStyle::Calm));
        spring.retarget(1.0);
        *imp.crossfade.borrow_mut() = Some(spring);
        imp.ensure_ticking();
        self.queue_resize();
    }

    pub fn expand_from(&self, origin: MorphOrigin, capture: bool, mutate: impl FnOnce()) {
        self.start_transform(TransformKind::Expand, origin, capture, mutate);
    }

    pub fn collapse_into(&self, target: MorphOrigin, capture: bool, mutate: impl FnOnce()) {
        self.start_transform(TransformKind::Collapse, target, capture, mutate);
    }

    fn start_transform(
        &self,
        kind: TransformKind,
        origin: MorphOrigin,
        capture: bool,
        mutate: impl FnOnce(),
    ) {
        let imp = imp_with_cleared_callback(self);
        if !animations_enabled() {
            imp.halt();
            mutate();
            self.queue_resize();
            return;
        }
        // Interrupting a transform continues from the shape currently on screen, with
        // the spring velocity kept (same direction) or reversed (opposite direction) so
        // momentum stays continuous.
        let interrupted = imp
            .transform
            .borrow()
            .as_ref()
            .map(|transform| (transform.kind, transform.progress.velocity()));
        let (from, velocity) = match interrupted {
            Some((previous, velocity)) => {
                let shape = imp.current_shape();
                let usable = shape.width > 0.0 && shape.height > 0.0;
                let velocity = if previous == kind {
                    velocity
                } else {
                    -velocity
                };
                (usable.then_some(shape), velocity)
            }
            None => (None, 0.0),
        };
        let ghost = if capture { imp.capture() } else { None };
        mutate();
        *imp.ghost.borrow_mut() = ghost;
        imp.crossfade.take();
        let mut progress = Spring::progress(params());
        progress.restart(0.0, 1.0, velocity);
        *imp.transform.borrow_mut() = Some(Transform {
            kind,
            origin,
            resolved: None,
            from,
            capture,
            progress,
        });
        imp.ensure_ticking();
        self.queue_resize();
    }

    pub fn on_settled(&self, callback: impl FnOnce() + 'static) {
        let imp = self.imp();
        if imp.is_animating() || imp.tick.borrow().is_some() {
            // Drop any replaced callback only after the cell borrow is released.
            let previous = imp.settled.replace(Some(Box::new(callback)));
            drop(previous);
        } else {
            callback();
        }
    }

    pub fn cancel(&self) {
        let imp = self.imp();
        imp.settled.take();
        imp.halt();
        self.queue_resize();
    }

    pub fn shape_in(&self, ancestor: &impl IsA<gtk::Widget>) -> Option<MorphRect> {
        let shape = self.imp().current_shape();
        let point = self.compute_point(ancestor, &graphene::Point::zero())?;
        Some(shape.translated(point.x(), point.y()))
    }
}

fn imp_with_cleared_callback(bin: &MorphBin) -> &imp::MorphBin {
    let imp = bin.imp();
    imp.settled.take();
    imp
}

#[cfg(test)]
mod measure_width_tests {
    use super::child_measure_width;

    #[test]
    fn unconstrained_probe_uses_allocated_width() {
        assert_eq!(child_measure_width(-1, 480, None), 480);
        assert_eq!(child_measure_width(-1, 0, None), -1);
    }

    #[test]
    fn constrained_probe_keeps_for_size() {
        assert_eq!(child_measure_width(300, 480, None), 300);
    }

    #[test]
    fn following_width_never_below_natural() {
        assert_eq!(child_measure_width(100, 480, Some((50, 433))), 433);
        assert_eq!(child_measure_width(-1, 0, Some((50, 433))), -1);
    }
}
