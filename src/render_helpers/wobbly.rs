//! A tile's picture bent onto a wobbly window's Bézier patch.
//!
//! The tile (window, border, focus ring, shadow, title bar) is drawn to an offscreen texture as
//! for the alpha animation, then this element draws that texture as a triangle mesh whose
//! vertices lie on the patch. So everything around the window bends with it, its rounded corners
//! too, and nothing needs a shader of its own. The mesh is drawn with a program of its own; the
//! element is only made while a window wobbles.

use std::rc::Rc;

use glam::{DVec2, Mat3, Vec2, Vec3};
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{
    ffi, link_program, Capability, GlesError, GlesFrame, GlesRenderer, GlesTexture,
};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::backend::renderer::{ContextId, Frame as _, Renderer as _, Texture as _};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size};

use super::offscreen::OffscreenRenderElement;
use super::renderer::AsGlesFrame as _;
use super::shaders::Shaders;
use crate::animation::wobbly::evaluate;
use crate::backend::tty::{TtyFrame, TtyRenderer, TtyRendererError};

/// Mesh cells along each side of the picture.
const CELLS: usize = 16;
/// Floats per vertex: position (0 to 1 across the element), texture coordinates.
const STRIDE: usize = 4;

#[derive(Debug, Clone)]
pub struct WobblyProgram(Rc<WobblyProgramInner>);

#[derive(Debug)]
struct WobblyProgramInner {
    program: ffi::types::GLuint,
    uniform_matrix: ffi::types::GLint,
    uniform_tex: ffi::types::GLint,
    uniform_alpha: ffi::types::GLint,
    attrib_position: ffi::types::GLint,
    attrib_tex_coords: ffi::types::GLint,
}

impl WobblyProgram {
    pub fn compile(renderer: &mut GlesRenderer) -> anyhow::Result<Self> {
        let program = renderer.with_context(|gl| unsafe {
            let program = link_program(
                gl,
                include_str!("shaders/wobbly.vert"),
                include_str!("shaders/wobbly.frag"),
            )?;
            Ok::<_, GlesError>(WobblyProgramInner {
                program,
                uniform_matrix: gl.GetUniformLocation(program, c"matrix".as_ptr()),
                uniform_tex: gl.GetUniformLocation(program, c"tex".as_ptr()),
                uniform_alpha: gl.GetUniformLocation(program, c"alpha".as_ptr()),
                attrib_position: gl.GetAttribLocation(program, c"position".as_ptr()),
                attrib_tex_coords: gl.GetAttribLocation(program, c"tex_coords".as_ptr()),
            })
        })??;
        Ok(Self(Rc::new(program)))
    }
}

#[derive(Debug, Clone)]
pub struct WobblyRenderElement {
    id: Id,
    commit: CommitCounter,
    program: WobblyProgram,
    texture: GlesTexture,
    renderer_context_id: ContextId<GlesTexture>,
    /// The box around the bent picture, physical.
    geometry: Rectangle<i32, Physical>,
    /// Triangles, `STRIDE` floats a vertex.
    vertices: Rc<[f32]>,
    alpha: f32,
}

impl WobblyRenderElement {
    pub fn has_shader(renderer: &mut GlesRenderer) -> bool {
        Shaders::get(renderer).wobbly.is_some()
    }

