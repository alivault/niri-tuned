//! The overview's backdrop blur: one framebuffer effect over the whole output, between the
//! backdrop and the workspaces, whose strength follows the overview's progress.

use std::cell::RefCell;

use niri_config::OverviewBackdropBlur;
use smithay::utils::{Logical, Rectangle};

use crate::render_helpers::background_effect::RenderParams;
use crate::render_helpers::blur::{ramped_blur, BlurOptions};
use crate::render_helpers::framebuffer_effect::{FramebufferEffect, FramebufferEffectElement};

/// What the blur looks like: the element is damaged only when this changes, so a still,
/// fully open overview keeps its blurred backdrop.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Key {
    options: BlurOptions,
    alpha: f32,
    noise: f32,
    saturation: f32,
    geometry: Rectangle<f64, Logical>,
}

#[derive(Debug)]
pub struct OverviewBlur {
    inner: RefCell<(FramebufferEffect, Option<Key>)>,
}

/// The blur at this progress (0 closed, 1 open): its options, alpha, noise and saturation.
/// Noise and saturation grow with it too, so nothing pops in when it starts.
fn at_progress(
    config: &OverviewBackdropBlur,
    progress: f64,
    scale: f64,
) -> Option<(BlurOptions, f32, f32, f32)> {
    if config.off {
        return None;
    }
    let progress = progress.clamp(0., 1.);
    let (options, alpha) = ramped_blur(config.radius * scale * progress)?;
    let noise = (config.noise * progress) as f32;
    let saturation = (1. + (config.saturation - 1.) * progress) as f32;
    Some((options, alpha, noise, saturation))
}

impl OverviewBlur {
    pub fn new() -> Self {
        Self {
            inner: RefCell::new((FramebufferEffect::new(), None)),
        }
    }

    /// The blur over `geometry` (the output) at the overview's `progress`; `None` while the
    /// overview is closed or the blur is off or too faint to see.
    pub fn render(
        &self,
        config: &OverviewBackdropBlur,
        progress: Option<f64>,
        geometry: Rectangle<f64, Logical>,
        scale: f64,
    ) -> Option<FramebufferEffectElement> {
        let (options, alpha, noise, saturation) = at_progress(config, progress?, scale)?;

        let key = Key {
            options,
            alpha,
            noise,
            saturation,
            geometry,
        };
        let mut inner = self.inner.borrow_mut();
        let (effect, last) = &mut *inner;
        if *last != Some(key) {
            *last = Some(key);
            effect.damage();
        }

        let elem = effect.render(
            None,
            RenderParams {
                geometry,
                subregion: None,
                clip: None,
                scale,
            },
            Some(options),
            noise,
            saturation,
        );
        Some(elem.with_alpha(alpha))
    }
}

impl Default for OverviewBlur {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> OverviewBackdropBlur {
        OverviewBackdropBlur {
            off: false,
            ..Default::default()
        }
    }

    #[test]
    fn off_or_closed_draws_nothing() {
        let blur = OverviewBlur::new();
        let geo = Rectangle::from_size((1280., 800.).into());
        assert!(blur
            .render(&OverviewBackdropBlur::default(), Some(1.), geo, 2.)
            .is_none());
        assert!(blur.render(&on(), None, geo, 2.).is_none());
        assert!(blur.render(&on(), Some(0.), geo, 2.).is_none());
        assert!(blur.render(&on(), Some(0.5), geo, 2.).is_some());
    }

    #[test]
    fn grows_with_progress() {
        let config = on();
        let mut last = (0., 0., 1.);
        for i in 1..=100 {
            let p = f64::from(i) / 100.;
            let (options, alpha, _noise, saturation) = at_progress(&config, p, 2.).unwrap();
            let spread = options.offset * f64::from(options.passes).exp2() * f64::from(alpha);
            assert!(spread >= last.0 - 1e-9, "blur shrank at {p}");
            assert!(alpha >= last.1);
            assert!(saturation >= last.2);
            last = (spread, alpha, saturation);
        }
        // Fully open: the configured radius, in physical pixels.
        let (options, alpha, noise, saturation) = at_progress(&config, 1., 2.).unwrap();
        assert_eq!(alpha, 1.);
        assert!((options.offset * f64::from(options.passes).exp2() - 64.).abs() < 1e-6);
        assert_eq!(noise, config.noise as f32);
        assert_eq!(saturation, config.saturation as f32);
    }
}
