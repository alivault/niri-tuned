//! Whole-screen zoom: a screen magnifier, driven by the modifier held with a touchpad pinch.
//!
//! The zoomed view follows the pointer proportionally (macOS' "continuously with pointer"): for a
//! focus point `p` (the pointer, in output-local logical coordinates) and a zoom level `z`, the
//! view's top-left corner is at `p × (1 − 1/z)`. The point under the pointer is then drawn at the
//! pointer itself, at every zoom level. So:
//!
//! - a pointer at an output edge shows that edge of the output;
//! - the point under the fingers stays put while pinching (the pointer doesn't move);
//! - pointer and tablet input need no transform: the cursor is drawn where it is, over what it
//!   points at. Only touch input, which lands on the zoomed picture, goes through
//!   [`screen_to_content`].

use std::cell::RefCell;

use smithay::utils::{Logical, Point, Size};

use crate::render_helpers::offscreen::{OffscreenBuffer, OffscreenData};

/// Levels below this snap to exactly 1, so that pinching back really turns the zoom off.
pub const SNAP_TO_OFF: f64 = 1.02;

/// Zoom state of one output.
#[derive(Debug)]
pub struct OutputScreenZoom {
    /// Zoom level, 1 (off) to the configured maximum.
    pub level: f64,
    /// The unzoomed output, one buffer per render target.
    pub offscreen: [OffscreenBuffer; 3],
    /// Render states of the last offscreen render for the output itself, so that the surfaces in
    /// it keep getting frame callbacks.
    pub offscreen_data: RefCell<Option<OffscreenData>>,
}

impl Default for OutputScreenZoom {
    fn default() -> Self {
        Self {
            level: 1.,
            offscreen: Default::default(),
            offscreen_data: RefCell::new(None),
        }
    }
}

impl OutputScreenZoom {
    pub fn is_zoomed(&self) -> bool {
        self.level > 1.
    }

    /// Sets the level, already clamped. Returns whether it changed.
    pub fn set_level(&mut self, level: f64) -> bool {
        if self.level == level {
            return false;
        }
        self.level = level;
        if !self.is_zoomed() {
            // Free the textures.
            self.offscreen = Default::default();
            self.offscreen_data.replace(None);
        }
        true
    }
}

/// A Mod + pinch in progress.
#[derive(Debug, Clone)]
pub struct ScreenZoomPinch {
    pub output: smithay::output::Output,
    /// The output's zoom level when the pinch began.
    pub start_level: f64,
}

/// Clamps a zoom level to 1..=`max`; levels close to 1 snap to exactly 1.
pub fn clamp_level(level: f64, max: f64) -> f64 {
    let max = if max.is_finite() { max.max(1.) } else { 1. };
    if level.is_nan() {
        return 1.;
    }
    let level = level.clamp(1., max);
    if level < SNAP_TO_OFF {
        1.
    } else {
        level
    }
}

/// The level for a pinch: it follows the fingers (libinput's cumulative pinch scale) directly.
/// Spreading the fingers zooms in, as content stretched with them.
pub fn pinch_level(start_level: f64, pinch_scale: f64, max: f64) -> f64 {
    clamp_level(start_level * pinch_scale.max(0.), max)
}

/// Top-left corner of the zoomed view, in output-local logical coordinates.
///
/// `focus` (the pointer) is clamped to the output first, so an output the pointer has left keeps
/// showing the side where it left.
pub fn view_origin(
    focus: Point<f64, Logical>,
    output_size: Size<f64, Logical>,
    level: f64,
) -> Point<f64, Logical> {
    let level = level.max(1.);
    let x = focus.x.clamp(0., output_size.w.max(0.));
    let y = focus.y.clamp(0., output_size.h.max(0.));
    let k = 1. - 1. / level;
    Point::from((x * k, y * k))
}

/// Where a point on the zoomed screen shows, in output-local (unzoomed) coordinates.
pub fn screen_to_content(
    screen: Point<f64, Logical>,
    origin: Point<f64, Logical>,
    level: f64,
) -> Point<f64, Logical> {
    origin + screen.downscale(level.max(1.))
}

/// Where an output-local (unzoomed) point shows on the zoomed screen.
pub fn content_to_screen(
    content: Point<f64, Logical>,
    origin: Point<f64, Logical>,
    level: f64,
) -> Point<f64, Logical> {
    (content - origin).upscale(level.max(1.))
}

