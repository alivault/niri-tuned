use std::sync::{Arc, Mutex};

use niri_config::CornerRadius;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::utils::{Logical, Point, Rectangle, Scale};
use smithay::wayland::compositor::{with_states, SurfaceData};
use wayland_server::protocol::wl_surface::WlSurface;

use crate::animation::{Animation, Clock};
use crate::handlers::background_effect::get_cached_blur_region;
use crate::niri_render_elements;
use crate::render_helpers::blur::BlurOptions;
use crate::render_helpers::damage::ExtraDamage;
use crate::render_helpers::framebuffer_effect::{FramebufferEffect, FramebufferEffectElement};
use crate::render_helpers::xray::{XrayElement, XrayPos};
use crate::render_helpers::RenderCtx;
use crate::utils::region::TransformedRegion;
use crate::utils::surface_geo;

#[derive(Debug)]
pub struct BackgroundEffect {
    nonxray: FramebufferEffect,
    /// Damage when options change.
    damage: ExtraDamage,
    /// Corner radius for clipping.
    ///
    /// Stored here in addition to `RenderParams` to damage when it changes.
    // FIXME: would be good to remove this duplication of radius.
    corner_radius: CornerRadius,
    blur_config: niri_config::Blur,
    options: Options,
    /// Fade of a surface-requested blur region; see [`RegionFade`].
    region_fade: RegionFade,
}

/// A surface's blur region as non-overlapping surface-local rects.
type BlurRects = Arc<Vec<Rectangle<i32, Logical>>>;

/// How a surface's own blur region (ext-background-effect) fades in and out.
///
/// The protocol has no alpha, so a client fading its surface cannot fade the blur behind it.
/// With a fade configured, the effect instead fades in when the surface first requests a
/// region and fades out when it drops it, drawing the last region until the fade-out ends.
/// Changes between two non-empty regions (moving, resizing, swapping) apply at once.
#[derive(Debug, Default)]
struct RegionFade {
    /// Whether the surface requested a non-empty region as of the last update.
    requested: bool,
    /// Last non-empty region, drawn while fading out.
    held: Option<BlurRects>,
    /// Ongoing fade, from 0 (no effect) to 1 (full effect) or back.
    anim: Option<Animation>,
}

/// Clock and animation for fading surface-requested blur regions.
#[derive(Debug, Clone)]
pub struct BlurFade {
    pub clock: Clock,
    pub config: niri_config::Animation,
}

impl RegionFade {
    /// Tracks `region` and returns the region to draw and its alpha.
    fn update(
        &mut self,
        region: Option<BlurRects>,
        fade: Option<&BlurFade>,
    ) -> (Option<BlurRects>, f32) {
        let requested = region.as_ref().is_some_and(|r| !r.is_empty());

        let Some(fade) = fade else {
            *self = Self {
                requested,
                ..Self::default()
            };
            return (region, 1.);
        };

        if requested != self.requested {
            let current = match &self.anim {
                Some(anim) => anim.clamped_value(),
                None if self.requested => 1.,
                None => 0.,
            };
            let target = if requested { 1. } else { 0. };
            self.anim = Some(Animation::new(
                fade.clock.clone(),
                current,
                target,
                0.,
                fade.config,
            ));
            self.requested = requested;
        }

        if requested {
            self.held = region.clone();
        }

        let alpha = self
            .anim
            .as_ref()
            .map_or(1., |anim| anim.clamped_value().clamp(0., 1.)) as f32;

        if self
            .anim
            .as_ref()
            .is_some_and(|anim| anim.is_clamped_done())
        {
            self.anim = None;
            if !requested {
                self.held = None;
            }
        }

        if requested {
            (region, alpha)
        } else if let Some(held) = &self.held {
            (Some(held.clone()), alpha)
        } else {
            (region, 1.)
        }
    }