    /// Bends `picture`, the tile drawn with its origin at 0, onto the patch over `points` (the
    /// tile's, `tile_size` across) and puts the tile at `location`.
    ///
    /// `id` and `commit` are the tile's own: the picture changes every frame it is made.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        renderer: &mut GlesRenderer,
        id: Id,
        commit: CommitCounter,
        picture: &OffscreenRenderElement,
        scale: Scale<f64>,
        location: Point<f64, Logical>,
        tile_size: Size<f64, Logical>,
        points: &[DVec2; 16],
        alpha: f32,
    ) -> Option<Self> {
        let program = Shaders::get(renderer).wobbly.clone()?;

        let texture = picture.texture().clone();
        let offset = picture.offset();
        let size = picture.logical_size();
        if size.w <= 0. || size.h <= 0. || tile_size.w <= 0. || tile_size.h <= 0. {
            return None;
        }
        // The picture's share of the texture (it can be larger, reused).
        let tex_size = texture.size();
        let src = picture.logical_size().to_physical(scale);
        let tex_scale = DVec2::new(src.w / tex_size.w as f64, src.h / tex_size.h as f64);

        // One physical pixel more around the picture: the texture's transparent border, for an
        // antialiased edge where the window has no shadow.
        let pad = DVec2::new(1. / src.w, 1. / src.h);
        let tile = DVec2::new(tile_size.w, tile_size.h);
        let origin = DVec2::new(offset.x, offset.y);
        let picture_size = DVec2::new(size.w, size.h);
        let location = DVec2::new(location.x, location.y);

        let n = CELLS + 1;
        let mut grid = Vec::with_capacity(n * n);
        let (mut lo, mut hi) = (DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY));
        for j in 0..n {
            for i in 0..n {
                let s = DVec2::new(i as f64, j as f64) / CELLS as f64;
                let s = -pad + s * (DVec2::ONE + 2. * pad);
                let on_tile = (origin + s * picture_size) / tile;
                let p = (location + evaluate(points, on_tile.x, on_tile.y)) * scale.x;
                if !p.is_finite() {
                    return None;
                }
                lo = lo.min(p);
                hi = hi.max(p);
                grid.push((p, s * tex_scale));
            }
        }

        let geometry = Rectangle::<i32, Physical>::from_extremities(
            (lo.x.floor() as i32, lo.y.floor() as i32),
            (hi.x.ceil() as i32, hi.y.ceil() as i32),
        );
        if geometry.is_empty() {
            return None;
        }
        let geo_loc = DVec2::new(geometry.loc.x as f64, geometry.loc.y as f64);
        let geo_size = DVec2::new(geometry.size.w as f64, geometry.size.h as f64);

        let mut vertices = Vec::with_capacity(CELLS * CELLS * 6 * STRIDE);
        let mut push = |i: usize, j: usize| {
            let (p, t) = grid[j * n + i];
            let p = (p - geo_loc) / geo_size;
            vertices.extend_from_slice(&[p.x as f32, p.y as f32, t.x as f32, t.y as f32]);
        };
        for j in 0..CELLS {
            for i in 0..CELLS {
                for (di, dj) in [(0, 0), (1, 0), (0, 1), (0, 1), (1, 0), (1, 1)] {
                    push(i + di, j + dj);
                }
            }
        }

        Some(Self {
            id,
            commit,
            program,
            texture,
            renderer_context_id: renderer.context_id(),
            geometry,
            vertices: vertices.into(),
            alpha,
        })
    }

    fn draw_mesh(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        if frame.context_id() != self.renderer_context_id {
            warn!("trying to render texture from different renderer");
            return Ok(());
        }
        if src.size.w <= 0. || src.size.h <= 0. || damage.is_empty() {
            return Ok(());
        }

        // Wrappers move, scale and crop the element: `src` is the part of its geometry drawn
        // to `dst`. The whole element then goes to:
        let k = Vec2::new(
            (dst.size.w as f64 / src.size.w) as f32,
            (dst.size.h as f64 / src.size.h) as f32,
        );
        let full_loc = Vec2::new(dst.loc.x as f32, dst.loc.y as f32)
            - Vec2::new(src.loc.x as f32, src.loc.y as f32) * k;
        let full_size = Vec2::new(self.geometry.size.w as f32, self.geometry.size.h as f32) * k;

        let projection = Mat3::from_cols_array(frame.projection());
        let matrix = projection * Mat3::from_translation(full_loc) * Mat3::from_scale(full_size);

        let instancing = frame.capabilities().contains(&Capability::Instancing);
        let program = self.program.0.clone();
        let tex_id = self.texture.tex_id();
        let alpha = self.alpha;
        let vertices = self.vertices.clone();
        let count = (vertices.len() / STRIDE) as i32;
        let damage: Vec<_> = damage
            .iter()
            .filter_map(|rect| rect.intersection(Rectangle::from_size(dst.size)))
            .map(|rect| (projection, rect.loc + dst.loc, rect.size))
            .collect();

        frame.with_context(move |gl| unsafe {
            let mut viewport = [0; 4];
            gl.GetIntegerv(ffi::VIEWPORT, viewport.as_mut_ptr());

            gl.ActiveTexture(ffi::TEXTURE0);
            gl.BindTexture(ffi::TEXTURE_2D, tex_id);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_S,
                ffi::CLAMP_TO_BORDER as i32,
            );
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_T,
                ffi::CLAMP_TO_BORDER as i32,
            );

            gl.UseProgram(program.program);
            gl.Uniform1i(program.uniform_tex, 0);
            gl.UniformMatrix3fv(
                program.uniform_matrix,
                1,
                ffi::FALSE,
                matrix.as_ref().as_ptr(),
            );
            gl.Uniform1f(program.uniform_alpha, alpha);

            let position = program.attrib_position as u32;
            let tex_coords = program.attrib_tex_coords as u32;
            let stride = (STRIDE * std::mem::size_of::<f32>()) as i32;
            gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
            gl.EnableVertexAttribArray(position);
            gl.VertexAttribPointer(
                position,
                2,
                ffi::FLOAT,
                ffi::FALSE,
                stride,
                vertices.as_ptr().cast(),
            );
            gl.EnableVertexAttribArray(tex_coords);
            gl.VertexAttribPointer(
                tex_coords,
                2,
                ffi::FLOAT,
                ffi::FALSE,
                stride,
                vertices.as_ptr().add(2).cast(),
            );
            if instancing {
                // Smithay's instanced quads leave divisors on these indices.
                gl.VertexAttribDivisor(position, 0);
                gl.VertexAttribDivisor(tex_coords, 0);
            }

            // Only inside the damage: what is outside it is not redrawn this frame, and may be
            // a window above this one.
            for (projection, loc, size) in damage {
                let (x, y, w, h) = scissor_box(projection, loc, size, viewport);
                if w <= 0 || h <= 0 {
                    continue;
                }
                gl.Scissor(x, y, w, h);
                gl.DrawArrays(ffi::TRIANGLES, 0, count);
            }
            gl.Scissor(viewport[0], viewport[1], viewport[2], viewport[3]);

            gl.DisableVertexAttribArray(position);
            gl.DisableVertexAttribArray(tex_coords);
            gl.BindTexture(ffi::TEXTURE_2D, 0);
        })
    }
}

