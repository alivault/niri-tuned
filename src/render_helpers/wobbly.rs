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

use super::blur::BlurOptions;
use super::framebuffer_effect;
use super::offscreen::OffscreenRenderElement;
use super::renderer::AsGlesFrame as _;
use super::shaders::Shaders;
use super::xray::{XrayLayer, XrayPos};
use crate::animation::wobbly::evaluate;
use crate::backend::tty::{TtyFrame, TtyRenderer, TtyRendererError};

/// Longest mesh cell side, in physical pixels: short enough that a bent edge shows no corners
/// (the curve leaves a 24 px chord by well under a pixel).
const CELL_PX: f64 = 24.;
/// Fewest and most cells along a side (the most keeps vertex indices in 16 bits).
const MIN_CELLS: usize = 8;
const MAX_CELLS: usize = 128;
const _: () = assert!((MAX_CELLS + 1) * (MAX_CELLS + 1) <= u16::MAX as usize + 1);
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
    /// The mesh's vertices, `STRIDE` floats each, rows first.
    vertices: Rc<[f32]>,
    /// Its triangles, three vertex indices each.
    indices: Rc<[u16]>,
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
        let tex_scale = texture_scale(picture, scale)?;
        let grid = Grid::new(picture, scale, location, tile_size, points)?;
        let geometry = grid.geometry;

        let mut vertices = Vec::with_capacity(grid.vertices.len() * STRIDE);
        for v in &grid.vertices {
            let p = grid.normalized(v.pos);
            let t = v.s * tex_scale;
            vertices.extend_from_slice(&[p.x as f32, p.y as f32, t.x as f32, t.y as f32]);
        }
        let indices = grid.indices();

        Some(Self {
            id,
            commit,
            program,
            texture,
            renderer_context_id: renderer.context_id(),
            geometry,
            vertices: vertices.into(),
            indices: indices.into(),
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

        let projection = Mat3::from_cols_array(frame.projection());
        let matrix = mesh_matrix(projection, src, dst, self.geometry);

        let instancing = frame.capabilities().contains(&Capability::Instancing);
        let program = self.program.0.clone();
        let tex_id = self.texture.tex_id();
        let alpha = self.alpha;
        let vertices = self.vertices.clone();
        let indices = self.indices.clone();
        let count = indices.len() as i32;
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
            gl.BindBuffer(ffi::ELEMENT_ARRAY_BUFFER, 0);
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
                gl.DrawElements(
                    ffi::TRIANGLES,
                    count,
                    ffi::UNSIGNED_SHORT,
                    indices.as_ptr().cast(),
                );
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

/// The picture's share of its texture (the texture can be larger, reused).
fn texture_scale(picture: &OffscreenRenderElement, scale: Scale<f64>) -> Option<DVec2> {
    let tex_size = picture.texture().size();
    let src = picture.logical_size().to_physical(scale);
    if src.w <= 0. || src.h <= 0. || tex_size.w <= 0 || tex_size.h <= 0 {
        return None;
    }
    Some(DVec2::new(
        src.w / tex_size.w as f64,
        src.h / tex_size.h as f64,
    ))
}

/// A vertex of the mesh over a picture.
#[derive(Debug, Clone, Copy)]
struct GridVertex {
    /// Where it is drawn, physical, in the element's (not yet wrapped) space.
    pos: DVec2,
    /// Where it is on the picture, from 0 to 1 (a pixel more around it).
    s: DVec2,
    /// Where it is drawn, logical, from the tile's origin: bent.
    bent: DVec2,
}

/// The mesh over a picture of the tile, bent onto the patch.
#[derive(Debug)]
struct Grid {
    /// The box around the bent picture, physical.
    geometry: Rectangle<i32, Physical>,
    cx: usize,
    cy: usize,
    /// Rows first, `(cx + 1) * (cy + 1)`.
    vertices: Vec<GridVertex>,
}

impl Grid {
    fn new(
        picture: &OffscreenRenderElement,
        scale: Scale<f64>,
        location: Point<f64, Logical>,
        tile_size: Size<f64, Logical>,
        points: &[DVec2; 16],
    ) -> Option<Self> {
        let offset = picture.offset();
        let size = picture.logical_size();
        Self::with(
            DVec2::new(offset.x, offset.y),
            DVec2::new(size.w, size.h),
            scale.x,
            DVec2::new(location.x, location.y),
            DVec2::new(tile_size.w, tile_size.h),
            points,
        )
    }

    /// The mesh over a picture at `origin` (logical, from the tile's origin), `picture_size`
    /// across, for a tile `tile` across at `location`.
    fn with(
        origin: DVec2,
        picture_size: DVec2,
        scale: f64,
        location: DVec2,
        tile: DVec2,
        points: &[DVec2; 16],
    ) -> Option<Self> {
        if picture_size.x <= 0. || picture_size.y <= 0. || tile.x <= 0. || tile.y <= 0. {
            return None;
        }
        let src = picture_size * scale;

        // One physical pixel more around the picture: the texture's transparent border, for an
        // antialiased edge where the window has no shadow.
        let pad = DVec2::new(1. / src.x, 1. / src.y);

        let cells = |px: f64| ((px / CELL_PX).ceil() as usize).clamp(MIN_CELLS, MAX_CELLS);
        let (cx, cy) = (cells(src.x), cells(src.y));
        let mut vertices = Vec::with_capacity((cx + 1) * (cy + 1));
        let (mut lo, mut hi) = (DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY));
        for j in 0..=cy {
            for i in 0..=cx {
                let s = DVec2::new(i as f64 / cx as f64, j as f64 / cy as f64);
                let s = -pad + s * (DVec2::ONE + 2. * pad);
                let on_tile = (origin + s * picture_size) / tile;
                let bent = evaluate(points, on_tile.x, on_tile.y);
                let pos = (location + bent) * scale;
                if !pos.is_finite() {
                    return None;
                }
                lo = lo.min(pos);
                hi = hi.max(pos);
                vertices.push(GridVertex { pos, s, bent });
            }
        }

        let geometry = Rectangle::<i32, Physical>::from_extremities(
            (lo.x.floor() as i32, lo.y.floor() as i32),
            (hi.x.ceil() as i32, hi.y.ceil() as i32),
        );
        if geometry.is_empty() {
            return None;
        }

        Some(Self {
            geometry,
            cx,
            cy,
            vertices,
        })
    }

    /// `pos` from 0 to 1 across the geometry.
    fn normalized(&self, pos: DVec2) -> DVec2 {
        let loc = DVec2::new(self.geometry.loc.x as f64, self.geometry.loc.y as f64);
        let size = DVec2::new(self.geometry.size.w as f64, self.geometry.size.h as f64);
        (pos - loc) / size
    }

    /// The triangles, three vertex indices each.
    fn indices(&self) -> Vec<u16> {
        let nx = self.cx + 1;
        let mut indices = Vec::with_capacity(self.cx * self.cy * 6);
        for j in 0..self.cy {
            for i in 0..self.cx {
                for (di, dj) in [(0, 0), (1, 0), (0, 1), (0, 1), (1, 0), (1, 1)] {
                    indices.push(((j + dj) * nx + i + di) as u16);
                }
            }
        }
        indices
    }
}

/// Where a point of the picture (`s`, 0 to 1 across it) is in the mask's texture: both are
/// drawn from the tile's origin, `picture` and `mask` are their (origin, size), logical, and
/// `mask_tex_scale` the mask's share of its texture.
///
/// The mask follows the picture, so the effect's shape bends with the window.
fn mask_coords(
    s: DVec2,
    picture: (DVec2, DVec2),
    mask: (DVec2, DVec2),
    mask_tex_scale: DVec2,
) -> DVec2 {
    let on_tile = picture.0 + s * picture.1;
    (on_tile - mask.0) / mask.1 * mask_tex_scale
}

/// Where a vertex drawn at `bent` (logical, from the tile's origin) is in the xray backdrop,
/// with the tile's origin at `pos_in_backdrop` and `zoom` (as in [`XrayPos`]).
///
/// The vertex's own place on screen, not the picture's: what the backdrop shows does not bend.
fn backdrop_coords(xray_pos: XrayPos, bent: DVec2) -> DVec2 {
    let pos = DVec2::new(xray_pos.pos_in_backdrop.x, xray_pos.pos_in_backdrop.y);
    (pos + bent) * xray_pos.zoom
}

/// Where a fragment at `frag` (framebuffer pixels) reads the framebuffer copied from
/// `captured`: what the shader does in live mode.
#[cfg(test)]
fn live_coords(frag: DVec2, captured: Rectangle<i32, Physical>) -> DVec2 {
    let loc = DVec2::new(captured.loc.x as f64, captured.loc.y as f64);
    let size = DVec2::new(captured.size.w as f64, captured.size.h as f64);
    (frag - loc) / size
}

#[derive(Debug, Clone)]
pub struct WobblyBackdropProgram(Rc<WobblyBackdropProgramInner>);

#[derive(Debug)]
struct WobblyBackdropProgramInner {
    program: ffi::types::GLuint,
    uniform_matrix: ffi::types::GLint,
    uniform_mask: ffi::types::GLint,
    uniform_tex: ffi::types::GLint,
    uniform_alpha: ffi::types::GLint,
    uniform_live: ffi::types::GLint,
    uniform_uv_rect: ffi::types::GLint,
    uniform_clip: ffi::types::GLint,
    uniform_noise: ffi::types::GLint,
    uniform_saturation: ffi::types::GLint,
    uniform_bg_color: ffi::types::GLint,
    attrib_position: ffi::types::GLint,
    attrib_mask_coords: ffi::types::GLint,
    attrib_backdrop_coords: ffi::types::GLint,
}

impl WobblyBackdropProgram {
    pub fn compile(renderer: &mut GlesRenderer) -> anyhow::Result<Self> {
        let program = renderer.with_context(|gl| unsafe {
            let program = link_program(
                gl,
                include_str!("shaders/wobbly_backdrop.vert"),
                concat!(
                    include_str!("shaders/wobbly_backdrop.frag"),
                    include_str!("shaders/postprocess.frag"),
                ),
            )?;
            let uniform = |name: &std::ffi::CStr| gl.GetUniformLocation(program, name.as_ptr());
            let attrib = |name: &std::ffi::CStr| gl.GetAttribLocation(program, name.as_ptr());
            Ok::<_, GlesError>(WobblyBackdropProgramInner {
                program,
                uniform_matrix: uniform(c"matrix"),
                uniform_mask: uniform(c"mask"),
                uniform_tex: uniform(c"tex"),
                uniform_alpha: uniform(c"alpha"),
                uniform_live: uniform(c"live"),
                uniform_uv_rect: uniform(c"uv_rect"),
                uniform_clip: uniform(c"clip"),
                uniform_noise: uniform(c"noise"),
                uniform_saturation: uniform(c"saturation"),
                uniform_bg_color: uniform(c"bg_color"),
                attrib_position: attrib(c"position"),
                attrib_mask_coords: attrib(c"mask_coords"),
                attrib_backdrop_coords: attrib(c"backdrop_coords"),
            })
        })??;
        Ok(Self(Rc::new(program)))
    }
}

/// What shows through a wobbly window's mask.
#[derive(Debug, Clone)]
pub enum BackdropSource {
    /// What is below the window, copied from the framebuffer as it is drawn (blurred with these
    /// options, if any).
    Live(Option<BlurOptions>),
    /// The xray buffers, bottom first (blurred if `blur`).
    Xray { layers: Vec<XrayLayer>, blur: bool },
}

/// The background effect behind a wobbly window: drawn through the window's mesh, so its shape
/// (the mask, drawn like the window's picture) bends with the window, while what shows through it
/// is read in screen space and stays put, as with a CSS backdrop filter.
///
/// Live, it is a framebuffer effect: Smithay copies what is below it first.
#[derive(Debug, Clone)]
pub struct WobblyBackdropElement {
    id: Id,
    commit: CommitCounter,
    program: WobblyBackdropProgram,
    mask: GlesTexture,
    renderer_context_id: ContextId<GlesTexture>,
    geometry: Rectangle<i32, Physical>,
    /// `BACKDROP_STRIDE` floats each, rows first.
    vertices: Rc<[f32]>,
    indices: Rc<[u16]>,
    source: BackdropSource,
    noise: f32,
    saturation: f32,
    alpha: f32,
}

/// Floats per backdrop vertex: position, mask coordinates, backdrop coordinates.
const BACKDROP_STRIDE: usize = 6;

impl WobblyBackdropElement {
    /// Whether it can be drawn: its shader and the mask's white texture.
    pub fn has_shader(renderer: &mut GlesRenderer) -> bool {
        let shaders = Shaders::get(renderer);
        shaders.wobbly.is_some() && shaders.wobbly_backdrop.is_some() && shaders.white.is_some()
    }

    /// The effect `mask` (drawn from the tile's origin, as `picture`) stands for, under the
    /// patch that bends `picture` (see [`WobblyRenderElement::new`]); `xray_pos` is the tile
    /// origin's.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        renderer: &mut GlesRenderer,
        id: Id,
        commit: CommitCounter,
        picture: &OffscreenRenderElement,
        mask: &OffscreenRenderElement,
        scale: Scale<f64>,
        location: Point<f64, Logical>,
        tile_size: Size<f64, Logical>,
        points: &[DVec2; 16],
        xray_pos: XrayPos,
        source: BackdropSource,
        noise: f32,
        saturation: f32,
        alpha: f32,
    ) -> Option<Self> {
        let program = Shaders::get(renderer).wobbly_backdrop.clone()?;
        if let BackdropSource::Xray { layers, .. } = &source {
            if layers.is_empty() {
                return None;
            }
        }

        let mask_tex_scale = texture_scale(mask, scale)?;
        let grid = Grid::new(picture, scale, location, tile_size, points)?;

        let p_off = picture.offset();
        let p_size = picture.logical_size();
        let m_off = mask.offset();
        let m_size = mask.logical_size();
        let picture_rect = (DVec2::new(p_off.x, p_off.y), DVec2::new(p_size.w, p_size.h));
        let mask_rect = (DVec2::new(m_off.x, m_off.y), DVec2::new(m_size.w, m_size.h));

        let mut vertices = Vec::with_capacity(grid.vertices.len() * BACKDROP_STRIDE);
        for v in &grid.vertices {
            let p = grid.normalized(v.pos);
            let m = mask_coords(v.s, picture_rect, mask_rect, mask_tex_scale);
            let b = backdrop_coords(xray_pos, v.bent);
            vertices.extend_from_slice(&[
                p.x as f32, p.y as f32, m.x as f32, m.y as f32, b.x as f32, b.y as f32,
            ]);
        }

        Some(Self {
            id,
            commit,
            program,
            mask: mask.texture().clone(),
            renderer_context_id: renderer.context_id(),
            geometry: grid.geometry,
            vertices: vertices.into(),
            indices: grid.indices().into(),
            source,
            noise,
            saturation,
            alpha,
        })
    }

    fn draw_backdrop(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        if frame.context_id() != self.renderer_context_id {
            warn!("trying to render texture from different renderer");
            return Ok(());
        }
        if src.size.w <= 0. || src.size.h <= 0. || damage.is_empty() {
            return Ok(());
        }

        // Bottom first.
        let mut passes: Vec<Pass> = Vec::new();
        match &self.source {
            BackdropSource::Live(_) => {
                let Some((texture, rect)) = cache.and_then(framebuffer_effect::captured) else {
                    return Ok(());
                };
                let rect = [
                    rect.loc.x as f32,
                    rect.loc.y as f32,
                    rect.size.w as f32,
                    rect.size.h as f32,
                ];
                passes.push(Pass {
                    texture,
                    live: true,
                    rect,
                    clip: false,
                    bg_color: [0.; 4],
                });
            }
            BackdropSource::Xray { layers, blur } => {
                for layer in layers {
                    let texture = match layer.buffer.borrow_mut().render(frame, *blur) {
                        Ok(texture) => texture,
                        Err(err) => {
                            warn!("error rendering effect buffer: {err:?}");
                            continue;
                        }
                    };
                    let r = layer.rect;
                    let rect = [
                        r.loc.x as f32,
                        r.loc.y as f32,
                        r.size.w as f32,
                        r.size.h as f32,
                    ];
                    passes.push(Pass {
                        texture,
                        live: false,
                        rect,
                        clip: layer.clip,
                        bg_color: layer.bg_color.components(),
                    });
                }
            }
        }
        if passes.is_empty() {
            return Ok(());
        }

        let projection = Mat3::from_cols_array(frame.projection());
        let matrix = mesh_matrix(projection, src, dst, self.geometry);

        let instancing = frame.capabilities().contains(&Capability::Instancing);
        let program = self.program.0.clone();
        let mask_id = self.mask.tex_id();
        let (noise, saturation, alpha) = (self.noise, self.saturation, self.alpha);
        let vertices = self.vertices.clone();
        let indices = self.indices.clone();
        let count = indices.len() as i32;
        let damage: Vec<_> = damage
            .iter()
            .filter_map(|rect| rect.intersection(Rectangle::from_size(dst.size)))
            .map(|rect| (rect.loc + dst.loc, rect.size))
            .collect();

        frame.with_context(move |gl| unsafe {
            let mut viewport = [0; 4];
            gl.GetIntegerv(ffi::VIEWPORT, viewport.as_mut_ptr());

            gl.UseProgram(program.program);
            gl.UniformMatrix3fv(
                program.uniform_matrix,
                1,
                ffi::FALSE,
                matrix.as_ref().as_ptr(),
            );
            gl.Uniform1i(program.uniform_mask, 0);
            gl.Uniform1i(program.uniform_tex, 1);
            gl.Uniform1f(program.uniform_alpha, alpha);
            gl.Uniform1f(program.uniform_noise, noise);
            gl.Uniform1f(program.uniform_saturation, saturation);

            gl.ActiveTexture(ffi::TEXTURE0);
            gl.BindTexture(ffi::TEXTURE_2D, mask_id);
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

            let stride = (BACKDROP_STRIDE * std::mem::size_of::<f32>()) as i32;
            let attribs = [
                (program.attrib_position as u32, 0),
                (program.attrib_mask_coords as u32, 2),
                (program.attrib_backdrop_coords as u32, 4),
            ];
            gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
            gl.BindBuffer(ffi::ELEMENT_ARRAY_BUFFER, 0);
            for (attrib, offset) in attribs {
                gl.EnableVertexAttribArray(attrib);
                gl.VertexAttribPointer(
                    attrib,
                    2,
                    ffi::FLOAT,
                    ffi::FALSE,
                    stride,
                    vertices.as_ptr().add(offset).cast(),
                );
                if instancing {
                    // Smithay's instanced quads leave divisors on these indices.
                    gl.VertexAttribDivisor(attrib, 0);
                }
            }

            gl.ActiveTexture(ffi::TEXTURE1);
            for Pass {
                texture,
                live,
                rect,
                clip,
                bg_color,
            } in &passes
            {
                gl.BindTexture(ffi::TEXTURE_2D, texture.tex_id());
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
                gl.TexParameteri(
                    ffi::TEXTURE_2D,
                    ffi::TEXTURE_WRAP_S,
                    ffi::CLAMP_TO_EDGE as i32,
                );
                gl.TexParameteri(
                    ffi::TEXTURE_2D,
                    ffi::TEXTURE_WRAP_T,
                    ffi::CLAMP_TO_EDGE as i32,
                );
                gl.Uniform1f(program.uniform_live, if *live { 1. } else { 0. });
                gl.Uniform4f(program.uniform_uv_rect, rect[0], rect[1], rect[2], rect[3]);
                gl.Uniform1f(program.uniform_clip, if *clip { 1. } else { 0. });
                gl.Uniform4f(
                    program.uniform_bg_color,
                    bg_color[0],
                    bg_color[1],
                    bg_color[2],
                    bg_color[3],
                );

                // Only inside the damage: what is outside it is not redrawn this frame.
                for (loc, size) in &damage {
                    let (x, y, w, h) = scissor_box(projection, *loc, *size, viewport);
                    if w <= 0 || h <= 0 {
                        continue;
                    }
                    gl.Scissor(x, y, w, h);
                    gl.DrawElements(
                        ffi::TRIANGLES,
                        count,
                        ffi::UNSIGNED_SHORT,
                        indices.as_ptr().cast(),
                    );
                }
            }
            gl.Scissor(viewport[0], viewport[1], viewport[2], viewport[3]);

            for (attrib, _) in attribs {
                gl.DisableVertexAttribArray(attrib);
            }
            gl.BindTexture(ffi::TEXTURE_2D, 0);
            gl.ActiveTexture(ffi::TEXTURE0);
            gl.BindTexture(ffi::TEXTURE_2D, 0);
        })
    }
}

