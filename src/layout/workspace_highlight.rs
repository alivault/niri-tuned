//! The overview's workspace highlight: a fill inside each workspace and a border outside it.
//!
//! Drawn at the workspace's zoomed size, so the border stays `border-width` screen pixels at
//! any overview zoom, and faded with the overview's progress, so it never shows outside the
//! overview (as in a plain workspace switch).

use niri_config::{CornerRadius, WorkspaceHighlight};
use smithay::utils::{Logical, Point, Rectangle};

use super::focus_ring::{FocusRing, FocusRingRenderElement};
use crate::render_helpers::renderer::NiriRenderer;

#[derive(Debug)]
struct Highlight {
    fill: FocusRing,
    border: FocusRing,
    /// Where the workspace is (zoomed, on the output), when it is drawn.
    geo: Option<Rectangle<f64, Logical>>,
}

#[derive(Debug, Default)]
pub struct WorkspaceHighlights {
    highlights: Vec<Highlight>,
}

impl Highlight {
    fn new() -> Self {
        Self {
            fill: FocusRing::new(niri_config::FocusRing::default()),
            border: FocusRing::new(niri_config::FocusRing::default()),
            geo: None,
        }
    }
}

/// The highlight's fill and border as focus rings: the fill a ring of width 0 drawn whole,
/// the border a ring around the outside.
fn ring_configs(
    config: &WorkspaceHighlight,
    border_width: f64,
) -> (niri_config::FocusRing, niri_config::FocusRing) {
    let fill = niri_config::FocusRing {
        off: false,
        width: 0.,
        active_color: config.fill(),
        inactive_color: config.fill(),
        urgent_color: config.fill(),
        active_gradient: None,
        inactive_gradient: None,
        urgent_gradient: None,
    };
    let border = niri_config::FocusRing {
        off: border_width <= 0.,
        width: border_width,
        active_color: config.active_border(),
        inactive_color: config.inactive_border(),
        urgent_color: config.active_border(),
        active_gradient: None,
        inactive_gradient: None,
        urgent_gradient: None,
    };
    (fill, border)
}

impl WorkspaceHighlights {
    /// `geos` are the workspaces' zoomed geometries, `None` for one not drawn; `alpha` the
    /// overview's progress (`None`: the overview is closed, nothing is drawn).
    pub fn update_render_elements(
        &mut self,
        config: &WorkspaceHighlight,
        geos: impl Iterator<Item = Option<Rectangle<f64, Logical>>>,
        active_idx: usize,
        scale: f64,
        alpha: Option<f64>,
    ) {
        let alpha = match alpha {
            Some(alpha) if !config.off => alpha.clamp(0., 1.) as f32,
            _ => {
                for h in &mut self.highlights {
                    h.geo = None;
                }
                return;
            }
        };

        // A whole number of physical pixels, at least one if any.
        let border_width = if config.border_width > 0. {
            (config.border_width * scale).round().max(1.) / scale
        } else {
            0.
        };
        let (fill_config, border_config) = ring_configs(config, border_width);

        let mut count = 0;
        for (idx, geo) in geos.enumerate() {
            if self.highlights.len() <= idx {
                self.highlights.push(Highlight::new());
            }
            let h = &mut self.highlights[idx];
            count = idx + 1;

            h.geo = geo;
            let Some(geo) = geo else {
                continue;
            };

            let is_active = idx == active_idx;
            let view_rect = Rectangle::from_size(geo.size);

            h.fill.update_config(fill_config);
            h.fill.update_render_elements(
                geo.size,
                is_active,
                false,
                false,
                view_rect,
                CornerRadius::default(),
                scale,
                alpha,
            );

            h.border.update_config(border_config);
            h.border.update_render_elements(
                geo.size,
                is_active,
                true,
                false,
                view_rect,
                CornerRadius::default(),
                scale,
                alpha,
            );
        }
        self.highlights.truncate(count);
    }

    /// Each drawn workspace's highlight, at its location on the output.
    pub fn render<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        push: &mut dyn FnMut(Point<f64, Logical>, FocusRingRenderElement),
    ) {
        for h in &self.highlights {
            let Some(geo) = h.geo else {
                continue;
            };
            h.border
                .render(renderer, Point::from((0., 0.)), &mut |elem| {
                    push(geo.loc, elem)
                });
            h.fill.render(renderer, Point::from((0., 0.)), &mut |elem| {
                push(geo.loc, elem)
            });
        }
    }

    pub fn update_shaders(&mut self) {
        for h in &mut self.highlights {
            h.fill.update_shaders();
            h.border.update_shaders();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> WorkspaceHighlight {
        WorkspaceHighlight {
            off: false,
            ..Default::default()
        }
    }

    fn geos(n: usize) -> impl Iterator<Item = Option<Rectangle<f64, Logical>>> {
        (0..n).map(|i| {
            Some(Rectangle::new(
                Point::from((100., 100. + 600. * i as f64)),
                (960., 540.).into(),
            ))
        })
    }

    fn drawn(h: &WorkspaceHighlights) -> usize {
        h.highlights.iter().filter(|h| h.geo.is_some()).count()
    }

    #[test]
    fn drawn_only_while_the_overview_is_open() {
        let mut h = WorkspaceHighlights::default();
        h.update_render_elements(&config(), geos(3), 0, 2., Some(0.5));
        assert_eq!(drawn(&h), 3);

        // Closed (a workspace switch outside the overview): nothing.
        h.update_render_elements(&config(), geos(3), 0, 2., None);
        assert_eq!(drawn(&h), 0);

        // Off: nothing, even in the overview.
        let off = WorkspaceHighlight::default();
        assert!(off.off);
        h.update_render_elements(&off, geos(3), 0, 2., Some(1.));
        assert_eq!(drawn(&h), 0);
    }

    #[test]
    fn follows_the_workspace_count() {
        let mut h = WorkspaceHighlights::default();
        h.update_render_elements(&config(), geos(4), 1, 1., Some(1.));
        assert_eq!(h.highlights.len(), 4);
        h.update_render_elements(&config(), geos(2), 1, 1., Some(1.));
        assert_eq!(h.highlights.len(), 2);

        // An undrawn (off-screen) workspace keeps its place but draws nothing.
        let some = [None, geos(1).next().unwrap()];
        h.update_render_elements(&config(), some.into_iter(), 1, 1., Some(1.));
        assert_eq!(drawn(&h), 1);
        assert!(h.highlights[0].geo.is_none());
    }

    #[test]
    fn border_is_whole_physical_pixels_outside_the_workspace() {
        let mut h = WorkspaceHighlights::default();
        let mut c = config();
        c.border_width = 0.3;
        h.update_render_elements(&c, geos(1), 0, 2., Some(1.));
        assert_eq!(h.highlights[0].border.width(), 0.5);

        c.border_width = 0.;
        h.update_render_elements(&c, geos(1), 0, 2., Some(1.));
        assert!(h.highlights[0].border.is_off());
    }
}