    /// Whether a fade is running or about to start on the next render.
    fn is_ongoing(&self, region: Option<&BlurRects>) -> bool {
        let requested = region.is_some_and(|r| !r.is_empty());
        self.anim.is_some() || requested != self.requested
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Options {
    pub blur: bool,
    pub xray: bool,
    pub noise: Option<f64>,
    pub saturation: Option<f64>,
}

impl Options {
    fn is_visible(&self) -> bool {
        self.xray
            || self.blur
            || self.noise.is_some_and(|x| x > 0.)
            || self.saturation.is_some_and(|x| x != 1.)
    }
}

/// Render-time parameters.
#[derive(Debug)]
pub struct RenderParams {
    /// Geometry of the background effect.
    pub geometry: Rectangle<f64, Logical>,
    /// Effect subregion, will be clipped to `geometry`.
    ///
    /// `subregion.iter()` should return `geometry`-relative rectangles.
    pub subregion: Option<TransformedRegion>,
    /// Geometry and radius for clipping in the same coordinate space as `geometry`.
    pub clip: Option<(Rectangle<f64, Logical>, CornerRadius)>,
    /// Scale to use for rounding to physical pixels.
    pub scale: f64,
}

impl RenderParams {
    fn fit_clip_radius(&mut self) {
        if let Some((geo, radius)) = &mut self.clip {
            // HACK: increase radius to avoid slight bleed on rounded corners.
            *radius = radius.expanded_by(1.);

            *radius = radius.fit_to(geo.size.w as f32, geo.size.h as f32);
        }
    }
}

niri_render_elements! {
    BackgroundEffectElement => {
        FramebufferEffect = FramebufferEffectElement,
        Xray = XrayElement,
        ExtraDamage = ExtraDamage,
    }
}

impl BackgroundEffect {
    pub fn new() -> Self {
        Self {
            nonxray: FramebufferEffect::new(),
            damage: ExtraDamage::new(),
            corner_radius: CornerRadius::default(),
            blur_config: niri_config::Blur::default(),
            options: Options::default(),
            region_fade: RegionFade::default(),
        }
    }

    /// Damage the background effect, for example when a blur subregion changes.
    pub fn damage(&mut self) {
        self.damage.damage_all();
        self.nonxray.damage();
    }

    pub fn update_config(&mut self, config: niri_config::Blur) {
        if self.blur_config == config {
            return;
        }

        self.blur_config = config;
        self.damage.damage_all();
        self.nonxray.damage();
    }

    pub fn update_render_elements(
        &mut self,
        corner_radius: CornerRadius,
        effect: niri_config::BackgroundEffect,
        has_blur_region: bool,
    ) {
        // If the surface explicitly requests a blur region, default blur to true.
        let blur = if has_blur_region {
            effect.blur != Some(false)
        } else {
            effect.blur == Some(true)
        };

        let mut options = Options {
            blur,
            xray: effect.xray == Some(true),
            noise: effect.noise,
            saturation: effect.saturation,
        };

        // If we have some background effect but xray wasn't explicitly set, default it to true
        // since it's cheaper.
        if options.is_visible() && effect.xray.is_none() {
            options.xray = true;
        }

        if self.options == options && self.corner_radius == corner_radius {
            return;
        }

        self.options = options;
        self.corner_radius = corner_radius;
        self.damage.damage_all();
        self.nonxray.damage();
    }

    pub fn is_visible(&self) -> bool {
        self.options.is_visible()
    }

    pub fn render(
        &self,
        ctx: RenderCtx<GlesRenderer>,
        ns: Option<usize>,
        mut params: RenderParams,
        xray_pos: XrayPos,
        alpha: f32,
        push: &mut dyn FnMut(BackgroundEffectElement),
    ) {
        if !self.is_visible() || alpha <= 0. {
            return;
        }

        if let Some(clip) = &mut params.clip {
            clip.1 = self.corner_radius;
        }
        params.fit_clip_radius();

        let damage = self.damage.render(params.geometry);

        // Use noise/saturation from options, falling back to blur defaults if blurred, and
        // to no effect if not blurred.
        let blur = self.options.blur && !self.blur_config.is_off();
        let blur_options = blur.then_some(BlurOptions::global(self.blur_config, params.scale));
        let noise = if blur { self.blur_config.noise } else { 0. };
        let noise = self.options.noise.unwrap_or(noise) as f32;
        let saturation = if blur {
            self.blur_config.saturation
        } else {
            1.
        };
        let saturation = self.options.saturation.unwrap_or(saturation) as f32;

        if self.options.xray {
            let Some(xray) = ctx.xray else {
                return;
            };

            push(damage.into());
            xray.render(
                ctx,
                params,
                xray_pos,
                blur,
                noise,
                saturation,
                &mut |elem| push(elem.with_alpha(alpha).into()),
            );
        } else {
            // Render non-xray effect.
            let elem = self
                .nonxray
                .render(ns, params, blur_options, noise, saturation)
                .with_alpha(alpha);
            push(elem.into());
        }
    }
}

fn render_params_for_tile(
    geometry: Rectangle<f64, Logical>,
    scale: f64,
    clip_to_geometry: bool,
    block_out: bool,
    blur_region: Option<BlurRects>,
    surface_geo: Rectangle<f64, Logical>,
    surface_anim_scale: Scale<f64>,
) -> Option<RenderParams> {
    // Effects not requested by the surface itself are drawn to match the geometry.
    let mut clip = true;

    let mut effect_geometry = geometry;
    let mut subregion = None;
    if let Some(rects) = blur_region {
        if rects.is_empty() {
            // Surface has a set, but empty blur region.
            return None;
        } else {
            // If the surface itself requests the effects, apply different defaults.
            clip = clip_to_geometry;

            // Use geometry-shaped blur for blocked-out windows to avoid unintentionally
            // leaking any surface shapes. We render those windows as geometry-shaped solid
            // rectangles anyway.
            if block_out {
                clip = true;
            } else {
                let mut surface_geo = surface_geo.upscale(surface_anim_scale);
                surface_geo.loc += geometry.loc;

                subregion = Some(TransformedRegion {
                    rects,
                    scale: surface_anim_scale,
                    offset: surface_geo.loc,
                });

                surface_geo = surface_geo
                    .to_physical_precise_round(scale)
                    .to_logical(scale);
                effect_geometry = surface_geo;
            }
        }
    }

    // This corner radius is reset to self.corner_radius in render().
    let clip = clip.then_some((geometry, CornerRadius::default()));

    Some(RenderParams {
        geometry: effect_geometry,
        subregion,
        clip,
        scale,
    })
}

/// Per-surface background effect stored in its data map.
struct SurfaceBackgroundEffect(Mutex<BackgroundEffect>);

impl SurfaceBackgroundEffect {
    fn get(states: &SurfaceData) -> &Self {
        states
            .data_map
            .get_or_insert(|| SurfaceBackgroundEffect(Mutex::new(BackgroundEffect::new())))
    }
}

pub fn damage_surface(states: &SurfaceData) {
    if let Some(effect) = states.data_map.get::<SurfaceBackgroundEffect>() {
        effect.0.lock().unwrap().damage();
    }
}

/// Whether the surface's blur region is fading, or will start fading on the next render.
pub fn is_fade_ongoing(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        let Some(effect) = states.data_map.get::<SurfaceBackgroundEffect>() else {
            return false;
        };
        let region = get_cached_blur_region(states);
        effect
            .0
            .lock()
            .unwrap()
            .region_fade
            .is_ongoing(region.as_ref())
    })
}

