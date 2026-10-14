//! Spring-driven geometry shared by Vigil's morphing surfaces. GTK-free.

use std::cell::Cell;

use crate::MotionStyle;

const STEP_SECONDS: f64 = 1.0 / 120.0;
const MAX_FRAME_SECONDS: f64 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpringParams {
    pub damping_ratio: f64,
    pub angular_frequency: f64,
}

impl SpringParams {
    pub fn for_style(style: MotionStyle) -> Self {
        match style {
            MotionStyle::Bold => Self {
                damping_ratio: 0.70,
                angular_frequency: 18.0,
            },
            MotionStyle::Calm => Self {
                damping_ratio: 1.0,
                angular_frequency: 22.0,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    params: SpringParams,
    value: f64,
    velocity: f64,
    target: f64,
    position_epsilon: f64,
    velocity_epsilon: f64,
    remainder: f64,
}

impl Spring {
    fn new(params: SpringParams, value: f64, position_epsilon: f64, velocity_epsilon: f64) -> Self {
        Self {
            params,
            value,
            velocity: 0.0,
            target: value,
            position_epsilon,
            velocity_epsilon,
            remainder: 0.0,
        }
    }

    /// A normalized 0..1 progress spring.
    pub fn progress(params: SpringParams) -> Self {
        Self::new(params, 0.0, 0.002, 0.05)
    }

    /// A spring measured in logical pixels.
    pub fn pixels(params: SpringParams, value: f64) -> Self {
        Self::new(params, value, 0.5, 5.0)
    }

    pub fn value(&self) -> f64 {
        self.value
    }

    pub fn velocity(&self) -> f64 {
        self.velocity
    }

    pub fn target(&self) -> f64 {
        self.target
    }

    pub fn retarget(&mut self, target: f64) {
        self.target = target;
    }

    pub fn restart(&mut self, from: f64, to: f64, velocity: f64) {
        self.value = from;
        self.target = to;
        self.velocity = velocity;
        self.remainder = 0.0;
    }

    pub fn snap(&mut self, value: f64) {
        self.restart(value, value, 0.0);
    }

    pub fn set_params(&mut self, params: SpringParams) {
        self.params = params;
    }

    pub fn advance(&mut self, seconds: f64) {
        let mut remaining = self.remainder + seconds.clamp(0.0, MAX_FRAME_SECONDS);
        while remaining >= STEP_SECONDS {
            self.step(STEP_SECONDS);
            remaining -= STEP_SECONDS;
        }
        self.remainder = remaining;
        if self.is_settled() {
            self.value = self.target;
            self.velocity = 0.0;
        }
    }

    pub fn is_settled(&self) -> bool {
        (self.value - self.target).abs() < self.position_epsilon
            && self.velocity.abs() < self.velocity_epsilon
    }

    fn step(&mut self, seconds: f64) {
        let omega = self.params.angular_frequency;
        let stiffness = omega * omega;
        let damping = 2.0 * self.params.damping_ratio * omega;
        let acceleration = -stiffness * (self.value - self.target) - damping * self.velocity;
        self.velocity += acceleration * seconds;
        self.value += self.velocity * seconds;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MorphRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl MorphRect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Unclamped interpolation so spring overshoot carries through; sizes never go negative.
    pub fn lerp(self, to: Self, t: f32) -> Self {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Self {
            x: mix(self.x, to.x),
            y: mix(self.y, to.y),
            width: mix(self.width, to.width).max(0.0),
            height: mix(self.height, to.height).max(0.0),
        }
    }

    /// Overlapping area of two rects, or `None` when they do not overlap.
    pub fn intersection(self, other: Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        (right > x && bottom > y).then(|| Self::new(x, y, right - x, bottom - y))
    }

    pub fn translated(self, dx: f32, dy: f32) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            ..self
        }
    }

    pub fn scaled_about_center(self, factor: f32) -> Self {
        let width = self.width * factor;
        let height = self.height * factor;
        Self {
            x: self.x + (self.width - width) / 2.0,
            y: self.y + (self.height - height) / 2.0,
            width,
            height,
        }
    }

    pub fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);
        Self::new(x, y, right - x, bottom - y)
    }
}

pub fn fade_in(progress: f64, start: f64, end: f64) -> f64 {
    ((progress - start) / (end - start)).clamp(0.0, 1.0)
}

pub fn fade_out(progress: f64, start: f64, end: f64) -> f64 {
    1.0 - fade_in(progress, start, end)
}

/// Uniform scale that fits full-size content to the morph shape's width.
pub fn content_scale(shape: MorphRect, full: MorphRect) -> f32 {
    if full.width <= 0.0 {
        1.0
    } else {
        (shape.width / full.width).max(0.0)
    }
}

thread_local! {
    static MOTION_STYLE: Cell<MotionStyle> = const { Cell::new(MotionStyle::Bold) };
}

pub fn set_motion_style(style: MotionStyle) {
    MOTION_STYLE.with(|cell| cell.set(style));
}

pub fn motion_style() -> MotionStyle {
    MOTION_STYLE.with(Cell::get)
}