/// A rectangle of the frame (physical) in the framebuffer's pixels, for `glScissor`.
fn scissor_box(
    projection: Mat3,
    loc: Point<i32, Physical>,
    size: Size<i32, Physical>,
    viewport: [i32; 4],
) -> (i32, i32, i32, i32) {
    let (x0, y0) = (loc.x as f32, loc.y as f32);
    let (x1, y1) = (x0 + size.w as f32, y0 + size.h as f32);
    let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
    for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
        let ndc = projection * Vec3::new(x, y, 1.);
        let px = Vec2::new(
            viewport[0] as f32 + (ndc.x + 1.) / 2. * viewport[2] as f32,
            viewport[1] as f32 + (ndc.y + 1.) / 2. * viewport[3] as f32,
        );
        lo = lo.min(px);
        hi = hi.max(px);
    }
    let (lx, ly) = (lo.x.round() as i32, lo.y.round() as i32);
    (lx, ly, hi.x.round() as i32 - lx, hi.y.round() as i32 - ly)
}

impl Element for WobblyRenderElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        let size = self.geometry.size;
        Rectangle::from_size(Size::from((size.w as f64, size.h as f64)))
    }

    fn damage_since(
        &self,
        _scale: Scale<f64>,
        _commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        // The mesh moves every frame it is drawn.
        DamageSet::from_slice(&[Rectangle::from_size(self.geometry.size)])
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for WobblyRenderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let _span = tracy_client::span!("WobblyRenderElement::draw");
        self.draw_mesh(frame, src, dst, damage)
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

impl<'render> RenderElement<TtyRenderer<'render>> for WobblyRenderElement {
    fn draw(
        &self,
        frame: &mut TtyFrame<'_, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        let _span = tracy_client::span!("WobblyRenderElement::draw");
        self.draw_mesh(frame.as_gles_frame(), src, dst, damage)?;
        Ok(())
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
    fn scissor_box_flips_to_the_framebuffer() {
        // Smithay's projection for an untransformed 100 × 50 output: y down to GL's y up.
        let projection =
            Mat3::from_cols_array(&[2. / 100., 0., 0., 0., -2. / 50., 0., -1., 1., 1.]);
        let b = scissor_box(
            projection,
            Point::new(10, 5),
            Size::new(20, 10),
            [0, 0, 100, 50],
        );
        assert_eq!(b, (10, 35, 20, 10));
    }
}
