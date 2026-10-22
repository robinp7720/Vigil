//! Geometry for transparent overlay "stages" that host morphing surfaces.
//!
//! Stage windows ignore exclusive zones and sit at `SURFACE_MARGIN` from the top edge, so
//! a morph can start over a bar item without resizing the layer surface every frame.

use gtk4::{self as gtk, cairo, prelude::*};

use super::morph::MorphRect;

pub const BAR_HEIGHT: i32 = 44;
pub const SURFACE_MARGIN: i32 = 0;
pub const NOTIFICATION_GAP: i32 = 10;
pub const PANEL_STAGE_MARGIN_X: i32 = 0;
pub const CONTROL_CENTER_RADIUS: f32 = 0.0;
pub const CARD_RADIUS: f32 = 0.0;

const FALLBACK_TOAST_ORIGIN_WIDTH: f32 = 160.0;
const FALLBACK_TOAST_ORIGIN_HEIGHT: f32 = 36.0;

/// The bar window is anchored top/left/right at `SURFACE_MARGIN`.
pub fn bar_to_monitor(rect: MorphRect) -> MorphRect {
    rect.translated(SURFACE_MARGIN as f32, SURFACE_MARGIN as f32)
}

pub fn panel_top_in_stage() -> i32 {
    BAR_HEIGHT + SURFACE_MARGIN
}

/// Space available to an edge-attached panel below the reserved bar.
pub fn panel_available_height(monitor_height: i32) -> i32 {
    (monitor_height - panel_top_in_stage()).max(1)
}

pub fn toast_top_in_stage() -> i32 {
    BAR_HEIGHT + NOTIFICATION_GAP
}

pub fn control_center_stage_width(
    panel_width: i32,
    origin: Option<MorphRect>,
    monitor_width: i32,
) -> i32 {
    let available = (monitor_width - SURFACE_MARGIN * 2).max(1);
    let reach = origin.map_or(panel_width, |origin| {
        panel_width.max(monitor_width - SURFACE_MARGIN - origin.x.floor() as i32)
    });
    reach.clamp(1, available)
}

/// The stage's size request: it may shrink only while hidden, and grows whenever more
/// width is needed (growing a right-anchored stage never moves the visible panel).
pub fn stage_width_request(current_request: i32, needed: i32, visible: bool) -> i32 {
    if visible {
        current_request.max(needed)
    } else {
        needed
    }
}

/// The width to map origins with: the allocated width when known, unless a just-grown
/// request has not been allocated yet.
#[cfg(test)]
pub fn origin_stage_width(requested: i32, allocated: Option<i32>) -> i32 {
    allocated
        .filter(|width| *width > 0)
        .map_or(requested, |width| width.max(requested))
}

/// Clips a toast's input shape to the stage area below the bar, so the stage never takes
/// input over the island while a toast morphs out of or back into it.
pub fn toast_input_shape(shape: MorphRect) -> Option<MorphRect> {
    let top = toast_top_in_stage() as f32;
    let bottom = shape.y + shape.height;
    shape.intersection(MorphRect::new(
        shape.x,
        top,
        shape.width,
        (bottom - top).max(0.0),
    ))
}

/// Maps a monitor-space rectangle directly into `panel_morph`'s local coordinates.
/// Because the panel is top-right aligned (`halign = End`, `margin_end = 0`), its monitor
/// position depends only on `monitor_width` and `panel_width`, never on async stage resizes.
pub fn monitor_to_panel_local(rect: MorphRect, monitor_width: i32, panel_width: i32) -> MorphRect {
    let panel_left = monitor_width - SURFACE_MARGIN - PANEL_STAGE_MARGIN_X - panel_width;
    let panel_top = SURFACE_MARGIN + panel_top_in_stage();
    rect.translated(-panel_left as f32, -panel_top as f32)
}

/// A stage anchored top-right with `SURFACE_MARGIN` margins.
#[cfg(test)]
pub fn monitor_to_top_right_stage(
    rect: MorphRect,
    monitor_width: i32,
    stage_width: i32,
) -> MorphRect {
    let left = monitor_width - SURFACE_MARGIN - stage_width;
    rect.translated(-left as f32, -SURFACE_MARGIN as f32)
}

/// A horizontally centered stage with a `SURFACE_MARGIN` top margin.
pub fn toast_origin(
    island_on_monitor: Option<MorphRect>,
    monitor_width: i32,
    stage_width: i32,
) -> MorphRect {
    let island = island_on_monitor.unwrap_or_else(|| {
        MorphRect::new(
            (monitor_width as f32 - FALLBACK_TOAST_ORIGIN_WIDTH) / 2.0,
            SURFACE_MARGIN as f32 + 4.0,
            FALLBACK_TOAST_ORIGIN_WIDTH,
            FALLBACK_TOAST_ORIGIN_HEIGHT,
        )
    });
    let left = (monitor_width - stage_width) / 2;
    island.translated(-left as f32, -SURFACE_MARGIN as f32)
}

