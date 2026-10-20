//! A live window surface drawn in perspective (the recent-windows Flip 3D).
//!
//! Wraps the surface's own `WaylandSurfaceRenderElement`, keeping its id and
//! commit counter, so the surface stays on the frame: it keeps getting frame
//! callbacks and presentation feedback, and its commits damage it. There is no
//! offscreen copy; the shader samples the client's buffer directly.

use glam::{Mat3, Vec2, Vec3};
use niri_config::CornerRadius;
use smithay::backend::renderer::buffer_y_inverted;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, WaylandSurfaceTexture,
};
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform,
};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};

use super::renderer::{AsGlesFrame as _, NiriRenderer};
use super::shaders::{mat3_uniform, Shaders};
use crate::backend::tty::{TtyFrame, TtyRenderer, TtyRendererError};

/// A 3×3 matrix acting on homogeneous 2D points, rows first.
pub type Homography = [[f64; 3]; 3];

#[derive(Debug)]
pub struct ProjectedSurfaceRenderElement<R: NiriRenderer> {
    inner: WaylandSurfaceRenderElement<R>,
    program: GlesTexProgram,
    /// Output-physical coordinates to window-logical ones.
    output_to_window: Homography,
    /// Where the element is drawn: the box around its projected quad, on the
    /// output, physical.
    bbox: Rectangle<i32, Physical>,
    /// The surface, in window-logical coordinates.
    surface: Rectangle<f64, Logical>,
    geometry: Size<f64, Logical>,
    corner_radius: CornerRadius,
}

impl<R: NiriRenderer> ProjectedSurfaceRenderElement<R> {
    /// `elem` was rendered with the window at the origin, at `scale`;
    /// `window_to_output` maps window-logical coordinates to output-physical
    /// ones and `output_to_window` back. `geometry` is the window's size, its
    /// corners rounded by `corner_radius`; `clip` is the output, physical.
    ///
    /// None when the surface falls outside the output.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        elem: WaylandSurfaceRenderElement<R>,
        scale: Scale<f64>,
        window_to_output: Homography,
        output_to_window: Homography,
        geometry: Size<f64, Logical>,
        corner_radius: CornerRadius,
        clip: Rectangle<i32, Physical>,
        program: GlesTexProgram,
    ) -> Option<Self> {
        // A single-pixel buffer is drawn as a solid rectangle, without a
        // texture program to project it; leave it out rather than draw it flat.
        if matches!(elem.texture(), WaylandSurfaceTexture::SolidColor(_)) {
            return None;
        }
        let phys = elem.geometry(scale).to_f64();
        let surface = Rectangle::new(
            Point::new(phys.loc.x / scale.x, phys.loc.y / scale.y),
            Size::new(phys.size.w / scale.x, phys.size.h / scale.y),
        );
        // What of the surface shows: the window geometry clips it.
        let shown = surface.intersection(Rectangle::from_size(geometry))?;
        let (x0, y0) = (shown.loc.x, shown.loc.y);
        let (x1, y1) = (x0 + shown.size.w, y0 + shown.size.h);
        let (mut bx0, mut by0) = (f64::INFINITY, f64::INFINITY);
        let (mut bx1, mut by1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for (x, y) in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)] {
            let p = apply(window_to_output, x, y);
            if !p.0.is_finite() || !p.1.is_finite() {
                return None;
            }
            bx0 = bx0.min(p.0);
            by0 = by0.min(p.1);
            bx1 = bx1.max(p.0);
            by1 = by1.max(p.1);
        }
        // One pixel more, for the antialiased edge.
        let bbox = Rectangle::<i32, Physical>::from_extremities(
            (bx0.floor() as i32 - 1, by0.floor() as i32 - 1),
            (bx1.ceil() as i32 + 1, by1.ceil() as i32 + 1),
        );
        let bbox = bbox.intersection(clip)?;
        if bbox.is_empty() {
            return None;
        }

        Some(Self {
            inner: elem,
            program,
            output_to_window,
            bbox,
            surface,
            geometry,
            corner_radius,
        })
    }

    pub fn shader(renderer: &mut R) -> Option<&GlesTexProgram> {
        Shaders::get(renderer).projected_surface.as_ref()
    }

    fn compute_uniforms(&self) -> Vec<Uniform<'static>> {
        let buf_size = self.inner.buffer_size();
        let buf_size = Vec2::new(buf_size.w as f32, buf_size.h as f32);
        let view = self.inner.view();
        let src_loc = Vec2::new(view.src.loc.x as f32, view.src.loc.y as f32);
        let src_size = Vec2::new(view.src.size.w as f32, view.src.size.h as f32);

        // Texture coordinates to the destination, as ClippedSurfaceRenderElement
        // maps them onto its geometry when that is the destination.
        let transform = self.inner.transform();
        let transform = match transform {
            Transform::_90 => Transform::_270,
            Transform::_270 => Transform::_90,
            x => x,
        };
        let transform_matrix = Mat3::from_translation(Vec2::new(0.5, 0.5))
            * Mat3::from_cols_array(transform.matrix().as_ref())
            * Mat3::from_translation(-Vec2::new(0.5, 0.5));
        let y_invert = if buffer_y_inverted(self.inner.buffer()).unwrap_or(false) {
            Mat3::from_scale(Vec2::new(1., -1.))
        } else {
            Mat3::IDENTITY
        };
        let input_to_box = transform_matrix
            * Mat3::from_scale(buf_size / src_size)
            * Mat3::from_translation(-src_loc / buf_size)
            * y_invert;
        let surface_to_input = input_to_box.inverse();

        // The box (0..1) to the output, then to the window.
        let b = self.bbox.to_f64();
        let box_to_output = [
            [b.size.w, 0., b.loc.x],
            [0., b.size.h, b.loc.y],
            [0., 0., 1.],
        ];
        let box_to_window = to_glam(mul(self.output_to_window, box_to_output));

        // Texels across the surface along its own axes (transformed buffers
        // swap them).
        let src = self.inner.src().size;
        let size = self.inner.transform().transform_size(src);
        let texels = (size.w as f32, size.h as f32);

        // The surface's own edges cut it, except where it reaches the window
        // geometry's edge: there the geometry's antialiased edge does.
        let s = self.surface;
        let g = self.geometry;
        let far = 1e6_f32;
        let cut = [
            if s.loc.x <= 0. { -far } else { 0. },
            if s.loc.y <= 0. { -far } else { 0. },
            if s.loc.x + s.size.w >= g.w { far } else { 1. },
            if s.loc.y + s.size.h >= g.h { far } else { 1. },
        ];

        vec![
            mat3_uniform("input_to_box", input_to_box),
            mat3_uniform("box_to_window", box_to_window),
            Uniform::new("box_pixel", (1. / b.size.w as f32, 1. / b.size.h as f32)),
            Uniform::new(
                "surface_rect",
                [
                    s.loc.x as f32,
                    s.loc.y as f32,
                    s.size.w as f32,
                    s.size.h as f32,
                ],
            ),
            mat3_uniform("surface_to_input", surface_to_input),
            Uniform::new("surface_texels", texels),
            Uniform::new("surface_cut", cut),
            Uniform::new("geo_size", (g.w as f32, g.h as f32)),
            Uniform::new("corner_radius", <[f32; 4]>::from(self.corner_radius)),
        ]
    }
}