/// How much the cursor grows: with the zoom, up to `max`.
pub fn cursor_scale(level: f64, max: f64) -> f64 {
    level.min(max).max(1.)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size() -> Size<f64, Logical> {
        Size::new(1920., 1080.)
    }

    fn p(x: f64, y: f64) -> Point<f64, Logical> {
        Point::from((x, y))
    }

    fn close(a: Point<f64, Logical>, b: Point<f64, Logical>) -> bool {
        (a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9
    }

    #[test]
    fn level_is_clamped_and_snaps_to_off() {
        assert_eq!(clamp_level(0.5, 16.), 1.);
        assert_eq!(clamp_level(1.01, 16.), 1.);
        assert_eq!(clamp_level(1.019, 16.), 1.);
        assert_eq!(clamp_level(1.02, 16.), 1.02);
        assert_eq!(clamp_level(2.5, 16.), 2.5);
        assert_eq!(clamp_level(40., 16.), 16.);
        assert_eq!(clamp_level(40., 4.), 4.);
        assert_eq!(clamp_level(f64::NAN, 16.), 1.);
        assert_eq!(clamp_level(f64::INFINITY, 16.), 16.);
        // A max below 1 means no zoom.
        assert_eq!(clamp_level(3., 0.5), 1.);
    }

    #[test]
    fn pinch_follows_the_fingers() {
        // Spread to twice the distance: twice the zoom.
        assert_eq!(pinch_level(1., 2., 16.), 2.);
        assert_eq!(pinch_level(4., 1.5, 16.), 6.);
        // Pinch closed: zoom out, and back to exactly off.
        assert_eq!(pinch_level(4., 0.5, 16.), 2.);
        assert_eq!(pinch_level(4., 0.252, 16.), 1.);
        assert_eq!(pinch_level(4., 0.1, 16.), 1.);
        // No further than the maximum.
        assert_eq!(pinch_level(8., 3., 16.), 16.);
        // A small spread from off doesn't leave a level just above 1.
        assert_eq!(pinch_level(1., 1.01, 16.), 1.);
        assert_eq!(pinch_level(1., -1., 16.), 1.);
    }

    #[test]
    fn view_follows_the_pointer_proportionally() {
        // Corners and edges show the output's own corners and edges.
        for level in [1., 1.5, 2., 4., 16.] {
            let w = size().w;
            let h = size().h;
            let visible = size().downscale(level);

            assert!(close(view_origin(p(0., 0.), size(), level), p(0., 0.)));
            let o = view_origin(p(w, h), size(), level);
            assert!(close(o + p(visible.w, visible.h), p(w, h)));
            let o = view_origin(p(0., h / 2.), size(), level);
            assert!((o.x).abs() < 1e-9);

            // The view never leaves the output.
            for focus in [p(-500., 30.), p(5000., 5000.), p(w / 3., h / 7.)] {
                let o = view_origin(focus, size(), level);
                assert!(o.x >= 0. && o.y >= 0.);
                assert!(o.x + visible.w <= w + 1e-9 && o.y + visible.h <= h + 1e-9);
            }
        }

        // The centre shows the centre.
        let o = view_origin(p(960., 540.), size(), 2.);
        assert!(close(o, p(480., 270.)));

        // Off, the view is the whole output.
        assert!(close(view_origin(p(700., 300.), size(), 1.), p(0., 0.)));
    }

    #[test]
    fn the_point_under_the_pointer_stays_put_while_pinching() {
        let pointer = p(1234.5, 321.25);
        let mut shown = None;
        for level in [1., 1.02, 1.5, 2., 3.3, 8., 16.] {
            let origin = view_origin(pointer, size(), level);
            // The content point drawn under the pointer...
            let under = screen_to_content(pointer, origin, level);
            assert!(close(under, pointer), "level {level}: {under:?}");
            // ...is the same at every level.
            if let Some(shown) = shown {
                assert!(close(under, shown));
            }
            shown = Some(under);
            // And the pointer's content position is drawn at the pointer.
            assert!(close(content_to_screen(pointer, origin, level), pointer));
        }
    }

    #[test]
    fn input_maps_to_what_is_drawn_there() {
        let level = 4.;
        let pointer = p(400., 900.);
        let origin = view_origin(pointer, size(), level);

        // Round trip.
        for screen in [p(0., 0.), p(10., 20.), p(1919., 1079.), pointer] {
            let content = screen_to_content(screen, origin, level);
            assert!(close(content_to_screen(content, origin, level), screen));
        }

        // A touch 40 px right of the pointer lands 10 px right of the pointed content.
        let touch = screen_to_content(pointer + p(40., 0.), origin, level);
        assert!(close(touch, pointer + p(10., 0.)));

        // The screen corners show the view's corners.
        assert!(close(screen_to_content(p(0., 0.), origin, level), origin));
        let far = screen_to_content(p(size().w, size().h), origin, level);
        assert!(close(far, origin + p(size().w / level, size().h / level)));
    }

    #[test]
    fn cursor_grows_with_the_zoom_up_to_its_max() {
        assert_eq!(cursor_scale(1., 3.), 1.);
        assert_eq!(cursor_scale(2., 3.), 2.);
        assert_eq!(cursor_scale(16., 3.), 3.);
        assert_eq!(cursor_scale(16., 1.), 1.);
    }

    #[test]
    fn levels_back_at_one_free_the_buffers() {
        let mut zoom = OutputScreenZoom::default();
        assert!(!zoom.is_zoomed());
        assert!(zoom.set_level(2.));
        assert!(zoom.is_zoomed());
        assert!(!zoom.set_level(2.));
        assert!(zoom.set_level(1.));
        assert!(!zoom.is_zoomed());
        assert!(zoom.offscreen_data.borrow().is_none());
    }
}
