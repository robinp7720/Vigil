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
                damping_ratio: 0.38,
                angular_frequency: 18.0,
            },
            MotionStyle::Calm => Self {
                damping_ratio: 1.0,
                angular_frequency: 22.0,
            },
        }
    }

    /// Coupled secondary-axis spring parameters so width and height oscillate out
    /// of phase during a playful morph bounce.
    pub fn cross_for_style(style: MotionStyle) -> Self {
        match style {
            MotionStyle::Bold => Self {
                damping_ratio: 0.34,
                angular_frequency: 13.5,
            },
            MotionStyle::Calm => Self::for_style(MotionStyle::Calm),
        }
    }

    /// Smoothly damped spring for widget layout allocations (`follow_width` / `follow_height`)
    /// so Wayland layer-shell buffers do not thrash with repeated resize oscillations.
    pub fn layout_for_style(style: MotionStyle) -> Self {
        match style {
            MotionStyle::Bold => Self {
                damping_ratio: 0.82,
                angular_frequency: 26.0,
            },
            MotionStyle::Calm => Self::for_style(MotionStyle::Calm),
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

    pub fn center_x(self) -> f32 {
        self.x + self.width * 0.5
    }

    pub fn center_y(self) -> f32 {
        self.y + self.height * 0.5
    }
}

fn safe_clamp(val: f32, lo: f32, hi: f32) -> f32 {
    if !val.is_finite() || !lo.is_finite() || !hi.is_finite() {
        return 0.0;
    }
    let (min_b, max_b) = if lo <= hi { (lo, hi) } else { (hi, lo) };
    val.max(min_b).min(max_b)
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let span = (edge1 - edge0).max(1e-5);
    let t = safe_clamp((x - edge0) / span, 0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Strength `0..=1` of the Genie funnel pinch at the click target neck.
pub fn genie_pinch(progress_y: f32, collapsing: bool, style: MotionStyle) -> f32 {
    if style == MotionStyle::Calm {
        return 0.0;
    }
    if collapsing {
        smoothstep(0.0, 0.28, progress_y)
    } else {
        1.0 - smoothstep(0.32, 0.88, progress_y)
    }
}

/// Horizontal `(x, width)` span for a Genie funnel slice at normalized vertical coordinate
/// `v` (`0.0..=1.0`) within `shape`, smoothly curving from the click target `origin` neck
/// to the emerging/collapsing `shape` body via a Hermite S-curve.
pub fn genie_slice_span(
    origin: MorphRect,
    full: MorphRect,
    shape: MorphRect,
    v: f32,
    progress_y: f32,
    collapsing: bool,
    style: MotionStyle,
) -> (f32, f32) {
    let pinch = genie_pinch(progress_y, collapsing, style);
    if pinch <= 0.004 {
        return (shape.x, shape.width);
    }
    let origin_v = if full.height > 1.0 {
        safe_clamp((origin.center_y() - full.y) / full.height, 0.0, 1.0)
    } else {
        0.0
    };
    let dist = if origin_v <= 0.5 { v } else { 1.0 - v };
    let d = safe_clamp(dist, 0.0, 1.0);
    let s = d * d * (3.0 - 2.0 * d);

    let neck_x = shape.x + (origin.x - shape.x) * pinch;
    let neck_w = (shape.width + (origin.width - shape.width) * pinch).max(1.0);

    let body_lag = if collapsing {
        let lag = 1.0 - smoothstep(0.08, 0.82, progress_y);
        lag * smoothstep(0.0, 0.25, progress_y) * 0.55
    } else {
        0.0
    };
    let body_x = shape.x + (full.x - shape.x) * body_lag;
    let body_w = (shape.width + (full.width - shape.width) * body_lag).max(1.0);

    let slice_x = neck_x + (body_x - neck_x) * s;
    let slice_w = (neck_w + (body_w - neck_w) * s).max(1.0);
    (slice_x, slice_w)
}

/// Interpolates `from` toward `to` with coupled two-axis spring progress and
/// volume-conserving squash-and-stretch so popups grow out from their click target
/// and deform their window shape through multiple elastic aftershock bounces.
pub fn morph_bounce_rect(
    from: MorphRect,
    to: MorphRect,
    progress_x: f32,
    progress_y: f32,
    velocity_y: f32,
    style: MotionStyle,
) -> MorphRect {
    if style == MotionStyle::Calm {
        return from.lerp(to, progress_y);
    }
    let mix = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let base_x = mix(from.x, to.x, progress_y);
    let base_y = mix(from.y, to.y, progress_y);
    let base_w = mix(from.width, to.width, progress_y).max(0.0);
    let base_h = mix(from.height, to.height, progress_y).max(0.0);

    // Ramp in the squash-and-stretch bounce deformation only as the popup approaches
    // its full size (`progress_y >= 0.55`) so the initial emergence starts at the exact
    // click-target width and position without premature bulging.
    let env = smoothstep(0.55, 0.95, progress_y);
    let over_x = progress_x - 1.0;
    let over_y = progress_y - 1.0;
    let phase_diff = progress_x - progress_y;
    let vel = safe_clamp(velocity_y / 22.0, -0.32, 0.32);

    // Ensure narrow-delta morphs (e.g. wide island -> toast) still exhibit a visible bounce.
    let min_span_w = to.width.max(from.width) * 0.32;
    let min_span_h = to.height.max(from.height) * 0.28;
    let extra_w = (min_span_w - (to.width - from.width).abs()).max(0.0) * over_x * env;
    let extra_h = (min_span_h - (to.height - from.height).abs()).max(0.0) * over_y * env;

    // Volume-conserving squash-and-stretch: when stretching tall (+stretch_y), width pinches;
    // when squashing short (-stretch_y), width bulges outward.
    let stretch_y = env * (over_y * 0.28 + vel * 0.24 - phase_diff * 0.38);
    let stretch_x = env * (-over_y * 0.36 - vel * 0.22 + phase_diff * 0.44);

    let width = (base_w + extra_w + to.width * stretch_x).max(0.0);
    let height = (base_h + extra_h + to.height * stretch_y).max(0.0);
    let x = base_x + (base_w - width) * 0.5;
    let y = base_y + (base_h - height) * 0.15;

    // Keep the top edge anchored to the click target (`from.y`) while the Genie neck is
    // pinched so the popup visibly grows downward out of the clicked button.
    let tether = 1.0 - smoothstep(0.32, 0.84, progress_y);
    if tether > 0.0 && from.y < y {
        let tethered_top = y + (from.y - y) * tether;
        let bottom = (y + height).max(tethered_top + from.height.min(height));
        return MorphRect::new(x, tethered_top, width, bottom - tethered_top);
    }

    MorphRect::new(x, y, width, height)
}

pub fn fade_in(progress: f64, start: f64, end: f64) -> f64 {
    ((progress - start) / (end - start)).clamp(0.0, 1.0)
}

pub fn fade_out(progress: f64, start: f64, end: f64) -> f64 {
    1.0 - fade_in(progress, start, end)
}

/// Non-uniform `(scale_x, scale_y)` that maps `full` into `shape` so squash-and-stretch
/// bounce deformations carry through to the popup content.
pub fn content_scales(shape: MorphRect, full: MorphRect) -> (f32, f32) {
    let sx = if full.width <= 0.0 {
        1.0
    } else {
        (shape.width / full.width).max(0.0)
    };
    let sy = if full.height <= 0.0 {
        sx
    } else {
        (shape.height / full.height).max(0.0)
    };
    (sx, sy)
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

    fn run_to_rest(params: SpringParams) -> (f64, f64, f64) {
        let mut spring = Spring::progress(params);
        spring.retarget(1.0);
        let mut peak = 0.0_f64;
        let mut min_after_peak = 1.0_f64;
        let mut passed_peak = false;
        let mut elapsed = 0.0;
        while !spring.is_settled() && elapsed < 2.0 {
            let prev = spring.value();
            spring.advance(1.0 / 60.0);
            elapsed += 1.0 / 60.0;
            peak = peak.max(spring.value());
            if spring.value() < prev && prev > 1.0 {
                passed_peak = true;
            }
            if passed_peak {
                min_after_peak = min_after_peak.min(spring.value());
            }
        }
        (peak, min_after_peak, elapsed)
    }

    #[test]
    fn bold_spring_bounces_playfully_and_settles() {
        let (peak, min_after_peak, elapsed) =
            run_to_rest(SpringParams::for_style(MotionStyle::Bold));
        assert!(peak > 1.15 && peak <= 1.35, "peak {peak}");
        assert!(
            min_after_peak < 0.96,
            "expected rebound oscillation below 0.96, got {min_after_peak}"
        );
        assert!(elapsed <= 0.85, "elapsed {elapsed}");
    }

    #[test]
    fn calm_spring_never_overshoots_and_settles() {
        let (peak, _, elapsed) = run_to_rest(SpringParams::for_style(MotionStyle::Calm));
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
    fn morph_bounce_rect_matches_endpoints_and_deforms_mid_bounce() {
        let from = MorphRect::new(200.0, 4.0, 120.0, 28.0);
        let to = MorphRect::new(0.0, 48.0, 480.0, 520.0);
        assert_eq!(
            morph_bounce_rect(from, to, 0.0, 0.0, 0.0, MotionStyle::Bold),
            from
        );
        assert_eq!(
            morph_bounce_rect(from, to, 1.0, 1.0, 0.0, MotionStyle::Bold),
            to
        );
        let stretched = morph_bounce_rect(from, to, 0.92, 1.20, 4.0, MotionStyle::Bold);
        assert!(stretched.height > to.height);
        assert!(stretched.width < to.width);
        let squashed = morph_bounce_rect(from, to, 1.18, 0.94, -4.0, MotionStyle::Bold);
        assert!(squashed.width > to.width);
        assert!(squashed.height < to.height);
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
    fn content_scales_follow_width_and_height_ratios() {
        let full = MorphRect::new(0.0, 0.0, 480.0, 600.0);
        assert_eq!(
            content_scales(MorphRect::new(0.0, 0.0, 240.0, 300.0), full),
            (0.5, 0.5)
        );
        assert_eq!(content_scales(full, MorphRect::default()), (1.0, 1.0));
    }

    #[test]
    fn genie_slice_forms_s_curve_funnel_from_click_target_and_unfurls_for_bounce() {
        let origin = MorphRect::new(220.0, -45.0, 36.0, 28.0);
        let full = MorphRect::new(0.0, 0.0, 480.0, 560.0);
        let mid_shape = morph_bounce_rect(origin, full, 0.38, 0.42, 6.0, MotionStyle::Bold);

        let (neck_x, neck_w) =
            genie_slice_span(origin, full, mid_shape, 0.0, 0.42, false, MotionStyle::Bold);
        let (mid_x, mid_w) =
            genie_slice_span(origin, full, mid_shape, 0.5, 0.42, false, MotionStyle::Bold);
        let (body_x, body_w) =
            genie_slice_span(origin, full, mid_shape, 1.0, 0.42, false, MotionStyle::Bold);

        // Neck stays pinched toward the click target while the body widens along an S-curve.
        assert!(neck_w < mid_w && mid_w < body_w);
        assert!(
            (neck_x - origin.x).abs() < (mid_x - origin.x).abs()
                && (mid_x - origin.x).abs() < (body_x - origin.x).abs()
        );

        // Once the popup reaches the bounce phase (progress >= 0.88), the funnel is fully unfurled.
        let bounce_shape = morph_bounce_rect(origin, full, 0.94, 1.18, 3.0, MotionStyle::Bold);
        assert_eq!(
            genie_slice_span(
                origin,
                full,
                bounce_shape,
                0.0,
                1.18,
                false,
                MotionStyle::Bold
            ),
            (bounce_shape.x, bounce_shape.width)
        );

        // Calm style disables the genie funnel completely.
        assert_eq!(
            genie_slice_span(origin, full, mid_shape, 0.0, 0.42, false, MotionStyle::Calm),
            (mid_shape.x, mid_shape.width)
        );
    }

    #[test]
    fn motion_style_defaults_to_bold_and_can_change() {
        assert_eq!(motion_style(), MotionStyle::Bold);
        set_motion_style(MotionStyle::Calm);
        assert_eq!(motion_style(), MotionStyle::Calm);
        set_motion_style(MotionStyle::Bold);
    }
}