/// One draw of the backdrop's mesh: what it reads and how.
#[derive(Debug)]
struct Pass {
    texture: GlesTexture,
    /// Read at `gl_FragCoord` (a framebuffer copy), not at backdrop coordinates (xray).
    live: bool,
    /// Where `texture` is: x, y, width, height.
    rect: [f32; 4],
    /// Nothing outside `rect`.
    clip: bool,
    bg_color: [f32; 4],
}

/// The matrix that puts a mesh (0 to 1 across `geometry`) where wrappers that move, scale and
/// crop the element put it: `src` is the part of its geometry drawn to `dst`.
fn mesh_matrix(
    projection: Mat3,
    src: Rectangle<f64, Buffer>,
    dst: Rectangle<i32, Physical>,
    geometry: Rectangle<i32, Physical>,
) -> Mat3 {
    let k = Vec2::new(
        (dst.size.w as f64 / src.size.w) as f32,
        (dst.size.h as f64 / src.size.h) as f32,
    );
    let full_loc = Vec2::new(dst.loc.x as f32, dst.loc.y as f32)
        - Vec2::new(src.loc.x as f32, src.loc.y as f32) * k;
    let full_size = Vec2::new(geometry.size.w as f32, geometry.size.h as f32) * k;
    projection * Mat3::from_translation(full_loc) * Mat3::from_scale(full_size)
}