/// Keeps the window's input region equal to the given window-coordinate shapes.
///
/// Applied during every frame-clock paint so the region lands in the same commit as the
/// pixels, and GTK's own allocation-time resets are overwritten.
pub fn track_input(window: &gtk::ApplicationWindow, shapes: impl Fn() -> Vec<MorphRect> + 'static) {
    let shapes = std::rc::Rc::new(shapes);
    window.connect_realize(move |window| {
        // Start empty so the first commit never grabs pointer focus from the bar below.
        if let Some(surface) = window.surface() {
            surface.set_input_region(&cairo::Region::create());
        }
        let Some(clock) = window.frame_clock() else {
            return;
        };
        let window = window.downgrade();
        let shapes = shapes.clone();
        clock.connect_paint(move |_| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let Some(surface) = window.surface() else {
                return;
            };
            let region = cairo::Region::create();
            for shape in shapes() {
                let rectangle = cairo::RectangleInt::new(
                    shape.x.floor() as i32,
                    shape.y.floor() as i32,
                    shape.width.ceil() as i32 + 1,
                    shape.height.ceil() as i32 + 1,
                );
                let _ = region.union_rectangle(&rectangle);
            }
            surface.set_input_region(&region);
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_attached_bar_rects_keep_monitor_coordinates() {
        assert_eq!(
            bar_to_monitor(MorphRect::new(100.0, 4.0, 30.0, 28.0)),
            MorphRect::new(100.0, 4.0, 30.0, 28.0)
        );
    }

    #[test]
    fn panels_attach_below_bar_and_toasts_keep_clearance() {
        assert_eq!(SURFACE_MARGIN + panel_top_in_stage(), 44);
        assert_eq!(SURFACE_MARGIN + toast_top_in_stage(), 54);
    }

    #[test]
    fn short_outputs_keep_the_whole_panel_scrollable_below_the_bar() {
        assert_eq!(panel_available_height(800), 756);
        assert_eq!(panel_available_height(1080), 1036);
        assert_eq!(panel_available_height(20), 1);
    }

    #[test]
    fn control_center_stage_widens_to_reach_the_origin() {
        assert_eq!(control_center_stage_width(512, None, 2560), 512);
        let near = MorphRect::new(2300.0, 9.0, 30.0, 28.0);
        assert_eq!(control_center_stage_width(512, Some(near), 2560), 512);
        let far = MorphRect::new(1900.0, 9.0, 30.0, 28.0);
        assert_eq!(control_center_stage_width(512, Some(far), 2560), 660);
        let beyond = MorphRect::new(-50.0, 9.0, 30.0, 28.0);
        assert_eq!(control_center_stage_width(512, Some(beyond), 1000), 1000);
    }

    #[test]
    fn stage_requests_shrink_only_while_hidden() {
        assert_eq!(stage_width_request(800, 512, false), 512);
        assert_eq!(stage_width_request(512, 800, false), 800);
        assert_eq!(stage_width_request(800, 512, true), 800);
        assert_eq!(stage_width_request(512, 800, true), 800);
    }

    #[test]
    fn origins_map_with_the_real_stage_width_when_known() {
        assert_eq!(origin_stage_width(800, None), 800);
        assert_eq!(origin_stage_width(800, Some(0)), 800);
        assert_eq!(origin_stage_width(512, Some(640)), 640);
        // A just-grown request wins over the stale, narrower allocation.
        assert_eq!(origin_stage_width(800, Some(512)), 800);
    }

    #[test]
    fn toast_input_stays_below_the_bar() {
        let top = toast_top_in_stage() as f32;
        let arriving = MorphRect::new(0.0, 4.0, 400.0, 80.0);
        assert_eq!(
            toast_input_shape(arriving),
            Some(MorphRect::new(0.0, top, 400.0, 84.0 - top))
        );
        assert_eq!(
            toast_input_shape(MorphRect::new(0.0, 4.0, 160.0, 36.0)),
            None
        );
        let settled = MorphRect::new(0.0, top, 400.0, 90.0);
        assert_eq!(toast_input_shape(settled), Some(settled));
    }

    #[test]
    fn monitor_rects_map_into_a_top_right_stage() {
        let rect = MorphRect::new(2300.0, 9.0, 30.0, 28.0);
        assert_eq!(
            monitor_to_top_right_stage(rect, 2560, 512),
            MorphRect::new(252.0, 9.0, 30.0, 28.0)
        );
        assert_eq!(
            monitor_to_panel_local(rect, 2560, 480),
            MorphRect::new(220.0, -35.0, 30.0, 28.0)
        );
    }

    #[test]
    fn toasts_grow_from_the_island_or_a_top_center_pill() {
        let island = MorphRect::new(1080.0, 9.0, 400.0, 36.0);
        assert_eq!(
            toast_origin(Some(island), 2560, 400),
            MorphRect::new(0.0, 9.0, 400.0, 36.0)
        );
        assert_eq!(
            toast_origin(None, 1920, 400),
            MorphRect::new(120.0, 4.0, 160.0, 36.0)
        );
    }
}