#[cfg(test)]
mod tests {
    #[test]
    fn rect_intersection_covers_overlap_containment_and_disjoint() {
        use super::MorphRect;
        let a = MorphRect::new(0.0, 0.0, 100.0, 100.0);
        assert_eq!(
            a.intersection(MorphRect::new(50.0, 60.0, 100.0, 100.0)),
            Some(MorphRect::new(50.0, 60.0, 50.0, 40.0))
        );
        let inner = MorphRect::new(10.0, 10.0, 20.0, 20.0);
        assert_eq!(a.intersection(inner), Some(inner));
        assert_eq!(inner.intersection(a), Some(inner));
        assert_eq!(a.intersection(MorphRect::new(200.0, 0.0, 10.0, 10.0)), None);
        assert_eq!(a.intersection(MorphRect::new(100.0, 0.0, 10.0, 10.0)), None);
    }

    use super::*;
    use crate::MotionStyle;

    fn run_to_rest(params: SpringParams) -> (f64, f64) {
        let mut spring = Spring::progress(params);
        spring.retarget(1.0);
        let mut peak = 0.0_f64;
        let mut elapsed = 0.0;
        while !spring.is_settled() && elapsed < 2.0 {
            spring.advance(1.0 / 60.0);
            elapsed += 1.0 / 60.0;
            peak = peak.max(spring.value());
        }
        (peak, elapsed)
    }

    #[test]
    fn bold_spring_overshoots_slightly_and_settles() {
        let (peak, elapsed) = run_to_rest(SpringParams::for_style(MotionStyle::Bold));
        assert!(peak > 1.02 && peak <= 1.04, "peak {peak}");
        assert!(elapsed <= 0.5, "elapsed {elapsed}");
    }

    #[test]
    fn calm_spring_never_overshoots_and_settles() {
        let (peak, elapsed) = run_to_rest(SpringParams::for_style(MotionStyle::Calm));
        assert!(peak <= 1.0, "peak {peak}");
        assert!(elapsed <= 0.5, "elapsed {elapsed}");
    }

    #[test]
    fn settled_spring_lands_exactly_on_target() {
        let mut spring = Spring::pixels(SpringParams::for_style(MotionStyle::Bold), 10.0);
        spring.retarget(200.0);
        for _ in 0..120 {
            spring.advance(1.0 / 60.0);
        }
        assert!(spring.is_settled());
        assert_eq!(spring.value(), 200.0);
        assert_eq!(spring.velocity(), 0.0);
    }

    #[test]
    fn retarget_preserves_velocity() {
        let mut spring = Spring::progress(SpringParams::for_style(MotionStyle::Bold));
        spring.retarget(1.0);
        spring.advance(0.05);
        let velocity = spring.velocity();
        assert!(velocity > 0.0);
        spring.retarget(0.0);
        assert_eq!(spring.velocity(), velocity);
    }

    #[test]
    fn large_frame_gaps_are_clamped() {
        let mut spring = Spring::progress(SpringParams::for_style(MotionStyle::Bold));
        spring.retarget(1.0);
        spring.advance(10.0);
        assert!(
            spring.value() < 1.0,
            "a stalled frame must not jump to rest"
        );
    }

    #[test]
    fn rect_lerp_extrapolates_but_never_inverts_size() {
        let from = MorphRect::new(0.0, 0.0, 10.0, 10.0);
        let to = MorphRect::new(100.0, 50.0, 110.0, 60.0);
        assert_eq!(from.lerp(to, 0.5), MorphRect::new(50.0, 25.0, 60.0, 35.0));
        assert_eq!(from.lerp(to, 1.1).x, 110.0);
        assert_eq!(to.lerp(from, 2.5).width, 0.0);
    }

    #[test]
    fn rect_helpers_translate_scale_and_union() {
        let rect = MorphRect::new(10.0, 20.0, 100.0, 40.0);
        assert_eq!(
            rect.translated(-10.0, 5.0),
            MorphRect::new(0.0, 25.0, 100.0, 40.0)
        );
        assert_eq!(
            rect.scaled_about_center(0.5),
            MorphRect::new(35.0, 30.0, 50.0, 20.0)
        );
        assert_eq!(
            rect.union(MorphRect::new(0.0, 0.0, 20.0, 20.0)),
            MorphRect::new(0.0, 0.0, 110.0, 60.0)
        );
    }

    #[test]
    fn fades_are_clamped_ramps() {
        assert_eq!(fade_in(-0.2, 0.0, 0.4), 0.0);
        assert_eq!(fade_in(0.2, 0.0, 0.4), 0.5);
        assert_eq!(fade_in(1.2, 0.0, 0.4), 1.0);
        assert_eq!(fade_out(0.2, 0.0, 0.4), 0.5);
    }

    #[test]
    fn content_scale_follows_width_ratio() {
        let full = MorphRect::new(0.0, 0.0, 480.0, 600.0);
        assert_eq!(
            content_scale(MorphRect::new(0.0, 0.0, 240.0, 10.0), full),
            0.5
        );
        assert_eq!(content_scale(full, MorphRect::default()), 1.0);
    }

    #[test]
    fn motion_style_defaults_to_bold_and_can_change() {
        assert_eq!(motion_style(), MotionStyle::Bold);
        set_motion_style(MotionStyle::Calm);
        assert_eq!(motion_style(), MotionStyle::Calm);
        set_motion_style(MotionStyle::Bold);
    }
}