// Silence, Clippy
// A Smithay user is talking
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub fn render_for_tile(
    ctx: RenderCtx<GlesRenderer>,
    ns: Option<usize>,
    geometry: Rectangle<f64, Logical>,
    scale: f64,
    clip_to_geometry: bool,
    surface: &WlSurface,
    surface_off: Point<f64, Logical>,
    surface_anim_scale: Scale<f64>,
    blur_config: niri_config::Blur,
    radius: CornerRadius,
    effect: niri_config::BackgroundEffect,
    should_block_out: bool,
    xray_pos: XrayPos,
    fade: Option<&BlurFade>,
    push: &mut dyn FnMut(BackgroundEffectElement),
) {
    render_for_tile_above(
        ctx,
        ns,
        geometry,
        scale,
        clip_to_geometry,
        surface,
        surface_off,
        surface_anim_scale,
        blur_config,
        radius,
        effect,
        should_block_out,
        xray_pos,
        fade,
        None,
        push,
    );
}

/// [`render_for_tile`], with the effect reaching `above` (height, corner radius of the whole)
/// over the window when the effect is the window rule's rather than the surface's own region:
/// behind the title bar niri draws there.
#[allow(clippy::too_many_arguments)]
pub fn render_for_tile_above(
    ctx: RenderCtx<GlesRenderer>,
    ns: Option<usize>,
    geometry: Rectangle<f64, Logical>,
    scale: f64,
    clip_to_geometry: bool,
    surface: &WlSurface,
    surface_off: Point<f64, Logical>,
    surface_anim_scale: Scale<f64>,
    blur_config: niri_config::Blur,
    radius: CornerRadius,
    effect: niri_config::BackgroundEffect,
    should_block_out: bool,
    mut xray_pos: XrayPos,
    fade: Option<&BlurFade>,
    above: Option<(f64, CornerRadius)>,
    push: &mut dyn FnMut(BackgroundEffectElement),
) {
    with_states(surface, |states| {
        let background_effect = SurfaceBackgroundEffect::get(states);
        let mut background_effect = background_effect.0.lock().unwrap();

        let blur_region = get_cached_blur_region(states);
        let (blur_region, alpha) = background_effect.region_fade.update(blur_region, fade);
        let has_blur_region = blur_region.as_ref().is_some_and(|r| !r.is_empty());

        // A surface's own region stays its own; the rule's effect covers the title bar too.
        let above = above.filter(|_| blur_region.is_none());
        let (geometry, radius) = match above {
            Some((height, radius)) => {
                let mut geometry = geometry;
                geometry.loc.y -= height;
                geometry.size.h += height;
                xray_pos = xray_pos.offset(Point::from((0., -height)));
                (geometry, radius)
            }
            None => (geometry, radius),
        };

        background_effect.update_config(blur_config);
        background_effect.update_render_elements(radius, effect, has_blur_region);

        if !background_effect.is_visible() {
            return;
        }

        let mut surface_geo = surface_geo(states).unwrap_or_default().to_f64();
        surface_geo.loc += surface_off;

        let Some(params) = render_params_for_tile(
            geometry,
            scale,
            clip_to_geometry,
            should_block_out,
            blur_region,
            surface_geo,
            surface_anim_scale,
        ) else {
            return;
        };

        let xray_pos = xray_pos.offset(params.geometry.loc - geometry.loc);
        background_effect.render(ctx, ns, params, xray_pos, alpha, push);
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn region() -> Option<Arc<Vec<Rectangle<i32, Logical>>>> {
        Some(Arc::new(vec![Rectangle::new(
            (10, 10).into(),
            (100, 50).into(),
        )]))
    }

    fn fade(clock: &Clock) -> BlurFade {
        BlurFade {
            clock: clock.clone(),
            config: niri_config::animations::BlurFadeAnim::default().0,
        }
    }

    fn at(clock: &mut Clock, ms: u64) {
        clock.set_unadjusted(Duration::from_millis(ms));
    }

    #[test]
    fn global_blur_radius_change_damages_the_effect() {
        use smithay::backend::renderer::element::Element as _;

        let mut effect = BackgroundEffect::new();
        let mut config = niri_config::Blur::default();
        effect.update_config(config);
        let first = effect.damage.current_commit();

        // Hot reload: a new radius redraws the blur behind windows and layers.
        config.radius = Some(64.);
        effect.update_config(config);
        let second = effect.damage.current_commit();
        assert_ne!(second, first);
        // The same config again: nothing to redraw.
        effect.update_config(config);
        assert_eq!(effect.damage.current_commit(), second);
        config.radius = Some(8.);
        effect.update_config(config);
        assert_ne!(effect.damage.current_commit(), second);
    }

    #[test]
    fn fades_in_when_region_appears() {
        let mut clock = Clock::with_time(Duration::ZERO);
        let fade = fade(&clock);
        let mut state = RegionFade::default();

        let (drawn, alpha) = state.update(region(), Some(&fade));
        assert_eq!(drawn, region());
        assert_eq!(alpha, 0.);
        assert!(state.is_ongoing(region().as_ref()));

        at(&mut clock, 70);
        let (_, alpha) = state.update(region(), Some(&fade));
        assert!(alpha > 0. && alpha < 1., "{alpha}");

        at(&mut clock, 140);
        let (_, alpha) = state.update(region(), Some(&fade));
        assert_eq!(alpha, 1.);
        assert!(!state.is_ongoing(region().as_ref()));
    }

    #[test]
    fn keeps_last_region_while_fading_out() {
        let mut clock = Clock::with_time(Duration::ZERO);
        let fade = fade(&clock);
        let mut state = RegionFade::default();
        state.update(region(), Some(&fade));
        at(&mut clock, 200);
        state.update(region(), Some(&fade));

        // The client drops its region: pending until the next render.
        assert!(state.is_ongoing(None));

        let (drawn, alpha) = state.update(None, Some(&fade));
        assert_eq!(drawn, region());
        assert_eq!(alpha, 1.);

        at(&mut clock, 270);
        let (drawn, alpha) = state.update(None, Some(&fade));
        assert_eq!(drawn, region());
        assert!(alpha > 0. && alpha < 1., "{alpha}");

        // Fade finished: nothing held any more.
        at(&mut clock, 340);
        let (drawn, alpha) = state.update(None, Some(&fade));
        assert_eq!(drawn, None);
        assert_eq!(alpha, 1.);
        assert!(!state.is_ongoing(None));
    }

    #[test]
    fn empty_region_counts_as_dropped() {
        let mut clock = Clock::with_time(Duration::ZERO);
        let fade = fade(&clock);
        let mut state = RegionFade::default();
        state.update(region(), Some(&fade));
        at(&mut clock, 200);
        state.update(region(), Some(&fade));

        let empty = Some(Arc::new(Vec::new()));
        let (drawn, _) = state.update(empty, Some(&fade));
        assert_eq!(drawn, region());
    }

    #[test]
    fn reversing_mid_fade_starts_from_current_alpha() {
        let mut clock = Clock::with_time(Duration::ZERO);
        let fade = fade(&clock);
        let mut state = RegionFade::default();
        state.update(region(), Some(&fade));
        at(&mut clock, 50);
        let (_, alpha_in) = state.update(region(), Some(&fade));

        let (_, alpha_out) = state.update(None, Some(&fade));
        assert!(
            (alpha_out - alpha_in).abs() < 1e-6,
            "{alpha_in} -> {alpha_out}"
        );
    }

    #[test]
    fn region_changes_apply_at_once() {
        let mut clock = Clock::with_time(Duration::ZERO);
        let fade = fade(&clock);
        let mut state = RegionFade::default();
        state.update(region(), Some(&fade));
        at(&mut clock, 200);
        state.update(region(), Some(&fade));

        let moved = Some(Arc::new(vec![Rectangle::new(
            (50, 10).into(),
            (80, 40).into(),
        )]));
        let (drawn, alpha) = state.update(moved.clone(), Some(&fade));
        assert_eq!(drawn, moved);
        assert_eq!(alpha, 1.);
        assert!(!state.is_ongoing(moved.as_ref()));
    }

    #[test]
    fn without_fade_nothing_is_held() {
        let clock = Clock::with_time(Duration::ZERO);
        let fade = fade(&clock);
        let mut state = RegionFade::default();
        state.update(region(), Some(&fade));

        assert_eq!(state.update(region(), None), (region(), 1.));
        assert_eq!(state.update(None, None), (None, 1.));
        assert!(!state.is_ongoing(None));
    }
}