fn apply(m: Homography, x: f64, y: f64) -> (f64, f64) {
    let w = m[2][0] * x + m[2][1] * y + m[2][2];
    (
        (m[0][0] * x + m[0][1] * y + m[0][2]) / w,
        (m[1][0] * x + m[1][1] * y + m[1][2]) / w,
    )
}

fn mul(a: Homography, b: Homography) -> Homography {
    let mut m = [[0.; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

fn to_glam(m: Homography) -> Mat3 {
    let col = |j: usize| Vec3::new(m[0][j] as f32, m[1][j] as f32, m[2][j] as f32);
    Mat3::from_cols(col(0), col(1), col(2))
}

impl<R: NiriRenderer> Element for ProjectedSurfaceRenderElement<R> {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.bbox
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        // Any change to the surface moves pixels anywhere in the projection.
        // (A change of the projection itself moves or reshapes the box, or is
        // damaged by the caller.)
        let damage = self.inner.damage_since(scale, commit);
        if damage.is_empty() {
            damage
        } else {
            DamageSet::from_slice(&[Rectangle::from_size(self.bbox.size)])
        }
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        // Edges and filtering blend; nothing is reliably opaque.
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for ProjectedSurfaceRenderElement<GlesRenderer> {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        frame.override_default_tex_program(self.program.clone(), self.compute_uniforms());
        let result = RenderElement::<GlesRenderer>::draw(
            &self.inner,
            frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        );
        frame.clear_tex_program_override();
        result
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        // Never scanned out: the buffer is not what is on screen.
        None
    }
}

impl<'render> RenderElement<TtyRenderer<'render>>
    for ProjectedSurfaceRenderElement<TtyRenderer<'render>>
{
    fn draw(
        &self,
        frame: &mut TtyFrame<'render, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        frame
            .as_gles_frame()
            .override_default_tex_program(self.program.clone(), self.compute_uniforms());
        let result =
            RenderElement::draw(&self.inner, frame, src, dst, damage, opaque_regions, cache);
        frame.as_gles_frame().clear_tex_program_override();
        result
    }

    fn underlying_storage(
        &self,
        _renderer: &mut TtyRenderer<'render>,
    ) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrices_compose_and_convert_column_major() {
        let a = [[2., 0., 3.], [0., 4., 5.], [0., 0., 1.]];
        let b = [[1., 0., 10.], [0., 1., 20.], [0., 0., 1.]];
        let m = mul(a, b);
        assert_eq!(apply(m, 1., 1.), (2. * 11. + 3., 4. * 21. + 5.));
        let g = to_glam(m);
        let p = g * Vec3::new(1., 1., 1.);
        assert_eq!((p.x as f64, p.y as f64), (25., 89.));
        // Projective rows divide.
        let p = [[1., 0., 0.], [0., 1., 0.], [0.5, 0., 1.]];
        assert_eq!(apply(p, 2., 4.), (1., 2.));
    }
}