impl Element for WobblyBackdropElement {
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

    fn is_framebuffer_effect(&self) -> bool {
        matches!(self.source, BackdropSource::Live(_))
    }
}

impl RenderElement<GlesRenderer> for WobblyBackdropElement {
    fn capture_framebuffer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), GlesError> {
        let _span = tracy_client::span!("WobblyBackdropElement::capture_framebuffer");
        let BackdropSource::Live(blur) = self.source else {
            return Ok(());
        };
        // `src` is physical already: its scale is 1.
        framebuffer_effect::capture(frame, src, dst, cache, 1., blur)
    }

    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let _span = tracy_client::span!("WobblyBackdropElement::draw");
        self.draw_backdrop(frame, src, dst, damage, cache)
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

impl<'render> RenderElement<TtyRenderer<'render>> for WobblyBackdropElement {
    fn capture_framebuffer(
        &self,
        frame: &mut TtyFrame<'_, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), TtyRendererError<'render>> {
        RenderElement::<GlesRenderer>::capture_framebuffer(
            self,
            frame.as_gles_frame(),
            src,
            dst,
            cache,
        )?;
        Ok(())
    }

    fn draw(
        &self,
        frame: &mut TtyFrame<'_, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        let _span = tracy_client::span!("WobblyBackdropElement::draw");
        self.draw_backdrop(frame.as_gles_frame(), src, dst, damage, cache)?;
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

    fn rest(size: DVec2) -> [DVec2; 16] {
        std::array::from_fn(|i| DVec2::new((i % 4) as f64, (i / 4) as f64) / 3. * size)
    }

    #[test]
    fn mask_coords_follow_the_picture() {
        // The picture has a 10 px shadow around a 100 × 60 tile; the mask is the tile only,
        // in the first 50 × 30 texels of a texture twice that.
        let picture = (DVec2::new(-10., -10.), DVec2::new(120., 80.));
        let mask = (DVec2::ZERO, DVec2::new(100., 60.));
        let tex_scale = DVec2::splat(0.5);

        let at = |on_tile: DVec2| (on_tile - picture.0) / picture.1;
        let m = |on_tile| mask_coords(at(on_tile), picture, mask, tex_scale);
        assert!(m(DVec2::ZERO).abs_diff_eq(DVec2::ZERO, 1e-12));
        assert!(m(DVec2::new(100., 60.)).abs_diff_eq(DVec2::splat(0.5), 1e-12));
        assert!(m(DVec2::new(50., 30.)).abs_diff_eq(DVec2::splat(0.25), 1e-12));
    }

    #[test]
    fn backdrop_coords_are_the_vertex_on_screen() {
        // The tile's origin at (200, 100) in an overview zoomed to 0.5.
        let xray_pos = XrayPos::new(Point::from((200., 100.)), 0.5);
        assert_eq!(
            backdrop_coords(xray_pos, DVec2::ZERO),
            DVec2::new(200., 100.)
        );
        assert_eq!(
            backdrop_coords(xray_pos, DVec2::new(100., -20.)),
            DVec2::new(250., 90.)
        );
    }

    #[test]
    fn live_coords_span_the_copy() {
        let captured = Rectangle::new(Point::new(30, 40), Size::new(200, 100));
        assert_eq!(live_coords(DVec2::new(30., 40.), captured), DVec2::ZERO);
        assert_eq!(live_coords(DVec2::new(230., 140.), captured), DVec2::ONE);
        assert_eq!(
            live_coords(DVec2::new(130., 90.), captured),
            DVec2::splat(0.5)
        );
    }

    #[test]
    fn the_shape_bends_and_the_backdrop_does_not() {
        let tile = DVec2::new(400., 300.);
        let (scale, location) = (2., DVec2::new(50., 20.));
        let xray_pos = XrayPos::new(Point::from((50., 20.)), 1.);
        let picture = (DVec2::new(-8., -8.), tile + 16.);
        let mask = (DVec2::ZERO, tile);

        let still = Grid::with(picture.0, picture.1, scale, location, tile, &rest(tile)).unwrap();
        let mut bent_points = rest(tile);
        bent_points[0] += DVec2::new(-40., 25.);
        bent_points[15] += DVec2::new(30., -10.);
        let bent = Grid::with(picture.0, picture.1, scale, location, tile, &bent_points).unwrap();
        assert_eq!(still.vertices.len(), bent.vertices.len());

        let mut moved = 0;
        for (a, b) in still.vertices.iter().zip(&bent.vertices) {
            // The same point of the shape, wherever the patch puts it.
            let ma = mask_coords(a.s, picture, mask, DVec2::ONE);
            let mb = mask_coords(b.s, picture, mask, DVec2::ONE);
            assert!(ma.abs_diff_eq(mb, 1e-9));

            // What shows there is what is on screen under the vertex: its backdrop
            // coordinates are its own place, in logical pixels.
            for v in [a, b] {
                let screen = v.pos / scale;
                assert!(backdrop_coords(xray_pos, v.bent).abs_diff_eq(screen, 1e-9));
            }
            if !a.pos.abs_diff_eq(b.pos, 1e-6) {
                moved += 1;
            }
        }
        assert!(moved > 0);

        // At rest, the mask is where the tile is.
        let corner = &still.vertices[0];
        let m = mask_coords(corner.s, picture, mask, DVec2::ONE);
        let expected = (picture.0 + corner.s * picture.1) / tile;
        assert!(m.abs_diff_eq(expected, 1e-12));
    }
}
