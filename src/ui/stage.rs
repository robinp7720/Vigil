//! Geometry for transparent overlay "stages" that host morphing surfaces.
//!
//! Stage windows ignore exclusive zones and sit at `SURFACE_MARGIN` from the top edge, so
//! a morph can start over a bar item without resizing the layer surface every frame.

use gtk4::{self as gtk, cairo, prelude::*};

use super::morph::MorphRect;

pub const BAR_HEIGHT: i32 = 44;
pub const SURFACE_MARGIN: i32 = 5;
pub const NOTIFICATION_GAP: i32 = 10;
pub const CONTROL_CENTER_RADIUS: f32 = 18.0;
pub const CARD_RADIUS: f32 = 14.0;

const FALLBACK_TOAST_ORIGIN_WIDTH: f32 = 160.0;
const FALLBACK_TOAST_ORIGIN_HEIGHT: f32 = 36.0;

/// The bar window is anchored top/left/right at `SURFACE_MARGIN`.
pub fn bar_to_monitor(rect: MorphRect) -> MorphRect {
    rect.translated(SURFACE_MARGIN as f32, SURFACE_MARGIN as f32)
}

pub fn panel_top_in_stage() -> i32 {
    BAR_HEIGHT + SURFACE_MARGIN
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

/// A stage anchored top-right with `SURFACE_MARGIN` margins.
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
    fn bar_rects_are_offset_by_the_layer_margins() {
        assert_eq!(
            bar_to_monitor(MorphRect::new(100.0, 4.0, 30.0, 28.0)),
            MorphRect::new(105.0, 9.0, 30.0, 28.0)
        );
    }

    #[test]
    fn stage_offsets_keep_previous_absolute_positions() {
        assert_eq!(SURFACE_MARGIN + panel_top_in_stage(), 54);
        assert_eq!(SURFACE_MARGIN + toast_top_in_stage(), 59);
    }

    #[test]
    fn control_center_stage_widens_to_reach_the_origin() {
        assert_eq!(control_center_stage_width(512, None, 2560), 512);
        let near = MorphRect::new(2300.0, 9.0, 30.0, 28.0);
        assert_eq!(control_center_stage_width(512, Some(near), 2560), 512);
        let far = MorphRect::new(1900.0, 9.0, 30.0, 28.0);
        assert_eq!(control_center_stage_width(512, Some(far), 2560), 655);
        let beyond = MorphRect::new(-50.0, 9.0, 30.0, 28.0);
        assert_eq!(control_center_stage_width(512, Some(beyond), 1000), 990);
    }

    #[test]
    fn monitor_rects_map_into_a_top_right_stage() {
        let rect = MorphRect::new(2300.0, 9.0, 30.0, 28.0);
        assert_eq!(
            monitor_to_top_right_stage(rect, 2560, 512),
            MorphRect::new(257.0, 4.0, 30.0, 28.0)
        );
    }

    #[test]
    fn toasts_grow_from_the_island_or_a_top_center_pill() {
        let island = MorphRect::new(1080.0, 9.0, 400.0, 36.0);
        assert_eq!(
            toast_origin(Some(island), 2560, 400),
            MorphRect::new(0.0, 4.0, 400.0, 36.0)
        );
        assert_eq!(
            toast_origin(None, 1920, 400),
            MorphRect::new(120.0, 4.0, 160.0, 36.0)
        );
    }
}
