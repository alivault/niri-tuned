use std::cmp::max;
use std::iter::{once, zip};
use std::rc::Rc;

use anyhow::{ensure, Context as _};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{ffi, link_program, GlesError, GlesRenderer, GlesTexture};
use smithay::backend::renderer::{ContextId, Renderer as _, Texture as _};
use smithay::gpu_span_location;
use smithay::utils::{Buffer, Size};

use crate::render_helpers::shaders::Shaders;

#[derive(Debug)]
pub struct Blur {
    program: BlurProgram,
    /// Context ID of the renderer that created the program and the textures.
    renderer_context_id: ContextId<GlesTexture>,
    /// Output texture followed by intermediate textures, large to small.
    ///
    /// Created lazily and stored here to avoid recreating blur textures frequently.
    textures: Vec<GlesTexture>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct BlurOptions {
    pub passes: u8,
    pub offset: f64,
}

impl BlurOptions {
    /// The global blur on an output of this scale: its `radius` (logical pixels) if set,
    /// else its `passes` and `offset` (physical, whatever the scale, as upstream).
    pub fn global(config: niri_config::Blur, scale: f64) -> Self {
        match config.radius {
            Some(radius) => static_blur(radius * scale),
            None => Self {
                passes: config.passes,
                offset: config.offset,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct BlurProgram(Rc<BlurProgramInner>);

#[derive(Debug)]
struct BlurProgramInner {
    down: BlurProgramInternal,
    up: BlurProgramInternal,
}

#[derive(Debug)]
struct BlurProgramInternal {
    program: ffi::types::GLuint,
    uniform_tex: ffi::types::GLint,
    uniform_half_pixel: ffi::types::GLint,
    uniform_offset: ffi::types::GLint,
    attrib_vert: ffi::types::GLint,
}

unsafe fn compile_program(gl: &ffi::Gles2, src: &str) -> Result<BlurProgramInternal, GlesError> {
    let program = unsafe { link_program(gl, include_str!("shaders/blur.vert"), src)? };

    let vert = c"vert";
    let tex = c"tex";
    let half_pixel = c"half_pixel";
    let offset = c"offset";

    Ok(BlurProgramInternal {
        program,
        uniform_tex: gl.GetUniformLocation(program, tex.as_ptr()),
        uniform_half_pixel: gl.GetUniformLocation(program, half_pixel.as_ptr()),
        uniform_offset: gl.GetUniformLocation(program, offset.as_ptr()),
        attrib_vert: gl.GetAttribLocation(program, vert.as_ptr()),
    })
}

impl BlurProgram {
    pub fn compile(renderer: &mut GlesRenderer) -> anyhow::Result<Self> {
        renderer
            .with_context(move |gl| unsafe {
                let down = compile_program(gl, include_str!("shaders/blur_down.frag"))
                    .context("error compiling blur_down shader")?;
                let up = compile_program(gl, include_str!("shaders/blur_up.frag"))
                    .context("error compiling blur_up shader")?;
                Ok(Self(Rc::new(BlurProgramInner { down, up })))
            })
            .context("error making GL context current")?
    }

    pub fn destroy(self, renderer: &mut GlesRenderer) -> Result<(), GlesError> {
        renderer.with_context(move |gl| unsafe {
            gl.DeleteProgram(self.0.down.program);
            gl.DeleteProgram(self.0.up.program);
        })
    }
}

impl Blur {
    pub fn new(renderer: &mut GlesRenderer) -> Option<Self> {
        let program = Shaders::get(renderer).blur.clone()?;
        Some(Self {
            program,
            renderer_context_id: renderer.context_id(),
            textures: Vec::new(),
        })
    }

    pub fn context_id(&self) -> ContextId<GlesTexture> {
        self.renderer_context_id.clone()
    }

    pub fn prepare_textures(
        &mut self,
        mut create_texture: impl FnMut(Fourcc, Size<i32, Buffer>) -> Result<GlesTexture, GlesError>,
        source: &GlesTexture,
        options: BlurOptions,
    ) -> anyhow::Result<()> {
        let _span = tracy_client::span!("Blur::prepare_textures");

        let passes = options.passes.clamp(1, 31) as usize;
        let size = source.size();

        if let Some(output) = self.textures.first_mut() {
            let old_size = output.size();
            if old_size != size {
                trace!(
                    "recreating textures: output size changed from {} × {} to {} × {}",
                    old_size.w,
                    old_size.h,
                    size.w,
                    size.h
                );
                self.textures.clear();
            } else if !output.is_unique_reference() {
                debug!("recreating textures: not unique",);
                // We only need to recreate the output texture here, but this case shouldn't really
                // happen anyway, and this is simpler.
                self.textures.clear();
            }
        }

        // Create any missing textures.
        let mut w = size.w;
        let mut h = size.h;
        for i in 0..=passes {
            let size = Size::new(w, h);
            w = max(1, w / 2);
            h = max(1, h / 2);

            if self.textures.len() > i {
                // This texture already exists.
                continue;
            }

            // debug!("creating texture for step {i} sized {w} × {h}");

            let texture: GlesTexture =
                create_texture(Fourcc::Abgr8888, size).context("error creating texture")?;
            self.textures.push(texture);
        }

        // Drop any no longer needed textures.
        self.textures.drain(passes + 1..);

        Ok(())
    }

    pub fn render(
        &mut self,
        renderer: &mut GlesRenderer,
        source: &GlesTexture,
        options: BlurOptions,
    ) -> anyhow::Result<GlesTexture> {
        let _span = tracy_client::span!("Blur::render");
        trace!("rendering blur");

        ensure!(
            renderer.context_id() == self.renderer_context_id,
            "wrong renderer"
        );

        let passes = options.passes.clamp(1, 31) as usize;
        let size = source.size();

        ensure!(
            self.textures.len() == passes + 1,
            "wrong textures len: expected {}, got {}",
            passes + 1,
            self.textures.len()
        );

        let output = &mut self.textures[0];
        ensure!(
            output.size() == size,
            "wrong output texture size: expected {size:?}, got {:?}",
            output.size()
        );

        ensure!(
            output.is_unique_reference(),
            "output texture has a non-unique reference"
        );

        renderer.with_profiled_context(gpu_span_location!("Blur::render"), |gl| unsafe {
            while gl.GetError() != ffi::NO_ERROR {}

            gl.Disable(ffi::BLEND);
            gl.Disable(ffi::SCISSOR_TEST);

            gl.ActiveTexture(ffi::TEXTURE0);

            let mut fbos = [0; 2];
            gl.GenFramebuffers(fbos.len() as _, fbos.as_mut_ptr());
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbos[0]);

            let program = &self.program.0.down;
            gl.UseProgram(program.program);
            gl.Uniform1i(program.uniform_tex, 0);
            gl.Uniform1f(program.uniform_offset, options.offset as f32);

            let vertices: [f32; 12] = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
            gl.EnableVertexAttribArray(program.attrib_vert as u32);
            gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
            gl.VertexAttribPointer(
                program.attrib_vert as u32,
                2,
                ffi::FLOAT,
                ffi::FALSE,
                0,
                vertices.as_ptr().cast(),
            );

            let src = once(source).chain(&self.textures[1..]);
            let dst = &self.textures[1..];
            for (src, dst) in zip(src, dst) {
                let dst_size = dst.size();
                let w = dst_size.w;
                let h = dst_size.h;
                gl.Viewport(0, 0, w, h);

                // During downsampling, half_pixel is half of the destination pixel.
                gl.Uniform2f(program.uniform_half_pixel, 0.5 / w as f32, 0.5 / h as f32);

                let src = src.tex_id();
                let dst = dst.tex_id();

                trace!("drawing down {src} to {dst}");
                gl.FramebufferTexture2D(
                    ffi::DRAW_FRAMEBUFFER,
                    ffi::COLOR_ATTACHMENT0,
                    ffi::TEXTURE_2D,
                    dst,
                    0,
                );

                gl.BindTexture(ffi::TEXTURE_2D, src);
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

                gl.DrawArrays(ffi::TRIANGLES, 0, 6);
            }

            gl.DisableVertexAttribArray(program.attrib_vert as u32);

            // Up
            let program = &self.program.0.up;
            gl.UseProgram(program.program);
            gl.Uniform1i(program.uniform_tex, 0);
            gl.Uniform1f(program.uniform_offset, options.offset as f32);

            let vertices: [f32; 12] = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
            gl.EnableVertexAttribArray(program.attrib_vert as u32);
            gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
            gl.VertexAttribPointer(
                program.attrib_vert as u32,
                2,
                ffi::FLOAT,
                ffi::FALSE,
                0,
                vertices.as_ptr().cast(),
            );

            let src = self.textures.iter().rev();
            let dst = self.textures.iter().rev().skip(1);
            for (src, dst) in zip(src, dst) {
                let dst_size = dst.size();
                let w = dst_size.w;
                let h = dst_size.h;
                gl.Viewport(0, 0, w, h);

                // During upsampling, half_pixel is half of the source pixel.
                let src_size = src.size();
                let src_w = src_size.w as f32;
                let src_h = src_size.h as f32;
                gl.Uniform2f(program.uniform_half_pixel, 0.5 / src_w, 0.5 / src_h);

                let src = src.tex_id();
                let dst = dst.tex_id();

                trace!("drawing up {src} to {dst}");
                gl.FramebufferTexture2D(
                    ffi::DRAW_FRAMEBUFFER,
                    ffi::COLOR_ATTACHMENT0,
                    ffi::TEXTURE_2D,
                    dst,
                    0,
                );

                gl.BindTexture(ffi::TEXTURE_2D, src);
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

                gl.DrawArrays(ffi::TRIANGLES, 0, 6);
            }

            gl.DisableVertexAttribArray(program.attrib_vert as u32);

            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, 0);
            gl.DeleteFramebuffers(fbos.len() as _, fbos.as_ptr());
        })?;

        Ok(self.textures[0].clone())
    }
}

/// The smallest Kawase offset for a pass count; the largest is twice it, where one more pass
/// at the smallest offset blurs as far.
const RAMP_MIN_OFFSET: f64 = 1.5;
const RAMP_MAX_PASSES: u8 = 8;

/// A blur that grows continuously to `radius` (physical pixels, roughly how far it spreads):
/// the options to blur with and the alpha to draw the blur at.
///
/// Dual Kawase spreads about `offset × 2^passes`. The pass count is the largest whose
/// smallest offset doesn't overshoot, and the offset makes up the rest, so the spread
/// never jumps where a pass is added. Below the smallest blur one pass can make (it
/// halves the resolution), that blur fades in over the sharp image instead. `None`: no
/// blur at all.
pub fn ramped_blur(radius: f64) -> Option<(BlurOptions, f32)> {
    if !(radius > 0.) {
        return None;
    }

    let smallest = RAMP_MIN_OFFSET * 2.;
    if radius < smallest {
        let options = BlurOptions {
            passes: 1,
            offset: RAMP_MIN_OFFSET,
        };
        return Some((options, (radius / smallest) as f32));
    }

    let passes = (radius / RAMP_MIN_OFFSET)
        .log2()
        .floor()
        .clamp(1., f64::from(RAMP_MAX_PASSES));
    let offset = radius / passes.exp2();
    let options = BlurOptions {
        passes: passes as u8,
        offset,
    };
    Some((options, 1.))
}

/// The blur for `radius` (physical pixels) where it can't fade in (windows, layers, the
/// cached xray background): as [`ramped_blur`], but a radius below the smallest blur one
/// pass makes is that smallest blur (a radius of 0 is no blur: [`niri_config::Blur::is_off`]).
pub fn static_blur(radius: f64) -> BlurOptions {
    let smallest = RAMP_MIN_OFFSET * 2.;
    let radius = if radius >= smallest { radius } else { smallest };
    ramped_blur(radius).map_or(
        BlurOptions {
            passes: 1,
            offset: RAMP_MIN_OFFSET,
        },
        |(options, _)| options,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spread(options: BlurOptions) -> f64 {
        options.offset * f64::from(options.passes).exp2()
    }

    #[test]
    fn ramped_blur_starts_sharp() {
        assert!(ramped_blur(0.).is_none());
        assert!(ramped_blur(-1.).is_none());
        assert!(ramped_blur(f64::NAN).is_none());

        let (options, alpha) = ramped_blur(0.3).unwrap();
        assert_eq!(options.passes, 1);
        assert!(alpha > 0. && alpha < 0.2);
    }

    #[test]
    fn ramped_blur_grows_without_jumps() {
        let mut last_spread = 0.;
        let mut last_alpha = 0.;
        let mut radius = 0.01;
        while radius < 400. {
            let (options, alpha) = ramped_blur(radius).unwrap();
            assert!(alpha >= last_alpha, "alpha fell at {radius}");
            if alpha == 1. {
                let s = spread(options);
                assert!((s - radius).abs() < 1e-6, "spread {s} for radius {radius}");
                assert!(s >= last_spread);
                assert!(
                    options.passes == RAMP_MAX_PASSES
                        || (RAMP_MIN_OFFSET - 1e-9..2. * RAMP_MIN_OFFSET + 1e-9)
                            .contains(&options.offset),
                    "offset {} out of range at {radius}",
                    options.offset
                );
                last_spread = s;
            }
            last_alpha = alpha;
            radius *= 1.01;
        }
    }

    #[test]
    fn ramped_blur_adds_a_pass_at_the_same_spread() {
        let edge = RAMP_MIN_OFFSET * 8.;
        let (below, _) = ramped_blur(edge - 1e-9).unwrap();
        let (at, _) = ramped_blur(edge).unwrap();
        assert_eq!(below.passes + 1, at.passes);
        assert!((spread(below) - spread(at)).abs() < 1e-6);
    }

    #[test]
    fn global_blur_radius_replaces_passes_and_offset() {
        let config = niri_config::Blur::default();
        let defaults = BlurOptions {
            passes: 3,
            offset: 3.,
        };
        // Unset: passes and offset as they are, at any scale.
        assert_eq!(BlurOptions::global(config, 1.), defaults);
        assert_eq!(BlurOptions::global(config, 2.), defaults);

        // Logical pixels: twice the spread at scale 2.
        let at = |radius, scale| {
            BlurOptions::global(
                niri_config::Blur {
                    radius: Some(radius),
                    ..config
                },
                scale,
            )
        };
        assert!((spread(at(40., 1.)) - 40.).abs() < 1e-6);
        assert!((spread(at(40., 2.)) - 80.).abs() < 1e-6);
        assert!((spread(at(12., 2.)) - 24.).abs() < 1e-6);
        for radius in [6., 8., 64., 96., 1024.] {
            assert_eq!(
                at(radius, 1.),
                ramped_blur(radius).unwrap().0,
                "radius {radius}"
            );
        }

        // Too faint to blur at full strength: the smallest blur, never none (0 is `off`).
        let smallest = BlurOptions {
            passes: 1,
            offset: RAMP_MIN_OFFSET,
        };
        for radius in [0.5, 1., 1.5] {
            assert_eq!(at(radius, 2.), smallest, "radius {radius}");
        }
        assert!(at(2., 2.).passes >= 1);
    }

    #[test]
    fn a_global_blur_radius_of_0_is_no_blur() {
        let mut config = niri_config::Blur::default();
        assert!(!config.is_off());
        config.radius = Some(2.);
        assert!(!config.is_off());
        config.radius = Some(0.);
        assert!(config.is_off());
        config.radius = None;
        config.off = true;
        assert!(config.is_off());
    }
}
