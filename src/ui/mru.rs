use std::cell::{Cell, RefCell};
use std::cmp::min;
use std::collections::HashMap;
use std::mem;
use std::rc::Rc;
use std::time::Duration;

use anyhow::ensure;
use niri_config::{
    Action, Bind, Color, Config, CornerRadius, GradientInterpolation, Key, Modifiers, MruDirection,
    MruFilter, MruLabelText, MruScope, Trigger,
};
use pango::FontDescription;
use pangocairo::cairo::{self, ImageSurface};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::utils::{
    Relocate, RelocateRenderElement, RescaleRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::desktop::layer_map_for_output;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, Scale, Size, Transform};

use crate::animation::{Animation, Clock};
use crate::layout::focus_ring::{FocusRing, FocusRingRenderElement};
use crate::layout::{Layout, LayoutElement as _, LayoutElementRenderElement};
use crate::niri::Niri;
use crate::niri_render_elements;
use crate::render_helpers::background_effect::RenderParams;
use crate::render_helpers::blur::{ramped_blur, BlurOptions};
use crate::render_helpers::border::BorderRenderElement;
use crate::render_helpers::clipped_surface::ClippedSurfaceRenderElement;
use crate::render_helpers::damage::ExtraDamage;
use crate::render_helpers::framebuffer_effect::{FramebufferEffect, FramebufferEffectElement};
use crate::render_helpers::gradient_fade_texture::GradientFadeTextureRenderElement;
use crate::render_helpers::offscreen::{OffscreenBuffer, OffscreenRenderElement};
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::projected_surface::ProjectedSurfaceRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::solid_color::SolidColorRenderElement;
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::render_helpers::RenderCtx;
use crate::utils::{
    baba_is_float_offset, output_size, round_logical_in_physical, to_physical_precise_round,
    with_toplevel_role,
};
use crate::window::mapped::MappedId;
use crate::window::Mapped;

mod flip;
mod icons;
use icons::IconCache;

#[cfg(test)]
mod tests;

/// Space between the Flip 3D stack's scope panel and the free area's corner.
const FLIP_PANEL_MARGIN: f64 = 16.;

/// Space under the Flip 3D stack's name pill.
const FLIP_LABEL_MARGIN: f64 = 32.;

/// Size of the app icon in a window preview's header.
const HEADER_ICON_SIZE: f64 = 24.;

/// Space between the header's app icon and the label beside it.
const HEADER_ICON_GAP: f64 = 8.;

/// Where the labels go, shared by layout, rendering, the container and
/// hit-testing. Lengths are logical and already rounded to physical pixels.
#[derive(Debug, Clone, Copy)]
struct LabelLayout {
    /// Whether there are labels at all (`text` is not `none`).
    shown: bool,
    /// Only the selected item has a label, under its highlight.
    selected_only: bool,
    /// Configured gap between the label and the preview or icon.
    gap: f64,
    /// Height of one line of the label font.
    height: f64,
    /// Window previews: the header above each one (its app icon with the
    /// label beside it, as Windows' Alt-Tab) and the gap under it. 0 for
    /// icons, whose label goes below.
    header: f64,
}

impl LabelLayout {
    fn new(recent: &niri_config::RecentWindows, scale: f64) -> Self {
        let round = |logical: f64| round_logical_in_physical(scale, logical);
        let icons_only = recent.previews.icons_only;
        let gap = round(recent.label.gap);
        let height = round(label_line_height(&recent.label.font));
        Self {
            shown: recent.label.text_for(icons_only) != MruLabelText::None,
            selected_only: recent.label.selected_only(icons_only),
            gap,
            height,
            header: if icons_only {
                0.
            } else {
                round(f64::max(HEADER_ICON_SIZE, height)) + gap
            },
        }
    }

    /// Height of a preview's header row, without the gap under it.
    fn header_row(&self) -> f64 {
        self.header - self.gap
    }

    /// Distance from an icon's bottom edge to its label's top: inside the
    /// highlight for every label, below it for the selected one alone.
    fn top(&self, padding: f64) -> f64 {
        if self.selected_only {
            padding + self.gap
        } else {
            self.gap
        }
    }

    /// How far below a preview or icon its highlight and label reach.
    fn extent(&self, padding: f64) -> f64 {
        if !self.shown || self.header > 0. {
            padding
        } else if self.selected_only {
            f64::max(padding, self.top(padding) + self.height)
        } else {
            // The highlight ends half a padding below the label.
            padding + self.top(padding) + self.height - (padding / 2.).round()
        }
    }
}

thread_local! {
    static LABEL_HEIGHT: RefCell<Option<(String, f64)>> = const { RefCell::new(None) };
}

/// Logical height of a line in `font`, measured once per font, so the layout
/// does not wait for label textures to render.
fn label_line_height(font: &str) -> f64 {
    LABEL_HEIGHT.with_borrow_mut(|cache| {
        if let Some((cached, height)) = cache {
            if cached == font {
                return *height;
            }
        }
        let height = measure_line_height(font).unwrap_or(18.);
        *cache = Some((font.to_owned(), height));
        height
    })
}

fn measure_line_height(font: &str) -> Option<f64> {
    let surface = ImageSurface::create(cairo::Format::ARgb32, 0, 0).ok()?;
    let cr = cairo::Context::new(&surface).ok()?;
    let layout = pangocairo::functions::create_layout(&cr);
    layout.set_font_description(Some(&FontDescription::from_string(font)));
    layout.set_text("Ag");
    let (_, height) = layout.pixel_size();
    (height > 0).then_some(f64::from(height))
}

/// The label text for a window. `app_id` says whether the window has one;
/// `name` is its app name, `None` while still being looked up (then nothing
/// is shown rather than something that changes a moment later).
fn label_text(
    kind: MruLabelText,
    app_id: bool,
    name: Option<String>,
    title: Option<String>,
) -> Option<String> {
    let title = title.filter(|t| !t.trim().is_empty());
    let name = name.filter(|n| !n.trim().is_empty());
    match kind {
        MruLabelText::None => None,
        MruLabelText::Title => title,
        MruLabelText::AppName if app_id => name,
        MruLabelText::AppAndTitle if app_id && name.is_none() => None,
        MruLabelText::AppName => title,
        MruLabelText::AppAndTitle => match (name, title) {
            (Some(name), Some(title)) if !title.trim().eq_ignore_ascii_case(name.trim()) => {
                Some(format!("{name} \u{2014} {title}"))
            }
            (Some(name), _) => Some(name),
            (None, title) => title,
        },
    }
}

/// Highlight padding around a preview: in icons-only mode it shrinks with the
/// icons, so a long row of small icons still fits on screen.
fn padding_for(previews: &niri_config::MruPreviews, padding: f64, preview_w: f64) -> f64 {
    if previews.icons_only {
        padding * f64::min(1., preview_w / f64::max(1., previews.icon_size.round()))
    } else {
        padding
    }
}

/// Width a title may take under its preview: an icon is narrow, so its title
/// may also use the highlight's padding on both sides.
fn title_width_for(previews: &niri_config::MruPreviews, preview_w: f64, padding: f64) -> f64 {
    if previews.icons_only {
        preview_w + 2. * padding
    } else {
        preview_w
    }
}

/// The narrowest a window preview's tile (its header and highlight) is: a
/// small window's preview stays its size, centred in a tile wide enough for
/// its title, never wider than the grid's cells (`cell_w`).
const MIN_TILE_WIDTH: f64 = 180.;

/// A preview's tile: the preview, widened to `MIN_TILE_WIDTH` (at most
/// `cell_w`, the widest preview) about its centre. Icons are their own tile.
fn tile_of(
    preview: Rectangle<f64, Logical>,
    cell_w: f64,
    icons_only: bool,
) -> Rectangle<f64, Logical> {
    if icons_only {
        return preview;
    }
    let w = f64::max(preview.size.w, f64::min(cell_w, MIN_TILE_WIDTH));
    Rectangle::new(
        Point::new(preview.loc.x + (preview.size.w - w) / 2., preview.loc.y),
        Size::new(w, preview.size.h),
    )
}

/// The app icon at the left of a preview's header, centred on its row.
fn header_icon_geometry(
    preview: Rectangle<f64, Logical>,
    labels: &LabelLayout,
    scale: f64,
) -> Rectangle<f64, Logical> {
    let round = |value| round_logical_in_physical(scale, value);
    let top = preview.loc.y - labels.header;
    Rectangle::new(
        Point::new(
            round(preview.loc.x),
            round(top + (labels.header_row() - HEADER_ICON_SIZE) / 2.),
        ),
        Size::new(HEADER_ICON_SIZE, HEADER_ICON_SIZE),
    )
}

/// Where a preview's header label starts: right of its app icon, centred on
/// the row; and the width it may take.
fn header_label_geometry(
    preview: Rectangle<f64, Logical>,
    labels: &LabelLayout,
    label_h: f64,
) -> (Point<f64, Logical>, f64) {
    let x = preview.loc.x + HEADER_ICON_SIZE + HEADER_ICON_GAP;
    let y = preview.loc.y - labels.header + (labels.header_row() - label_h) / 2.;
    (
        Point::new(x, y),
        f64::max(1., preview.size.w - HEADER_ICON_SIZE - HEADER_ICON_GAP),
    )
}

/// Where each window on `output` is on screen now, as the layout draws it
/// (following view scrolling and workspace switches), in output-logical
/// coordinates. Windows on other outputs or hidden workspaces are missing.
fn flat_rects(niri: &Niri, output: &Output) -> HashMap<MappedId, Rectangle<f64, Logical>> {
    let mut rects = HashMap::new();
    let Some(mon) = niri.layout.monitor_for_output(output) else {
        return rects;
    };
    let zoom = mon.overview_zoom();
    for (ws, geo) in mon.workspaces_with_render_geo() {
        for (tile, pos, visible) in ws.tiles_with_render_positions() {
            if !visible {
                continue;
            }
            let loc = geo.loc + (pos + tile.window_loc()).upscale(zoom);
            let size = tile.window_size().upscale(zoom);
            rects.insert(tile.window().id(), Rectangle::new(loc, size));
        }
    }
    rects
}

/// Computes the centered, wrapping layout for a list of natural thumbnail sizes.
///
/// The returned sizes are the on-screen display sizes (each thumbnail keeps its
/// own aspect ratio), and the locations are their top-left corners. Thumbnails
/// keep their full size (`max_scale`, `max_height`) and fill rows left to right,
/// starting a new row when one is full; only when the rows outgrow the screen
/// does everything shrink, to the largest size that fits. `single_row` keeps
/// one row, shrinking it to fit. Every row is centered, and the whole block is
/// centered on the output. Also returns the row length.
#[allow(clippy::too_many_arguments)]
fn compute_grid_layout(
    natural: &[Size<f64, Logical>],
    output_size: Size<f64, Logical>,
    scale: f64,
    padding: f64,
    gap: f64,
    title_gap: f64,
    max_scale: f64,
    max_height: f64,
    single_row: bool,
    shrink_padding: bool,
    margin: Size<f64, Logical>,
) -> (usize, Vec<Size<f64, Logical>>, Vec<Point<f64, Logical>>) {
    let round = |logical: f64| round_logical_in_physical(scale, logical);

    let count = natural.len();
    if count == 0 {
        return (0, Vec::new(), Vec::new());
    }

    let max_w = natural.iter().map(|s| s.w).fold(1., f64::max);
    let max_h = natural.iter().map(|s| s.h).fold(1., f64::max);

    let working_w = (output_size.w - 2. * margin.w).max(1.);
    let working_h = (output_size.h - 2. * margin.h).max(1.);

    // The largest scale at which `cols` columns and `rows` rows fit. Icons
    // shrink their padding and the gaps between them too (as macOS's app
    // switcher); previews keep them.
    let fit = |cols: usize, rows: usize| -> f64 {
        let (c, r) = (cols as f64, rows as f64);
        let (s_w, s_h) = if shrink_padding {
            (
                working_w / (c * (max_w + 2. * padding) + (c - 1.) * gap),
                (working_h - r * title_gap) / (r * (max_h + 2. * padding) + (r - 1.) * gap),
            )
        } else {
            (
                (working_w - c * 2. * padding - (c - 1.) * gap) / (c * max_w),
                (working_h - r * (2. * padding + title_gap) - (r - 1.) * gap) / (r * max_h),
            )
        };
        s_w.min(s_h)
    };

    // Full size unless the rows outgrow the screen: then the largest size at
    // which some row length fits (one row, wrap false: the size that fits it).
    let s = if single_row {
        fit(count, 1)
    } else {
        (1..=count)
            .map(|cols| fit(cols, count.div_ceil(cols)))
            .fold(0., f64::max)
    };
    let s = s.min(max_scale).min(max_height / max_h).max(0.001);
    let (padding, gap) = if shrink_padding {
        (padding * s, gap * s)
    } else {
        (padding, gap)
    };

    // Rows fill left to right and wrap when full. Counted on the unrounded
    // width, so the row length `s` was chosen for always fits.
    let cols = if single_row {
        count
    } else {
        let per = max_w * s + 2. * padding + gap;
        (((working_w + gap) / per + 1e-6).floor() as usize).clamp(1, count)
    };
    let rows = count.div_ceil(cols);

    let content_w = round(max_w * s);
    let content_h = round(max_h * s);
    let box_w = content_w + 2. * padding;
    let box_h = content_h + 2. * padding + title_gap;
    let total_h = rows as f64 * box_h + (rows as f64 - 1.) * gap;
    let start_y = round((output_size.h - total_h) / 2.);

    let mut sizes = Vec::with_capacity(count);
    let mut locs = Vec::with_capacity(count);
    for (i, size) in natural.iter().enumerate() {
        let row = i / cols;
        let col = i % cols;
        // The short last row is centered on its own.
        let row_count = usize::min(cols, count - row * cols);
        let row_w = row_count as f64 * box_w + (row_count as f64 - 1.) * gap;
        let row_x = round((output_size.w - row_w) / 2.);

        let box_x = row_x + col as f64 * (box_w + gap);
        let box_y = start_y + row as f64 * (box_h + gap);

        let display = size.upscale(s);
        let display = display.to_physical_precise_round(scale).to_logical(scale);

        let loc = Point::new(
            round(box_x + padding + (content_w - display.w) / 2.),
            round(box_y + padding + (content_h - display.h) / 2.),
        );

        sizes.push(display);
        locs.push(loc);
    }

    (cols, sizes, locs)
}

/// Returns the index of the thumbnail one row above (`row_delta = -1`) or below
/// (`row_delta = 1`) `idx` in a grid with `cols` columns, or `None` at the grid
/// edge. Short rows, including the last one, clamp to the nearest column.
fn grid_neighbor_index(idx: usize, count: usize, cols: usize, row_delta: i32) -> Option<usize> {
    if count == 0 || cols == 0 || row_delta == 0 || idx >= count {
        return None;
    }

    let rows = count.div_ceil(cols);
    let row = idx / cols;
    let col = idx % cols;

    let target_row = row as i32 + row_delta;
    if target_row < 0 || target_row >= rows as i32 {
        return None;
    }
    let target_row = target_row as usize;

    let row_start = target_row * cols;
    let row_count = usize::min(cols, count - row_start);
    let target_col = usize::min(col, row_count - 1);
    Some(row_start + target_col)
}

// The scope panel is drawn as the Alniac UI library's SegmentedControl
// (alniac ui/qml/Alniac/UI/SegmentedControl.qml), the toggle ALNIAC's apps
// and wallpaper picker use: a recessed track, equal segments, the current
// one on a raised pill, hairlines between unchosen ones. ALNIAC's theme sync
// supplies its colours; keep these numbers in step with that file.

/// Logical height of the scope panel.
const PANEL_HEIGHT: f64 = 26.;

/// Inset between the scope panel edge and the current scope's pill.
const PANEL_INSET: f64 = 2.;

/// Horizontal padding added to the widest label for every segment.
const PANEL_SEGMENT_PADDING: f64 = 20.;

/// Height of the hairline between two unchosen segments, of the panel's.
const PANEL_SEPARATOR_FRACTION: f64 = 0.44;

/// Gap between the scope panel and the top of the container below it.
const PANEL_GAP: f64 = 12.;

/// Minimum distance between the scope panel and the top of the output.
const PANEL_MARGIN: f64 = 24.;

/// Scopes in the order they are cycled through.
static SCOPE_CYCLE: [MruScope; 3] = [MruScope::All, MruScope::Workspace, MruScope::Output];

/// Window MRU traversal context.
#[derive(Debug)]
pub struct WindowMru {
    /// Windows in MRU order.
    thumbnails: Vec<Thumbnail>,

    /// Id of the currently selected window.
    current_id: Option<MappedId>,

    /// Current scope.
    scope: MruScope,

    /// Current filter.
    app_id_filter: Option<String>,
}

pub struct WindowMruUi {
    state: UiState,
    preset_opened_binds: Vec<Bind>,
    dynamic_opened_binds: Vec<Bind>,
    config: Rc<RefCell<Config>>,
    icons: Rc<RefCell<IconCache>>,
}

pub enum MruCloseRequest {
    Cancel,
    Confirm,
}

niri_render_elements! {
    ThumbnailRenderElement<R> => {
        LayoutElement = LayoutElementRenderElement<R>,
        ClippedSurface = ClippedSurfaceRenderElement<R>,
        Border = BorderRenderElement,
    }
}

niri_render_elements! {
    WindowMruUiRenderElement<R> => {
        SolidColor = SolidColorRenderElement,
        TextureElement = PrimaryGpuTextureRenderElement,
        GradientFadeElem = GradientFadeTextureRenderElement,
        FocusRing = FocusRingRenderElement,
        BackdropBlur = FramebufferEffectElement,
        Offscreen = OffscreenRenderElement,
        Damage = ExtraDamage,
        Thumbnail = RelocateRenderElement<RescaleRenderElement<ThumbnailRenderElement<R>>>,
        Projected = ProjectedSurfaceRenderElement<R>,
    }
}

enum UiState {
    Open(Inner),
    Closing {
        inner: Inner,
        anim: Animation,
    },
    Closed {
        /// Scope used when the UI was last opened.
        previous_scope: MruScope,
    },
}

/// State of an opened MRU UI.
struct Inner {
    /// List of Window Ids to display in the MRU UI.
    wmru: WindowMru,

    /// Animation clock.
    clock: Clock,

    /// Current config.
    config: Rc<RefCell<Config>>,

    /// Time when the UI should appear.
    open_at: Duration,

    /// Output the UI was opened on.
    output: Output,

    /// Scope panel textures.
    scope_panel: RefCell<ScopePanel>,
    /// Where the scope panel was last drawn, for pointer hit-testing.
    panel_hit: RefCell<Option<PanelHit>>,
    /// Scope pill under the pointer.
    panel_hover: Cell<Option<MruScope>>,

    /// Rounded, tinted container panel behind the previews.
    container: RefCell<FocusRing>,
    /// Blur clipped to the container panel.
    container_blur: RefCell<BackdropBlur>,
    icons: Rc<RefCell<IconCache>>,

    /// Offscreen buffer for the closing fade animation on the main output.
    offscreen: OffscreenBuffer,

    /// The Flip 3D stack (`previews.style "flip"`).
    flip: FlipState,
}

/// The Flip 3D stack's motion: the windows fly from where they are into the
/// stack as it opens (`enter`), back as it closes, and the stack scrolls as
/// the selection moves (`scroll`).
#[derive(Default)]
struct FlipState {
    /// The selection's position in the stack once `scroll` ends: the index
    /// of the selected window, counted on without wrapping, so that a step
    /// past the last window keeps going forward.
    target: f64,
    scroll: Option<Animation>,
    /// From 0 (every window where it really is) to 1 (in the stack); created
    /// when the switcher appears.
    enter: Option<Animation>,
    /// The windows as last drawn, front first, for the pointer.
    hits: RefCell<Vec<(MappedId, [Point<f64, Logical>; 4])>>,
    /// Damage for a window whose projection changed without its box moving.
    damage: RefCell<HashMap<MappedId, (ExtraDamage, [Point<f64, Logical>; 4], f32)>>,
}

#[derive(Debug)]
struct MoveAnimation {
    anim: Animation,
    from: Point<f64, Logical>,
}

type MruTexture = TextureBuffer<GlesTexture>;

/// Cached title texture.
#[derive(Debug, Default)]
struct TitleTexture {
    title: String,
    font: String,
    scale: f64,
    color: Option<Color>,
    texture: Option<Option<MruTexture>>,
}

/// Cached scope panel textures.
#[derive(Debug, Default)]
struct ScopePanel {
    scale: f64,
    config: Option<niri_config::MruScopePanel>,
    /// Keyed by (current scope, hovered scope).
    textures: HashMap<(MruScope, Option<MruScope>), Option<PanelTexture>>,
}

/// Logical horizontal extent of each scope pill, from the panel's left edge.
type PillRanges = [(MruScope, f64, f64); 3];

#[derive(Debug, Clone)]
struct PanelTexture {
    texture: MruTexture,
    pills: PillRanges,
}

/// On-screen scope panel geometry.
#[derive(Debug, Clone, Copy)]
struct PanelHit {
    geometry: Rectangle<f64, Logical>,
    pills: PillRanges,
}

impl PanelHit {
    fn scope_at(&self, pos: Point<f64, Logical>) -> Option<MruScope> {
        if !self.geometry.contains(pos) {
            return None;
        }
        let x = pos.x - self.geometry.loc.x;
        self.pills
            .iter()
            .find(|(_, start, end)| *start <= x && x < *end)
            .map(|(scope, _, _)| *scope)
    }
}

#[derive(Debug)]
struct Thumbnail {
    id: MappedId,

    /// Focus timestamp, if any.
    timestamp: Option<Duration>,
    /// Whether the window is on the current MRU workspace.
    on_current_workspace: bool,
    /// Whether the window is on the current MRU output.
    on_current_output: bool,

    /// Cached app ID of the window.
    ///
    /// Currently not updated live to avoid having to refilter windows.
    app_id: Option<String>,
    /// Cached size of the window.
    size: Size<i32, Logical>,

    clock: Clock,
    config: niri_config::MruPreviews,
    open_animation: Option<Animation>,
    move_animation: Option<MoveAnimation>,
    title_texture: RefCell<TitleTexture>,
    background: RefCell<FocusRing>,
    border: RefCell<FocusRing>,
    preview_damage: ExtraDamage,
}

/// A live-framebuffer effect, never the wallpaper-only xray cache.
#[derive(Debug)]
struct BackdropBlur {
    effect: FramebufferEffect,
    config: Option<(
        niri_config::Blur,
        Option<f64>,
        CornerRadius,
        Rectangle<f64, Logical>,
    )>,
}

impl BackdropBlur {
    fn new() -> Self {
        Self {
            effect: FramebufferEffect::new(),
            config: None,
        }
    }

    /// The blur over `geometry`, and the alpha to draw it with: `strength`
    /// (a radius in logical pixels, 0 none) if given, else the global blur.
    fn render(
        &mut self,
        config: niri_config::Blur,
        strength: Option<f64>,
        geometry: Rectangle<f64, Logical>,
        radius: CornerRadius,
        scale: f64,
    ) -> Option<(FramebufferEffectElement, f32)> {
        if self.config != Some((config, strength, radius, geometry)) {
            self.config = Some((config, strength, radius, geometry));
            self.effect.damage();
        }
        if config.is_off() {
            return None;
        }
        // A radius, as the overview's backdrop blur: passes and offset worked
        // out from it, the faintest faded in rather than jumping.
        let (options, alpha) = match strength {
            Some(strength) => ramped_blur(strength * scale)?,
            None => (BlurOptions::global(config, scale), 1.),
        };
        let radius = radius.fit_to(geometry.size.w as f32, geometry.size.h as f32);
        let elem = self.effect.render(
            None,
            RenderParams {
                geometry,
                subregion: None,
                clip: Some((geometry, radius)),
                scale,
            },
            Some(options),
            config.noise as f32,
            config.saturation as f32,
        );
        Some((elem, alpha))
    }
}

impl Thumbnail {
    fn from_mapped(mapped: &Mapped, clock: Clock, config: niri_config::MruPreviews) -> Self {
        let app_id = with_toplevel_role(mapped.toplevel(), |role| role.app_id.clone());

        let background = FocusRing::new(niri_config::FocusRing {
            off: false,
            width: 0.,
            active_gradient: None,
            ..Default::default()
        });
        let border = FocusRing::new(niri_config::FocusRing {
            off: false,
            active_gradient: None,
            ..Default::default()
        });

        Self {
            id: mapped.id(),
            timestamp: mapped.get_focus_timestamp(),
            on_current_output: false,
            on_current_workspace: false,
            app_id,
            size: mapped.size(),
            clock,
            config,
            open_animation: None,
            move_animation: None,
            title_texture: Default::default(),
            background: RefCell::new(background),
            border: RefCell::new(border),
            preview_damage: ExtraDamage::new(),
        }
    }

    fn update_config(&mut self, config: niri_config::MruPreviews) {
        if self.config != config {
            self.preview_damage.damage_all();
        }
        self.config = config;
    }

    fn are_animations_ongoing(&self) -> bool {
        self.open_animation.is_some() || self.move_animation.is_some()
    }

    fn advance_animations(&mut self) {
        self.open_animation.take_if(|a| a.is_done());
        self.move_animation.take_if(|a| a.anim.is_done());
    }

    /// Animate thumbnail motion from the given visual offset (old position minus
    /// new position).
    fn animate_move_from_offset(
        &mut self,
        offset: Point<f64, Logical>,
        config: niri_config::Animation,
    ) {
        // Preserve the previous config if ongoing.
        let anim = self.move_animation.take().map(|ma| ma.anim);
        let anim = anim
            .map(|anim| anim.restarted(1., 0., 0.))
            .unwrap_or_else(|| Animation::new(self.clock.clone(), 1., 0., 0., config));

        self.move_animation = Some(MoveAnimation { anim, from: offset });
    }

    fn animate_open_with_config(&mut self, config: niri_config::Animation) {
        self.open_animation = Some(Animation::new(self.clock.clone(), 0., 1., 0., config));
    }

    fn render_offset(&self) -> Point<f64, Logical> {
        self.move_animation
            .as_ref()
            .map(|ma| Point::new(ma.from.x * ma.anim.value(), ma.from.y * ma.anim.value()))
            .unwrap_or(Point::new(0., 0.))
    }

    fn update_window(&mut self, mapped: &Mapped) {
        self.size = mapped.size();
    }

    #[allow(clippy::too_many_arguments)]
    fn render<R: NiriRenderer>(
        &self,
        mut ctx: RenderCtx<R>,
        config: &Config,
        mapped: &Mapped,
        preview_geo: Rectangle<f64, Logical>,
        tile: Rectangle<f64, Logical>,
        scale: f64,
        is_active: bool,
        bob_y: f64,
        icons: &mut IconCache,
        label: Option<&str>,
        labels: LabelLayout,
        label_bounds: Option<(f64, f64)>,
        push: &mut dyn FnMut(WindowMruUiRenderElement<R>),
    ) {
        let _span = tracy_client::span!("Thumbnail::render");

        let config = &config.recent_windows;
        let round = move |logical: f64| round_logical_in_physical(scale, logical);
        let padding = round(padding_for(
            &config.previews,
            config.highlight.padding,
            preview_geo.size.w,
        ));
        let label_top = labels.top(padding);
        let icons_only = config.previews.icons_only;

        let s = Scale::from(scale);

        let preview_alpha = self
            .open_animation
            .as_ref()
            .map_or(1., |a| a.clamped_value() as f32)
            .clamp(0., 1.);

        let bob_y = if mapped.rules().baba_is_float == Some(true) {
            bob_y
        } else {
            0.
        };
        let bob_offset = Point::new(0., bob_y);

        // Clip thumbnails to their geometry.
        let radius = if mapped.sizing_mode().is_normal() {
            mapped.geometry_corner_radius()
        } else {
            CornerRadius::default()
        };

        let has_border_shader = BorderRenderElement::has_shader(ctx.renderer);
        let clip_shader = ClippedSurfaceRenderElement::shader(ctx.renderer).cloned();
        let thumbnail_shader = if config.previews.antialiasing {
            ClippedSurfaceRenderElement::thumbnail_shader(ctx.renderer).cloned()
        } else {
            None
        };
        let geo = Rectangle::from_size(self.size.to_f64());
        let thumbnail_scale =
            (preview_geo.size.w / geo.size.w).min(preview_geo.size.h / geo.size.h);
        // FIXME: deduplicate code with Tile::render_inner()
        let clip = move |elem| match elem {
            LayoutElementRenderElement::Wayland(elem) => {
                if let Some(shader) = thumbnail_shader.clone() {
                    let elem = ClippedSurfaceRenderElement::new(elem, s, geo, shader, radius)
                        .with_thumbnail_filter(thumbnail_scale);
                    return ThumbnailRenderElement::ClippedSurface(elem);
                }
                if let Some(shader) = clip_shader.clone() {
                    if ClippedSurfaceRenderElement::will_clip(&elem, s, geo, radius) {
                        let elem =
                            ClippedSurfaceRenderElement::new(elem, s, geo, shader.clone(), radius);
                        return ThumbnailRenderElement::ClippedSurface(elem);
                    }
                }

                // If we don't have the shader, render it normally.
                let elem = LayoutElementRenderElement::Wayland(elem);
                ThumbnailRenderElement::LayoutElement(elem)
            }
            LayoutElementRenderElement::SolidColor(elem) => {
                // In this branch we're rendering a blocked-out window with a solid
                // color. We need to render it with a rounded corner shader even if
                // clip_to_geometry is false, because in this case we're assuming that
                // the unclipped window CSD already has corners rounded to the
                // user-provided radius, so our blocked-out rendering should match that
                // radius.
                if radius != CornerRadius::default() && has_border_shader {
                    return BorderRenderElement::new(
                        geo.size,
                        Rectangle::from_size(geo.size),
                        GradientInterpolation::default(),
                        Color::from_color32f(elem.color()),
                        Color::from_color32f(elem.color()),
                        0.,
                        Rectangle::from_size(geo.size),
                        0.,
                        radius,
                        (scale
                            * if config.previews.antialiasing {
                                thumbnail_scale
                            } else {
                                1.
                            }) as f32,
                        1.,
                    )
                    .into();
                }

                // Otherwise, render the solid color as is.
                LayoutElementRenderElement::SolidColor(elem).into()
            }
            elem @ LayoutElementRenderElement::BackgroundEffect(_) => {
                // This is only used on popups for now. If subsurface blur is implemented, this
                // will need to be handled somehow.
                error!("background effect clipping is unimplemented");
                elem.into()
            }
        };

        let downscale = move |elem| {
            let thumb_scale = Scale {
                x: preview_geo.size.w / geo.size.w,
                y: preview_geo.size.h / geo.size.h,
            };
            let offset = Point::new(
                preview_geo.size.w - (geo.size.w * thumb_scale.x),
                preview_geo.size.h - (geo.size.h * thumb_scale.y),
            )
            .downscale(2.);
            let elem = RescaleRenderElement::from_element(elem, Point::new(0, 0), thumb_scale);
            let elem = RelocateRenderElement::from_element(
                elem,
                (preview_geo.loc + offset + bob_offset).to_physical_precise_round(scale),
                Relocate::Relative,
            );
            WindowMruUiRenderElement::Thumbnail(elem)
        };

        // Keep the underlying Wayland element IDs for live frame callbacks.
        // Explicit damage also updates static previews when filtering is toggled.
        push(WindowMruUiRenderElement::Damage(
            self.preview_damage.render(Rectangle::new(
                preview_geo.loc + bob_offset,
                preview_geo.size,
            )),
        ));

        // Render elements are front-to-back: the icon must be above the window,
        // centered on its lower edge (half inside, half outside), like Exposé.
        let should_block_out = ctx.target.should_block_out(mapped.rules().block_out_from);
        if icons_only {
            // The app icon stands in for the preview, filling its box.
            if !should_block_out {
                let size = preview_geo.size.w;
                if let Some(texture) =
                    icons.get(ctx.as_gles().renderer, self.app_id.as_deref(), size, scale)
                {
                    let loc = (preview_geo.loc + bob_offset)
                        .to_physical_precise_round(scale)
                        .to_logical(scale);
                    let elem = TextureRenderElement::from_texture_buffer(
                        texture,
                        loc,
                        preview_alpha,
                        None,
                        None,
                        Kind::Unspecified,
                    );
                    push(WindowMruUiRenderElement::TextureElement(
                        PrimaryGpuTextureRenderElement(elem),
                    ));
                }
            }
        } else if !should_block_out {
            if let Some(texture) = icons.get(
                ctx.as_gles().renderer,
                self.app_id.as_deref(),
                HEADER_ICON_SIZE,
                scale,
            ) {
                let geo = header_icon_geometry(tile, &labels, scale);
                let elem = TextureRenderElement::from_texture_buffer(
                    texture,
                    geo.loc,
                    preview_alpha,
                    None,
                    None,
                    Kind::Unspecified,
                );
                push(WindowMruUiRenderElement::TextureElement(
                    PrimaryGpuTextureRenderElement(elem),
                ));
            }
        }

        // Filter directly on the GPU: no offscreen flattening, so hidden windows
        // keep their surface IDs, presentation tracking and live frame callbacks.
        if !icons_only {
            mapped.render_normal(ctx.r(), Point::new(0., 0.), s, preview_alpha, &mut |elem| {
                let elem = clip(elem);
                let elem = downscale(elem);
                push(elem)
            });
        }

        // The selected icon's lone label may use the container's width.
        let bounds = label_bounds.filter(|_| labels.selected_only && icons_only);
        let mut title_size = None;
        let title_texture = label.and_then(|label| {
            self.title_texture.borrow_mut().get(
                ctx.as_gles().renderer,
                label,
                &config.label.font,
                scale,
                config.container.title_color,
            )
        });
        let title_texture = title_texture.map(|texture| {
            let mut size = texture.logical_size();
            let max_w = match bounds {
                Some((x0, x1)) => f64::max(1., x1 - x0),
                None if !icons_only => header_label_geometry(tile, &labels, size.h).1,
                None => title_width_for(&config.previews, preview_geo.size.w, padding),
            };
            size.w = f64::min(size.w, max_w);
            title_size = Some(size);
            (texture, size)
        });

        // Hide title for blocked-out windows, but only after computing the title size. This way,
        // the background and the border won't have to oscillate in size between normal and
        // screencast renders, causing excessive damage.
        let title_texture = title_texture.filter(|_| !should_block_out);

        if let Some((texture, size)) = title_texture {
            // Clip from the right if it doesn't fit.
            let src = Rectangle::from_size(size);

            let loc = if icons_only {
                let mut x = preview_geo.loc.x + (preview_geo.size.w - size.w) / 2.;
                if let Some((x0, x1)) = bounds {
                    x = x.min(x1 - size.w).max(x0);
                }
                Point::new(x, preview_geo.loc.y + preview_geo.size.h + label_top)
            } else {
                header_label_geometry(tile, &labels, size.h).0
            };
            let loc = loc.to_physical_precise_round(scale).to_logical(scale);
            let texture = TextureRenderElement::from_texture_buffer(
                texture,
                loc,
                preview_alpha,
                Some(src),
                None,
                Kind::Unspecified,
            );

            let ctx = ctx.as_gles();
            if let Some(program) = GradientFadeTextureRenderElement::shader(ctx.renderer) {
                let elem = GradientFadeTextureRenderElement::new(texture, program);
                push(WindowMruUiRenderElement::GradientFadeElem(elem));
            } else {
                let elem = PrimaryGpuTextureRenderElement(texture);
                push(WindowMruUiRenderElement::TextureElement(elem));
            }
        }

        let is_urgent = mapped.is_urgent();
        if is_active || is_urgent {
            let padding = Point::new(padding, padding);

            let mut size = tile.size;
            size += padding.to_size().upscale(2.);
            // A preview's header is inside its highlight.
            size.h += labels.header;

            // A lone selected label sits under the highlight, not in it.
            if let Some(title_size) = title_size.filter(|_| !labels.selected_only && icons_only) {
                size.h += label_top + title_size.h;
                // Subtract half the padding so it looks more balanced visually.
                size.h -= round(padding.y / 2.);
            }

            // FIXME: gradient support (will require passing down correct view_rect).
            let highlight = &config.highlight;
            let mut color = if is_urgent {
                highlight.urgent_color
            } else {
                highlight.border_color.unwrap_or(highlight.active_color)
            };
            let mut background_color = highlight.background_color;
            if !is_active {
                color *= 0.4;
                background_color *= 0.4;
            }

            let radius = CornerRadius::from(config.highlight.corner_radius as f32);

            let loc = tile.loc - padding - Point::new(0., labels.header);

            let mut background = self.background.borrow_mut();
            let mut config = *background.config();
            // Fill and border are independent; preserve inactive-urgent dimming.
            config.active_color = background_color;
            background.update_config(config);
            background.update_render_elements(
                size,
                true,
                false,
                false,
                Rectangle::default(),
                radius,
                scale,
                highlight.background_opacity as f32,
            );
            background.render(ctx.renderer, loc, &mut |elem| {
                push(WindowMruUiRenderElement::FocusRing(elem))
            });

            let mut border = self.border.borrow_mut();
            let mut config = *border.config();
            config.off = !is_active;
            config.width = round(highlight.border_width);
            config.active_color = color;
            border.update_config(config);
            border.set_thicken_corners(false);
            border.update_render_elements(
                size,
                true,
                true,
                false,
                Rectangle::default(),
                radius.expanded_by(config.width as f32),
                scale,
                highlight.border_opacity as f32,
            );

            border.render(ctx.renderer, loc, &mut |elem| {
                push(WindowMruUiRenderElement::FocusRing(elem))
            });
        }
    }
}

impl WindowMru {
    pub fn new(niri: &Niri) -> Self {
        let Some(output) = niri.layout.active_output() else {
            return Self {
                thumbnails: Vec::new(),
                current_id: None,
                scope: MruScope::All,
                app_id_filter: None,
            };
        };

        let config = niri.config.borrow().recent_windows.previews;
        let mut thumbnails = Vec::new();
        for (mon, ws_idx, ws) in niri.layout.workspaces() {
            let mon = mon.expect("an active output exists so all workspaces have a monitor");
            let on_current_output = mon.output() == output;
            let on_current_workspace = on_current_output && mon.active_workspace_idx() == ws_idx;

            for mapped in ws.windows() {
                let mut thumbnail = Thumbnail::from_mapped(mapped, niri.clock.clone(), config);
                thumbnail.on_current_output = on_current_output;
                thumbnail.on_current_workspace = on_current_workspace;
                thumbnails.push(thumbnail);
            }
        }

        thumbnails
            .sort_by(|Thumbnail { timestamp: t1, .. }, Thumbnail { timestamp: t2, .. }| t2.cmp(t1));

        let current_id = thumbnails.first().map(|t| t.id);
        Self {
            thumbnails,
            current_id,
            scope: MruScope::All,
            app_id_filter: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.thumbnails.is_empty()
    }

    #[cfg(test)]
    fn verify_invariants(&self) {
        if let Some(id) = self.current_id {
            assert!(
                self.thumbnails().any(|thumbnail| thumbnail.id == id),
                "current_id must be present in the current filtered thumbnail list",
            );
        } else {
            assert!(
                self.thumbnails().next().is_none(),
                "unset current_id must mean that the filtered thumbnail list is empty",
            );
        }
    }

    fn thumbnails(&self) -> impl DoubleEndedIterator<Item = &Thumbnail> {
        let matches = match_filter(self.scope, self.app_id_filter.as_deref());
        self.thumbnails.iter().filter(move |t| matches(t))
    }

    fn thumbnails_with_idx(&self) -> impl DoubleEndedIterator<Item = (usize, &Thumbnail)> {
        let matches = match_filter(self.scope, self.app_id_filter.as_deref());
        self.thumbnails
            .iter()
            .enumerate()
            .filter(move |(_, t)| matches(t))
    }

    fn are_animations_ongoing(&self) -> bool {
        self.thumbnails.iter().any(|t| t.are_animations_ongoing())
    }

    fn advance_animations(&mut self) {
        for thumbnail in &mut self.thumbnails {
            thumbnail.advance_animations();
        }
    }

    fn forward(&mut self) {
        let Some(id) = self.current_id else {
            return;
        };

        let next = self.thumbnails().skip_while(|t| t.id != id).nth(1);
        self.current_id = Some(if let Some(next) = next {
            next.id
        } else {
            // We wrapped around.
            self.thumbnails().next().unwrap().id
        });
    }

    fn backward(&mut self) {
        let Some(id) = self.current_id else {
            return;
        };

        let next = self.thumbnails().rev().skip_while(|t| t.id != id).nth(1);
        self.current_id = Some(if let Some(next) = next {
            next.id
        } else {
            // We wrapped around.
            self.thumbnails().next_back().unwrap().id
        });
    }

    fn set_current(&mut self, id: MappedId) {
        if self.thumbnails().any(|thumbnail| thumbnail.id == id) {
            self.current_id = Some(id);
        }
    }

    fn first_id(&self) -> Option<MappedId> {
        self.thumbnails().next().map(|thumbnail| thumbnail.id)
    }

    fn first(&mut self) {
        self.current_id = self.first_id();
    }

    fn last(&mut self) {
        let id = self.thumbnails().next_back().map(|thumbnail| thumbnail.id);
        self.current_id = id;
    }

    pub fn set_scope(&mut self, scope: MruScope) -> Option<MruScope> {
        if self.scope == scope {
            return None;
        }
        let rv = Some(self.scope);

        if let Some(id) = self.current_id {
            let (current_idx, _) = self
                .thumbnails_with_idx()
                .find(|(_, thumbnail)| thumbnail.id == id)
                .unwrap();

            self.scope = scope;

            // Try to select the same, or the first thumbnail to the left. Failing that, select the
            // first one to the right.
            let mut id = self.first_id();

            for (idx, thumbnail) in self.thumbnails_with_idx() {
                if idx > current_idx {
                    break;
                }
                id = Some(thumbnail.id);
            }
            self.current_id = id;
        } else {
            self.scope = scope;
            self.current_id = self.first_id();
        }

        rv
    }

    pub fn set_filter(&mut self, filter: MruFilter) -> Option<Option<String>> {
        if self.app_id_filter.is_some() == (filter == MruFilter::AppId) {
            // Filter unchanged.
            return None;
        }

        if let Some(id) = self.current_id {
            let (current_idx, current_thumbnail) = self
                .thumbnails_with_idx()
                .find(|(_, thumbnail)| thumbnail.id == id)
                .unwrap();

            let old = match filter {
                MruFilter::All => {
                    let old = self.app_id_filter.take();
                    Some(old.expect("verified by early return at the top"))
                }
                MruFilter::AppId => {
                    // If the current thumbnail is missing an app id, we can't set the filter.
                    let current = current_thumbnail.app_id.clone()?;
                    let old = self.app_id_filter.replace(current);
                    assert!(old.is_none(), "verified by early return at the top");
                    None
                }
            };

            // Try to select the same, or the first thumbnail to the left. Failing that, select the
            // first one to the right.
            let mut id = self.first_id();

            for (idx, thumbnail) in self.thumbnails_with_idx() {
                if idx > current_idx {
                    break;
                }
                id = Some(thumbnail.id);
            }
            self.current_id = id;

            Some(old)
        } else {
            match filter {
                MruFilter::All => {
                    let old = self.app_id_filter.take();
                    let old = old.expect("verified by early return at the top");
                    self.current_id = self.first_id();
                    Some(Some(old))
                }
                MruFilter::AppId => {
                    // We don't have a current window to set the app id filter.
                    None
                }
            }
        }
    }

    fn idx_of(&self, id: MappedId) -> Option<usize> {
        self.thumbnails.iter().position(|t| t.id == id)
    }

    fn remove_by_idx(&mut self, idx: usize) -> Option<Thumbnail> {
        let id = self.thumbnails[idx].id;

        // Try to pick a different window when removing the current one.
        if self.current_id == Some(id) {
            self.forward();
        }

        // If we're still on the same window, that means it's the last visible one.
        if self.current_id == Some(id) {
            self.current_id = None;
        }

        Some(self.thumbnails.remove(idx))
    }
}

fn matches(scope: MruScope, app_id_filter: Option<&str>, thumbnail: &Thumbnail) -> bool {
    let x = match scope {
        MruScope::All => true,
        MruScope::Output => thumbnail.on_current_output,
        MruScope::Workspace => thumbnail.on_current_workspace,
    };
    if !x {
        return false;
    }

    if let Some(app_id) = app_id_filter {
        thumbnail.app_id.as_deref() == Some(app_id)
    } else {
        true
    }
}

fn match_filter(scope: MruScope, app_id_filter: Option<&str>) -> impl Fn(&Thumbnail) -> bool + '_ {
    move |thumbnail| matches(scope, app_id_filter, thumbnail)
}

impl WindowMruUi {
    pub fn new(config: Rc<RefCell<Config>>) -> Self {
        let mut rv = Self {
            state: UiState::Closed {
                previous_scope: MruScope::default(),
            },
            preset_opened_binds: Vec::new(),
            dynamic_opened_binds: Vec::new(),
            config,
            icons: Rc::new(RefCell::new(IconCache::new())),
        };
        rv.update_binds();
        rv
    }

    pub fn update_binds(&mut self) {
        let config = self.config.borrow();
        // The switcher's own keys (recent-windows.opened-binds) come first, then
        // the ones following the normal binds (close-window, focus-column-*).
        self.preset_opened_binds = config.recent_windows.opened_binds.clone();
        self.dynamic_opened_binds = make_dynamic_opened_binds(&config);
    }

    pub fn update_config(&mut self) {
        self.icons.borrow_mut().invalidate();
        let inner = match &mut self.state {
            UiState::Open(inner) => inner,
            UiState::Closing { inner, .. } => inner,
            UiState::Closed { .. } => return,
        };
        inner.update_config();
    }

    pub fn is_open(&self) -> bool {
        matches!(self.state, UiState::Open { .. })
    }

    pub fn open(&mut self, clock: Clock, wmru: WindowMru, output: Output) {
        if self.is_open() {
            return;
        }

        let open_delay = self.config.borrow().recent_windows.open_delay_ms;
        let open_delay = Duration::from_millis(u64::from(open_delay));

        // Apps may have been installed or changed their icon since last time.
        self.icons.borrow_mut().refresh();

        let mut inner = Inner {
            wmru,
            open_at: clock.now_unadjusted() + open_delay,
            clock,
            config: self.config.clone(),
            output,
            scope_panel: Default::default(),
            panel_hit: Default::default(),
            panel_hover: Default::default(),
            container: RefCell::new(FocusRing::new(niri_config::FocusRing {
                off: false,
                width: 0.,
                active_gradient: None,
                ..Default::default()
            })),
            container_blur: RefCell::new(BackdropBlur::new()),
            icons: self.icons.clone(),
            offscreen: OffscreenBuffer::default(),
            flip: FlipState::default(),
        };
        inner.sync_flip(false);

        self.state = UiState::Open(inner);
    }

    pub fn close(&mut self, close_request: MruCloseRequest) -> Option<MappedId> {
        if !self.is_open() {
            return None;
        }
        let state = mem::replace(
            &mut self.state,
            UiState::Closed {
                previous_scope: MruScope::default(),
            },
        );
        let UiState::Open(inner) = state else {
            unreachable!();
        };

        let response = match close_request {
            MruCloseRequest::Cancel => None,
            MruCloseRequest::Confirm => inner.wmru.current_id,
        };

        if !inner.is_fully_open() {
            // Hasn't displayed yet, no need to fade out.
            let UiState::Closed { previous_scope } = &mut self.state else {
                unreachable!()
            };
            *previous_scope = inner.wmru.scope;
            return response;
        }

        let config = self.config.borrow();
        let config = config.animations.recent_windows_close.0;

        let anim = Animation::new(inner.clock.clone(), 1., 0., 0., config);
        self.state = UiState::Closing { inner, anim };
        response
    }

    pub fn advance(&mut self, dir: MruDirection, filter: Option<MruFilter>) {
        let UiState::Open(inner) = &mut self.state else {
            return;
        };
        if let Some(filter) = filter {
            inner.set_filter(filter);
        }

        let flip = inner.is_flip();
        match dir {
            MruDirection::Forward => inner.wmru.forward(),
            MruDirection::Backward => inner.wmru.backward(),
            // The stack has no rows: up and down step through it too.
            MruDirection::Up if flip => inner.wmru.backward(),
            MruDirection::Down if flip => inner.wmru.forward(),
            MruDirection::Up => inner.advance_grid(-1),
            MruDirection::Down => inner.advance_grid(1),
        }
        inner.sync_flip(true);
    }

    pub fn set_scope(&mut self, scope: MruScope) {
        let UiState::Open(inner) = &mut self.state else {
            return;
        };
        inner.set_scope(scope);
    }

    pub fn cycle_scope(&mut self) {
        let UiState::Open(inner) = &mut self.state else {
            return;
        };

        let scope = inner.wmru.scope;
        let scope = SCOPE_CYCLE
            .into_iter()
            .cycle()
            .skip_while(|s| *s != scope)
            .nth(1)
            .unwrap();
        self.set_scope(scope);
    }

    pub fn pointer_motion(&mut self, pos_within_output: Point<f64, Logical>) -> Option<MappedId> {
        let UiState::Open(inner) = &mut self.state else {
            return None;
        };
        // Don't handle pointer until the UI is visible.
        if !inner.is_fully_open() {
            return None;
        }

        inner.panel_hover.set(inner.scope_under(pos_within_output));

        let id = inner.thumbnail_under(pos_within_output);
        // Hovering the stack doesn't select: the stack would scroll away
        // under the pointer. A click does (`select_under`).
        if let Some(id) = id.filter(|_| !inner.is_flip()) {
            inner.wmru.set_current(id);
        }
        id
    }

    /// Selects the window under the pointer, as a click does; returns it.
    pub fn select_under(&mut self, pos_within_output: Point<f64, Logical>) -> Option<MappedId> {
        let UiState::Open(inner) = &mut self.state else {
            return None;
        };
        if !inner.is_fully_open() {
            return None;
        }
        let id = inner.thumbnail_under(pos_within_output)?;
        inner.wmru.set_current(id);
        inner.sync_flip(true);
        Some(id)
    }

    /// Whether the switcher is drawing the windows of `output` itself (the
    /// Flip 3D stack, from the moment it shows until it has closed), so the
    /// layout must not draw them too.
    pub fn hides_windows(&self, output: &Output) -> bool {
        match &self.state {
            UiState::Open(inner) => {
                inner.is_flip() && inner.is_fully_open() && inner.output == *output
            }
            UiState::Closing { inner, .. } => inner.is_flip() && inner.output == *output,
            UiState::Closed { .. } => false,
        }
    }

    /// Scope pill under the pointer, if the switcher is open and showing the panel.
    pub fn scope_under(&self, pos_within_output: Point<f64, Logical>) -> Option<MruScope> {
        let UiState::Open(inner) = &self.state else {
            return None;
        };
        if !inner.is_fully_open() {
            return None;
        }
        inner.scope_under(pos_within_output)
    }

    pub fn first(&mut self) {
        let UiState::Open(inner) = &mut self.state else {
            return;
        };
        inner.wmru.first();
        inner.sync_flip(true);
    }

    pub fn last(&mut self) {
        let UiState::Open(inner) = &mut self.state else {
            return;
        };
        inner.wmru.last();
        inner.sync_flip(true);
    }

    pub fn scope(&self) -> MruScope {
        match &self.state {
            UiState::Closed { previous_scope, .. } => *previous_scope,
            UiState::Open(inner) | UiState::Closing { inner, .. } => inner.wmru.scope,
        }
    }

    pub fn current_window_id(&self) -> Option<MappedId> {
        let UiState::Open(inner) = &self.state else {
            return None;
        };
        inner.wmru.current_id
    }

    pub fn update_window(&mut self, layout: &Layout<Mapped>, id: MappedId) {
        let UiState::Open(inner) = &mut self.state else {
            return;
        };
        inner.update_window(layout, id);
    }

    pub fn remove_window(&mut self, id: MappedId) {
        let UiState::Open(inner) = &mut self.state else {
            return;
        };

        let Some(_thumbnail) = inner.remove_window(id) else {
            return;
        };

        if inner.wmru.thumbnails.is_empty() {
            self.close(MruCloseRequest::Cancel);
        }
    }

    pub fn render_output<R: NiriRenderer>(
        &self,
        niri: &Niri,
        output: &Output,
        mut ctx: RenderCtx<R>,
        push: &mut dyn FnMut(WindowMruUiRenderElement<R>),
    ) {
        let (inner, progress) = match &self.state {
            UiState::Closed { .. } => return,
            UiState::Closing { inner, anim } => (inner, anim.clamped_value()),
            UiState::Open(inner) => {
                if inner.is_fully_open() {
                    (inner, 1.)
                } else {
                    return;
                }
            }
        };

        let _span = tracy_client::span!("WindowMruUi::render_output");

        let alpha = progress.clamp(0., 1.) as f32;

        // The switcher only draws on the output it was opened on: a rounded,
        // blurred container hugging the previews, like macOS AltTab. The rest
        // of the desktop stays sharp and undimmed.
        if *output != inner.output {
            return;
        }

        // The stack flies back as the switcher closes, never through an
        // offscreen copy, so the windows stay live throughout.
        if inner.is_flip() {
            inner.render_flip(niri, ctx, progress, push);
            return;
        }

        let Some(container_geo) = inner.container_geometry() else {
            return;
        };
        let scale = output.current_scale().fractional_scale();
        // Full screen: no corners.
        let radius = if self.config.borrow().recent_windows.container.fullscreen {
            CornerRadius::default()
        } else {
            CornerRadius::from(self.config.borrow().recent_windows.container_radius() as f32)
        };

        // During the closing fade, use an offscreen to avoid transparent compositing artifacts.
        let mut pushed_offscreen = false;
        if alpha < 1. {
            let mut ctx = ctx.as_gles();

            let mut elems = Vec::new();
            inner.render(niri, ctx.r(), &mut |elem| elems.push(elem));
            inner.render_container(ctx.renderer, container_geo, radius, scale, &mut |elem| {
                elems.push(elem)
            });

            match inner
                .offscreen
                .render(ctx.renderer, Scale::from(scale), &elems)
            {
                Ok((elem, _sync, _data)) => {
                    // FIXME: would be good to passthrough offscreen data to visible windows here.
                    // As is, during the closing fade, windows from other workspaces stop receiving
                    // frame callbacks.
                    //
                    // Anyhow, this is not very noticeable since Alt-Tab closing happens quickly.
                    push(WindowMruUiRenderElement::Offscreen(elem.with_alpha(alpha)));
                    pushed_offscreen = true;
                }
                Err(err) => {
                    warn!("error rendering MRU to offscreen for fade-out: {err:?}");
                }
            }
        }

        // When alpha is 1., render everything directly, without an offscreen.
        //
        // This is not used as fallback when offscreen fails to render because it looks better to
        // hide the previews immediately than to render them with alpha = 1. during a fade-out.
        if alpha == 1. {
            inner.render(niri, ctx.r(), &mut |elem| push(elem));
            inner.render_container(ctx.renderer, container_geo, radius, scale, push);
        } else if !pushed_offscreen {
            return;
        }

        // Blur only the desktop behind the container. Elements are front-to-back,
        // so previews, highlights, icons, titles and the container tint are all
        // composed AFTER the framebuffer blur.
        let mut blur = inner.container_blur.borrow_mut();
        let (blur_config, strength) = {
            let config = inner.config.borrow();
            (config.blur, config.recent_windows.container.blur_radius)
        };
        if let Some((elem, ramp)) = blur.render(blur_config, strength, container_geo, radius, scale)
        {
            push(WindowMruUiRenderElement::BackdropBlur(
                elem.with_alpha(alpha * ramp),
            ));
        }
    }

    pub fn are_animations_ongoing(&self) -> bool {
        match &self.state {
            UiState::Open(inner) => inner.are_animations_ongoing(),
            UiState::Closing { .. } => true,
            UiState::Closed { .. } => false,
        }
    }

    pub fn advance_animations(&mut self) {
        match &mut self.state {
            UiState::Open(inner) => inner.advance_animations(),
            UiState::Closing { inner, anim } => {
                if anim.is_done() {
                    self.state = UiState::Closed {
                        previous_scope: inner.wmru.scope,
                    };
                    return;
                }
                inner.advance_animations();
            }
            UiState::Closed { .. } => {}
        }
    }

    pub fn opened_bindings(&mut self, mods: Modifiers) -> impl Iterator<Item = &Bind> + Clone {
        // Fill modifiers with the current mods.
        for bind in &mut self.preset_opened_binds {
            bind.key.modifiers = mods;
        }
        for bind in &mut self.dynamic_opened_binds {
            bind.key.modifiers = mods;
        }

        self.preset_opened_binds
            .iter()
            .chain(&self.dynamic_opened_binds)
    }

    pub fn output(&self) -> Option<&Output> {
        match &self.state {
            UiState::Open(inner) => Some(&inner.output),
            _ => None,
        }
    }

    #[cfg(feature = "dbus")]
    pub fn a11y_scope_text(&self) -> String {
        let scope = match self.scope() {
            MruScope::All => "all",
            MruScope::Output => "output",
            MruScope::Workspace => "workspace",
        };
        format!("Scope {scope}")
    }
}

impl Inner {
    fn update_config(&mut self) {
        let config = self.config.borrow().recent_windows.previews;
        for thumbnail in &mut self.wmru.thumbnails {
            thumbnail.update_config(config);
        }
    }

    fn are_animations_ongoing(&self) -> bool {
        self.clock.now_unadjusted() < self.open_at
            || self.wmru.are_animations_ongoing()
            || (!self.wmru.is_empty() && self.icons.borrow().pending())
            || self.flip.scroll.is_some()
            || self.flip.enter.as_ref().is_some_and(|a| !a.is_done())
    }

    fn advance_animations(&mut self) {
        self.wmru.advance_animations();
        self.flip.scroll.take_if(|a| a.is_done());
        // The stack starts flying in once the switcher shows.
        if self.flip.enter.is_none() && self.is_flip() && self.is_fully_open() {
            let config = self.config.borrow().animations.overview_open_close.0;
            self.flip.enter = Some(Animation::new(self.clock.clone(), 0., 1., 0., config));
        }
    }

    fn is_flip(&self) -> bool {
        self.config.borrow().recent_windows.previews.flip
    }

    /// The selection's position in the stack now (see `FlipState::target`).
    fn flip_scroll(&self) -> f64 {
        self.flip
            .scroll
            .as_ref()
            .map_or(self.flip.target, |a| a.value())
    }

    /// How far the windows are into the stack: 0 where they really are, 1 in
    /// the stack. `closing` runs from 1 to 0 as the switcher closes.
    fn flip_presence(&self, closing: f64) -> f64 {
        let enter = self.flip.enter.as_ref().map_or(0., |a| a.value());
        (enter * closing).clamp(0., 1.)
    }

    /// Moves the stack to the selected window: the short way round, animated
    /// from wherever it is (keeping its speed, so quick steps add up), or at
    /// once.
    fn sync_flip(&mut self, animate: bool) {
        let count = self.wmru.thumbnails().count();
        let Some(current) = self.wmru.current_id else {
            return;
        };
        let Some(index) = self.wmru.thumbnails().position(|t| t.id == current) else {
            return;
        };
        if !animate {
            self.flip.target = index as f64;
            self.flip.scroll = None;
            return;
        }
        let delta = flip::shortest_delta(self.flip.target, index, count);
        if delta == 0. {
            return;
        }
        let from = self.flip_scroll();
        let velocity = self.flip.scroll.as_ref().map_or(0., |a| {
            let now = self.clock.now();
            let dt = Duration::from_millis(4);
            (a.value_at(now) - a.value_at(now.saturating_sub(dt))) / dt.as_secs_f64()
        });
        self.flip.target += delta;
        let config = self.config.borrow().animations.window_movement.0;
        // Animation::new divides a velocity by the clock rate (it expects one
        // in real time); this one was measured on the clock.
        let velocity = velocity * self.clock.rate();
        self.flip.scroll = Some(Animation::new(
            self.clock.clone(),
            from,
            self.flip.target,
            velocity,
            config,
        ));
    }

    /// Each visible window's slot in the stack as drawn now.
    fn flip_slots(&self) -> HashMap<MappedId, f64> {
        let count = self.wmru.thumbnails().count();
        let scroll = self.flip_scroll();
        self.wmru
            .thumbnails()
            .enumerate()
            .map(|(i, t)| (t.id, flip::slot_of(i, scroll, count) + t.render_offset().x))
            .collect()
    }

    /// After the stack's windows changed, the selection is reset to the
    /// selected window, and every window slides from its old slot to its new
    /// one; new ones fade in.
    fn animate_flip_reflow_from(&mut self, old: HashMap<MappedId, f64>) {
        self.sync_flip(false);
        let open_config = self.config.borrow().animations.window_open.anim;
        let move_config = self.config.borrow().animations.window_movement.0;
        let new = self.flip_slots();
        let count = new.len().max(1) as f64;
        for thumbnail in &mut self.wmru.thumbnails {
            let Some(new_slot) = new.get(&thumbnail.id) else {
                continue;
            };
            match old.get(&thumbnail.id) {
                Some(old_slot) => {
                    // The short way, if it moved round the end of the stack.
                    let mut offset = old_slot - new_slot;
                    if offset > count / 2. {
                        offset -= count;
                    } else if offset < -count / 2. {
                        offset += count;
                    }
                    thumbnail.move_animation = None;
                    if offset.abs() > 1e-3 {
                        thumbnail.animate_move_from_offset(Point::new(offset, 0.), move_config);
                    }
                }
                None => thumbnail.animate_open_with_config(open_config),
            }
        }
    }

    fn is_fully_open(&self) -> bool {
        self.open_at <= self.clock.now_unadjusted()
    }

    /// Visual top-left of every visible thumbnail, including any in-flight move
    /// animation offset.
    fn visual_positions(&self) -> HashMap<MappedId, Point<f64, Logical>> {
        self.thumbnails()
            .map(|(thumbnail, geo)| {
                let offset = thumbnail.render_offset();
                (
                    thumbnail.id,
                    Point::new(geo.loc.x + offset.x, geo.loc.y + offset.y),
                )
            })
            .collect()
    }

    /// Animates the reflow that follows a visibility change.
    ///
    /// Existing thumbnails slide from their previous position; thumbnails that
    /// appeared fade in.
    fn animate_reflow_from(&mut self, old: HashMap<MappedId, Point<f64, Logical>>) {
        let open_config = self.config.borrow().animations.window_open.anim;
        let move_config = self.config.borrow().animations.window_movement.0;

        let new: Vec<(MappedId, Point<f64, Logical>)> = self
            .thumbnails()
            .map(|(thumbnail, geo)| (thumbnail.id, geo.loc))
            .collect();

        for (id, new_loc) in new {
            let Some(thumbnail) = self.wmru.thumbnails.iter_mut().find(|t| t.id == id) else {
                continue;
            };

            if let Some(old_loc) = old.get(&id) {
                let offset = Point::new(old_loc.x - new_loc.x, old_loc.y - new_loc.y);
                if offset.x.abs() >= 0.5 || offset.y.abs() >= 0.5 {
                    thumbnail.animate_move_from_offset(offset, move_config);
                }
            } else {
                thumbnail.animate_open_with_config(open_config);
            }
        }
    }

    fn update_window(&mut self, layout: &Layout<Mapped>, id: MappedId) {
        let Some(thumbnail) = self.wmru.thumbnails.iter_mut().find(|t| t.id == id) else {
            return;
        };

        let Some((_, mapped)) = layout.windows().find(|(_, m)| m.id() == id) else {
            error!("window in the MRU must be present in the layout");
            return;
        };

        thumbnail.update_window(mapped);
    }

    fn remove_window(&mut self, id: MappedId) -> Option<Thumbnail> {
        let idx = self.wmru.idx_of(id)?;

        if self.is_flip() {
            let old = self.flip_slots();
            let removed = self.wmru.remove_by_idx(idx);
            self.animate_flip_reflow_from(old);
            return removed;
        }

        let old = self.visual_positions();
        let removed = self.wmru.remove_by_idx(idx);
        self.animate_reflow_from(old);

        removed
    }

    fn set_scope(&mut self, scope: MruScope) {
        if self.is_flip() {
            let old = self.flip_slots();
            if self.wmru.set_scope(scope).is_some() {
                self.animate_flip_reflow_from(old);
            }
            return;
        }
        let old = self.visual_positions();
        if self.wmru.set_scope(scope).is_some() {
            self.animate_reflow_from(old);
        }
    }

    fn set_filter(&mut self, filter: MruFilter) {
        if self.is_flip() {
            let old = self.flip_slots();
            if self.wmru.set_filter(filter).is_some() {
                self.animate_flip_reflow_from(old);
            }
            return;
        }
        let old = self.visual_positions();
        if self.wmru.set_filter(filter).is_some() {
            self.animate_reflow_from(old);
        }
    }

    /// Lays out the visible thumbnails in a centered, wrapping grid.
    ///
    /// The thumbnail scale is chosen to make the previews as large as possible
    /// while fitting every window on screen, so fewer windows yield bigger
    /// previews and more windows shrink to fit.
    fn thumbnails(&self) -> impl Iterator<Item = (&Thumbnail, Rectangle<f64, Logical>)> {
        let (_, sizes, locs) = self.grid_layout();
        self.wmru
            .thumbnails()
            .zip(sizes.into_iter().zip(locs))
            .map(|(thumbnail, (size, loc))| (thumbnail, Rectangle::new(loc, size)))
    }

    /// Computes the size and top-left location of each visible thumbnail in the
    /// centered grid, plus the number of columns.
    fn grid_layout(&self) -> (usize, Vec<Size<f64, Logical>>, Vec<Point<f64, Logical>>) {
        let output_size = output_size(&self.output);
        let scale = self.output.current_scale().fractional_scale();
        let round = |logical: f64| round_logical_in_physical(scale, logical);

        let config = self.config.borrow();
        let recent = &config.recent_windows;
        let padding = round(recent.highlight.padding) + round(recent.highlight.border_width);
        let gap = round(recent.previews.gap);
        // Room under each row for its label (and a preview's app icon).
        let labels = LabelLayout::new(recent, scale);
        let title_gap = labels.header + labels.extent(padding) - padding;
        let (natural, max_scale, max_height): (Vec<Size<f64, Logical>>, f64, f64) =
            if recent.previews.icons_only {
                // Equal squares at the icon size, shrinking only to fit.
                let size = round(f64::max(1., recent.previews.icon_size));
                let natural = self.wmru.thumbnails().map(|_| Size::new(size, size));
                (natural.collect(), 1., size)
            } else {
                let natural = self.wmru.thumbnails().map(|t| t.size.to_f64());
                (
                    natural.collect(),
                    f64::max(0.001, recent.previews.max_scale),
                    f64::max(1., recent.previews.max_height),
                )
            };

        let (cols, sizes, mut locs) = compute_grid_layout(
            &natural,
            output_size,
            scale,
            padding,
            gap,
            title_gap,
            max_scale,
            max_height,
            !recent.previews.wraps(),
            recent.previews.icons_only,
            // The container's margins plus its padding around the items.
            Size::new(
                round(recent.container.margin_x) + round(recent.container.padding),
                round(recent.container.margin_y) + round(recent.container.padding),
            ),
        );
        // Each row's room holds the headers above the previews.
        for loc in &mut locs {
            loc.y += labels.header;
        }
        (cols, sizes, locs)
    }

    /// Number of columns in the current grid layout.
    /// The grid's cell width: the widest preview (each is centred in a cell
    /// that wide).
    fn cell_width(&self) -> f64 {
        self.thumbnails_in_view_static()
            .map(|(_, geo)| geo.size.w)
            .fold(0., f64::max)
    }

    fn grid_columns(&self) -> usize {
        self.grid_layout().0
    }

    /// Moves the selection one row up (`row_delta = -1`) or down
    /// (`row_delta = 1`), staying in place at the grid edges.
    fn advance_grid(&mut self, row_delta: i32) {
        let cols = self.grid_columns();
        let ids: Vec<MappedId> = self.wmru.thumbnails().map(|t| t.id).collect();
        let Some(current_id) = self.wmru.current_id else {
            return;
        };
        let Some(idx) = ids.iter().position(|id| *id == current_id) else {
            return;
        };
        if let Some(target) = grid_neighbor_index(idx, ids.len(), cols, row_delta) {
            self.wmru.set_current(ids[target]);
        }
    }

    fn thumbnails_in_view_static(
        &self,
    ) -> impl Iterator<Item = (&Thumbnail, Rectangle<f64, Logical>)> {
        // The grid always fits on screen, so nothing needs to be culled or
        // scrolled into view.
        self.thumbnails()
    }

    fn thumbnails_in_view_render(
        &self,
    ) -> impl Iterator<Item = (&Thumbnail, Rectangle<f64, Logical>)> {
        let output_size = output_size(&self.output);
        let scale = self.output.current_scale().fractional_scale();
        let round = move |logical: f64| round_logical_in_physical(scale, logical);

        self.thumbnails().filter_map(move |(thumbnail, mut geo)| {
            let offset = thumbnail.render_offset();
            geo.loc.x += round(offset.x);
            geo.loc.y += round(offset.y);

            if geo.loc.x + geo.size.w < 0.
                || output_size.w < geo.loc.x
                || geo.loc.y + geo.size.h < 0.
                || output_size.h < geo.loc.y
            {
                return None;
            }

            Some((thumbnail, geo))
        })
    }

    /// The scope panel, centred on `center_x` just above `top`, never closer
    /// to the top of the output than its margin.
    /// The scope panel, at `place` (its top left for its size).
    fn render_scope_panel<R: NiriRenderer>(
        &self,
        mut ctx: RenderCtx<R>,
        place: &dyn Fn(Size<f64, Logical>) -> Point<f64, Logical>,
        alpha: f32,
        push: &mut dyn FnMut(WindowMruUiRenderElement<R>),
    ) {
        let scale = self.output.current_scale().fractional_scale();
        let panel_config = self.config.borrow().recent_windows.scope_panel.clone();
        let current = self.wmru.scope;
        let hovered = self.panel_hover.get().filter(|scope| *scope != current);
        let panel = if panel_config.off {
            None
        } else {
            self.scope_panel.borrow_mut().get(
                ctx.as_gles().renderer,
                scale,
                &panel_config,
                current,
                hovered,
            )
        };
        *self.panel_hit.borrow_mut() = None;
        if let Some(PanelTexture { texture, pills }) = panel {
            let round = |logical: f64| round_logical_in_physical(scale, logical);
            let size = texture.logical_size();
            let at = place(size);
            let location = Point::new(round(at.x), round(at.y));
            *self.panel_hit.borrow_mut() = Some(PanelHit {
                geometry: Rectangle::new(location, size),
                pills,
            });
            let elem = PrimaryGpuTextureRenderElement(TextureRenderElement::from_texture_buffer(
                texture.clone(),
                location,
                alpha,
                None,
                None,
                Kind::Unspecified,
            ));
            push(WindowMruUiRenderElement::TextureElement(elem));
        }
    }

    fn render<R: NiriRenderer>(
        &self,
        niri: &Niri,
        mut ctx: RenderCtx<R>,
        push: &mut dyn FnMut(WindowMruUiRenderElement<R>),
    ) {
        let output_size = output_size(&self.output);
        let scale = self.output.current_scale().fractional_scale();

        // Sit just above the container, centred on it.
        let (center_x, top) = match self.container_geometry() {
            Some(geo) => (geo.loc.x + geo.size.w / 2., geo.loc.y),
            None => (output_size.w / 2., output_size.h / 2.),
        };
        // Just above the container, centred on it, never closer to the top of
        // the output than the margin.
        // Full screen, the container's top is the output's: the panel goes at
        // the top of the area panels (a top bar) leave free instead.
        let fullscreen = self.config.borrow().recent_windows.container.fullscreen;
        let free = layer_map_for_output(&self.output)
            .non_exclusive_zone()
            .to_f64();
        let above = |size: Size<f64, Logical>| {
            let round = |logical: f64| round_logical_in_physical(scale, logical);
            if fullscreen {
                return Point::new(
                    free.loc.x + (free.size.w - size.w) / 2.,
                    free.loc.y + round(PANEL_MARGIN),
                );
            }
            let y = f64::max(round(PANEL_MARGIN), top - round(PANEL_GAP) - size.h);
            Point::new(center_x - size.w / 2., y)
        };
        self.render_scope_panel(ctx.r(), &above, 1., push);

        let current_id = self.wmru.current_id;

        let bob_y = baba_is_float_offset(self.clock.now(), output_size.h);
        let bob_y = round_logical_in_physical(scale, bob_y);

        let config = self.config.borrow();
        let recent = &config.recent_windows;
        let labels = LabelLayout::new(recent, scale);
        let label_kind = recent.label.text_for(recent.previews.icons_only);
        let label_bounds = self.container_geometry().map(|geo| {
            let inset = round_logical_in_physical(scale, recent.container.padding);
            (geo.loc.x + inset, geo.loc.x + geo.size.w - inset)
        });

        let cell_w = self.cell_width();
        let icons_only = recent.previews.icons_only;
        let mut icons = self.icons.borrow_mut();
        for (thumbnail, geo) in self.thumbnails_in_view_render() {
            let id = thumbnail.id;
            let Some((_, mapped)) = niri.layout.windows().find(|(_, m)| m.id() == id) else {
                error!("window in the MRU must be present in the layout");
                continue;
            };

            let is_active = Some(id) == current_id;
            let label = (labels.shown && (is_active || !labels.selected_only))
                .then(|| {
                    let name = match label_kind {
                        MruLabelText::AppName | MruLabelText::AppAndTitle => {
                            icons.name(thumbnail.app_id.as_deref())
                        }
                        _ => None,
                    };
                    let title = with_toplevel_role(mapped.toplevel(), |role| role.title.clone());
                    let app_id = thumbnail.app_id.as_deref().is_some_and(|id| !id.is_empty());
                    label_text(label_kind, app_id, name, title)
                })
                .flatten();
            thumbnail.render(
                ctx.r(),
                &config,
                mapped,
                geo,
                tile_of(geo, cell_w, icons_only),
                scale,
                is_active,
                bob_y,
                &mut icons,
                label.as_deref(),
                labels,
                label_bounds,
                push,
            );
        }
    }

    /// The window label for `thumbnail`, as the grid would show it.
    fn label_for(
        &self,
        icons: &mut IconCache,
        thumbnail: &Thumbnail,
        mapped: &Mapped,
    ) -> Option<String> {
        let config = self.config.borrow();
        let recent = &config.recent_windows;
        let kind = recent.label.text_for(recent.previews.icons_only);
        if kind == MruLabelText::None {
            return None;
        }
        let name = match kind {
            MruLabelText::AppName | MruLabelText::AppAndTitle => {
                icons.name(thumbnail.app_id.as_deref())
            }
            _ => None,
        };
        let title = with_toplevel_role(mapped.toplevel(), |role| role.title.clone());
        let app_id = thumbnail.app_id.as_deref().is_some_and(|id| !id.is_empty());
        label_text(kind, app_id, name, title)
    }

    /// The Flip 3D stack: every window, live, in perspective. `closing` runs
    /// from 1 to 0 as the switcher closes.
    fn render_flip<R: NiriRenderer>(
        &self,
        niri: &Niri,
        mut ctx: RenderCtx<R>,
        closing: f64,
        push: &mut dyn FnMut(WindowMruUiRenderElement<R>),
    ) {
        let output_size = output_size(&self.output);
        let scale = self.output.current_scale().fractional_scale();
        let stage = flip::Stage::new(output_size, self.config.borrow().recent_windows.flip);
        let presence = self.flip_presence(closing);
        let scroll = self.flip_scroll();
        let count = self.wmru.thumbnails().count();
        let flat = flat_rects(niri, &self.output);
        let shown = self.config.borrow().recent_windows.flip.visible_windows;

        // On top: the scope panel and the selected window's name, fading with
        // the stack.
        // At the top right, clear of the stack: inside the area panels (a top
        // bar) leave free.
        let free = layer_map_for_output(&self.output)
            .non_exclusive_zone()
            .to_f64();
        let top_right = |size: Size<f64, Logical>| {
            Point::new(
                free.loc.x + free.size.w - FLIP_PANEL_MARGIN - size.w,
                free.loc.y + FLIP_PANEL_MARGIN,
            )
        };
        self.render_scope_panel(ctx.r(), &top_right, presence as f32, push);
        self.render_flip_label(niri, ctx.r(), presence, push);

        struct Card<'a> {
            thumbnail: &'a Thumbnail,
            mapped: &'a Mapped,
            size: Size<f64, Logical>,
            pose: flip::Pose,
            alpha: f64,
        }
        let mut cards = Vec::with_capacity(count);
        for (index, thumbnail) in self.wmru.thumbnails().enumerate() {
            let Some((_, mapped)) = niri.layout.windows().find(|(_, m)| m.id() == thumbnail.id)
            else {
                error!("window in the MRU must be present in the layout");
                continue;
            };
            let size = mapped.size().to_f64();
            if size.w < 1. || size.h < 1. {
                continue;
            }
            let slot = flip::slot_of(index, scroll, count) + thumbnail.render_offset().x;
            let in_stack = stage.slot_pose(slot, size);
            let slot_alpha = flip::slot_alpha(slot, count, shown);
            let (pose, alpha) = match flat.get(&thumbnail.id) {
                // Flying between where it is and the stack.
                Some(rect) => (
                    flip::Pose::flat(*rect, size).lerp(in_stack, presence),
                    1. + (slot_alpha - 1.) * presence,
                ),
                // Not on screen: fading in and out a slot behind its place.
                None => {
                    let mut pose = in_stack;
                    pose.z += (1. - presence) * stage.slot_depth(1.);
                    (pose, slot_alpha * presence)
                }
            };
            let open = thumbnail
                .open_animation
                .as_ref()
                .map_or(1., |a| a.clamped_value().clamp(0., 1.));
            let alpha = alpha * open;
            if alpha <= 0.002 {
                continue;
            }
            cards.push(Card {
                thumbnail,
                mapped,
                size,
                pose,
                alpha,
            });
        }
        // Elements go front to back: the nearest window first.
        cards.sort_by(|a, b| a.pose.z.total_cmp(&b.pose.z));

        let mut hits = self.flip.hits.borrow_mut();
        hits.clear();
        let mut damage = self.flip.damage.borrow_mut();
        damage.retain(|id, _| cards.iter().any(|c| c.thumbnail.id == *id));

        let Some(program) = ProjectedSurfaceRenderElement::shader(ctx.renderer).cloned() else {
            warn!("no projected surface shader; the Flip 3D switcher can't draw windows");
            return;
        };
        let clip = Rectangle::from_size(output_size.to_physical_precise_round(scale));
        let to_physical = [[scale, 0., 0.], [0., scale, 0.], [0., 0., 1.]];

        for card in &cards {
            let quad = stage.project(card.pose, card.size);
            let window_to_output = flip::mul(to_physical, flip::homography(card.size, quad));
            let Some(output_to_window) = flip::invert(window_to_output) else {
                continue;
            };
            // Pointer hits only on windows that are really there.
            if card.alpha > 0.5 {
                hits.push((card.thumbnail.id, quad));
            }

            // A change of the projection alone (its box unchanged) must still
            // repaint it.
            let alpha = card.alpha as f32;
            let entry = damage
                .entry(card.thumbnail.id)
                .or_insert_with(|| (ExtraDamage::new(), quad, alpha));
            if entry.1 != quad || entry.2 != alpha {
                entry.0.damage_all();
                entry.1 = quad;
                entry.2 = alpha;
            }

            let radius = if card.mapped.sizing_mode().is_normal() {
                card.mapped.geometry_corner_radius()
            } else {
                CornerRadius::default()
            };
            card.mapped.render_normal(
                ctx.r(),
                Point::new(0., 0.),
                Scale::from(scale),
                alpha,
                &mut |elem| {
                    // Blocked-out windows (a solid colour) stay out of the
                    // stack, as out of a screencast.
                    if let LayoutElementRenderElement::Wayland(elem) = elem {
                        if let Some(elem) = ProjectedSurfaceRenderElement::new(
                            elem,
                            Scale::from(scale),
                            window_to_output,
                            output_to_window,
                            card.size,
                            radius,
                            clip,
                            program.clone(),
                        ) {
                            push(WindowMruUiRenderElement::Projected(elem));
                        }
                    }
                },
            );
            push(WindowMruUiRenderElement::Damage(
                entry.0.render(flip::bounds(&quad)),
            ));
        }
    }

    /// The selected window's app icon and name on a pill at the bottom of the
    /// output, under the stack.
    fn render_flip_label<R: NiriRenderer>(
        &self,
        niri: &Niri,
        mut ctx: RenderCtx<R>,
        presence: f64,
        push: &mut dyn FnMut(WindowMruUiRenderElement<R>),
    ) {
        if presence <= 0. {
            return;
        }
        let Some(current) = self.wmru.current_id else {
            return;
        };
        let Some(thumbnail) = self.wmru.thumbnails().find(|t| t.id == current) else {
            return;
        };
        let Some((_, mapped)) = niri.layout.windows().find(|(_, m)| m.id() == current) else {
            return;
        };
        let output_size = output_size(&self.output);
        let scale = self.output.current_scale().fractional_scale();
        let round = |logical: f64| round_logical_in_physical(scale, logical);
        let alpha = presence as f32;

        let mut icons = self.icons.borrow_mut();
        let label = self.label_for(&mut icons, thumbnail, mapped);
        let config = self.config.borrow();
        let recent = &config.recent_windows;
        let title = label.and_then(|label| {
            thumbnail.title_texture.borrow_mut().get(
                ctx.as_gles().renderer,
                &label,
                &recent.label.font,
                scale,
                recent.container.title_color,
            )
        });
        let icon = icons.get(
            ctx.as_gles().renderer,
            thumbnail.app_id.as_deref(),
            HEADER_ICON_SIZE,
            scale,
        );

        // Icon, gap, name; at most most of the output wide.
        let title_size = title.as_ref().map(|t| {
            let mut size = t.logical_size();
            size.w = size.w.min(output_size.w * 0.6);
            size
        });
        let row_h = f64::max(HEADER_ICON_SIZE, title_size.map_or(0., |s| s.h));
        let content_w = HEADER_ICON_SIZE + title_size.map_or(0., |s| HEADER_ICON_GAP + s.w);
        let (pad_x, pad_y) = (16., 10.);
        let pill = Rectangle::new(
            Point::new(
                round((output_size.w - content_w) / 2. - pad_x),
                round(output_size.h - FLIP_LABEL_MARGIN - row_h - 2. * pad_y),
            ),
            Size::new(round(content_w + 2. * pad_x), round(row_h + 2. * pad_y)),
        );
        let x = pill.loc.x + pad_x;
        let y = pill.loc.y + pad_y;

        if let (Some(texture), Some(size)) = (title, title_size) {
            let loc = Point::new(
                x + HEADER_ICON_SIZE + HEADER_ICON_GAP,
                y + (row_h - size.h) / 2.,
            );
            let loc = loc.to_physical_precise_round(scale).to_logical(scale);
            let elem = TextureRenderElement::from_texture_buffer(
                texture,
                loc,
                alpha,
                Some(Rectangle::from_size(size)),
                None,
                Kind::Unspecified,
            );
            match GradientFadeTextureRenderElement::shader(ctx.as_gles().renderer) {
                Some(program) => push(WindowMruUiRenderElement::GradientFadeElem(
                    GradientFadeTextureRenderElement::new(elem, program),
                )),
                None => push(WindowMruUiRenderElement::TextureElement(
                    PrimaryGpuTextureRenderElement(elem),
                )),
            }
        }
        if let Some(texture) = icon {
            let loc = Point::new(x, y + (row_h - HEADER_ICON_SIZE) / 2.);
            let loc = loc.to_physical_precise_round(scale).to_logical(scale);
            let elem = TextureRenderElement::from_texture_buffer(
                texture,
                loc,
                alpha,
                None,
                None,
                Kind::Unspecified,
            );
            push(WindowMruUiRenderElement::TextureElement(
                PrimaryGpuTextureRenderElement(elem),
            ));
        }

        // The pill: the container's tint over its blur.
        let radius = CornerRadius::from((pill.size.h / 2.) as f32);
        drop(config);
        self.render_container_faded(ctx.renderer, pill, radius, scale, alpha, push);
        let (blur_config, strength) = {
            let config = self.config.borrow();
            (config.blur, config.recent_windows.container.blur_radius)
        };
        let mut blur = self.container_blur.borrow_mut();
        if let Some((elem, ramp)) = blur.render(blur_config, strength, pill, radius, scale) {
            push(WindowMruUiRenderElement::BackdropBlur(
                elem.with_alpha(alpha * ramp),
            ));
        }
    }

    /// Rounded container panel hugging every preview highlight and title.
    fn container_geometry(&self) -> Option<Rectangle<f64, Logical>> {
        // Full screen: all of the output, while there is anything to show.
        if self.config.borrow().recent_windows.container.fullscreen {
            return (self.wmru.thumbnails().next().is_some())
                .then(|| Rectangle::from_size(output_size(&self.output)));
        }
        let scale = self.output.current_scale().fractional_scale();
        let round = move |logical: f64| round_logical_in_physical(scale, logical);
        let config = self.config.borrow();
        let previews = config.recent_windows.previews;
        let raw_padding = config.recent_windows.highlight.padding;
        let border = config.recent_windows.highlight.border_width;
        let labels = LabelLayout::new(&config.recent_windows, scale);
        let margin = round(config.recent_windows.container.padding);
        drop(config);

        let cell_w = self.cell_width();
        let mut bounds: Option<(f64, f64, f64, f64)> = None;
        for (_, geo) in self.thumbnails_in_view_render() {
            let padding = round(padding_for(&previews, raw_padding, geo.size.w)) + round(border);
            let tile = tile_of(geo, cell_w, previews.icons_only);
            let x0 = tile.loc.x - padding;
            let y0 = geo.loc.y - padding - labels.header;
            let x1 = tile.loc.x + tile.size.w + padding;
            // The highlight with its label, or the selected item's lone label.
            let y1 = geo.loc.y + geo.size.h + labels.extent(padding);
            bounds = Some(match bounds {
                None => (x0, y0, x1, y1),
                Some((a, b, c, d)) => (a.min(x0), b.min(y0), c.max(x1), d.max(y1)),
            });
        }

        let (x0, y0, x1, y1) = bounds?;
        let loc = Point::new(round(x0 - margin), round(y0 - margin));
        let size = Size::new(round(x1 + margin) - loc.x, round(y1 + margin) - loc.y);
        Some(Rectangle::new(loc, size))
    }

    fn render_container<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        geo: Rectangle<f64, Logical>,
        radius: CornerRadius,
        scale: f64,
        push: &mut dyn FnMut(WindowMruUiRenderElement<R>),
    ) {
        self.render_container_faded(renderer, geo, radius, scale, 1., push);
    }

    fn render_container_faded<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        geo: Rectangle<f64, Logical>,
        radius: CornerRadius,
        scale: f64,
        alpha: f32,
        push: &mut dyn FnMut(WindowMruUiRenderElement<R>),
    ) {
        let tint = self.config.borrow().recent_windows.container;
        // An overlay opacity of its own: the colour at that opacity, its own
        // alpha aside; else the colour's alpha times the background opacity.
        let (color, opacity) = match tint.overlay_opacity {
            Some(opacity) => {
                let mut color = tint.background_color;
                color.a = 1.;
                (color, opacity)
            }
            None => (tint.background_color, tint.background_opacity),
        };
        let mut container = self.container.borrow_mut();
        let mut config = *container.config();
        config.active_color = color;
        container.update_config(config);
        container.update_render_elements(
            geo.size,
            true,
            false,
            false,
            Rectangle::default(),
            radius,
            scale,
            opacity as f32 * alpha,
        );
        container.render(renderer, geo.loc, &mut |elem| {
            push(WindowMruUiRenderElement::FocusRing(elem))
        });
    }

    fn scope_under(&self, pos: Point<f64, Logical>) -> Option<MruScope> {
        if self.config.borrow().recent_windows.scope_panel.off {
            return None;
        }
        self.panel_hit.borrow().and_then(|hit| hit.scope_at(pos))
    }

    fn thumbnail_under(&self, pos: Point<f64, Logical>) -> Option<MappedId> {
        if self.is_flip() {
            return self
                .flip
                .hits
                .borrow()
                .iter()
                .find(|(_, quad)| flip::quad_contains(quad, pos))
                .map(|(id, _)| *id);
        }
        let scale = self.output.current_scale().fractional_scale();
        let round = move |logical: f64| round_logical_in_physical(scale, logical);
        let config = self.config.borrow();
        let previews = config.recent_windows.previews;
        let raw_padding = config.recent_windows.highlight.padding;
        let border = config.recent_windows.highlight.border_width;
        let labels = LabelLayout::new(&config.recent_windows, scale);
        drop(config);

        let cell_w = self.cell_width();
        for (thumbnail, geo) in self.thumbnails_in_view_static() {
            let padding = round(padding_for(&previews, raw_padding, geo.size.w)) + round(border);
            let padding = Point::new(padding, padding);
            let mut geo = tile_of(geo, cell_w, previews.icons_only);
            geo.loc -= padding;
            geo.size += padding.to_size().upscale(2.);
            geo.loc.y -= labels.header;
            geo.size.h += labels.header;

            // It doesn't really matter all that much if the title texture is stale here, and it
            // would be annoying to thread the rendering into this function. The texture might be
            // one frame stale or so.
            // Only labels inside the highlight count; a lone selected label is not a target.
            let title = thumbnail.title_texture.borrow();
            let title = title
                .get_stale()
                .filter(|_| labels.shown && !labels.selected_only && labels.header == 0.);
            if let Some(texture) = title {
                let title_size = texture.logical_size();
                geo.size.h += labels.top(padding.y) + title_size.h;
                // Subtract half the padding so it looks more balanced visually.
                geo.size.h -= round(padding.y / 2.);
            }

            if geo.contains(pos) {
                return Some(thumbnail.id);
            }
        }

        None
    }
}

impl TitleTexture {
    fn get(
        &mut self,
        renderer: &mut GlesRenderer,
        title: &str,
        font: &str,
        scale: f64,
        color: Color,
    ) -> Option<MruTexture> {
        if self.title != title
            || self.font != font
            || self.scale != scale
            || self.color != Some(color)
        {
            self.texture = None;
            self.title = title.to_owned();
            self.font = font.to_owned();
            self.scale = scale;
            self.color = Some(color);
        }

        self.texture
            .get_or_insert_with(|| generate_title_texture(renderer, title, font, scale, color).ok())
            .clone()
    }

    fn get_stale(&self) -> Option<&MruTexture> {
        if let Some(Some(texture)) = &self.texture {
            Some(texture)
        } else {
            None
        }
    }
}

fn generate_title_texture(
    renderer: &mut GlesRenderer,
    title: &str,
    font: &str,
    scale: f64,
    color: Color,
) -> anyhow::Result<MruTexture> {
    let _span = tracy_client::span!("mru::generate_title_texture");

    let mut font = FontDescription::from_string(font);
    font.set_absolute_size(to_physical_precise_round(scale, font.size()));

    let surface = ImageSurface::create(cairo::Format::ARgb32, 0, 0)?;
    let cr = cairo::Context::new(&surface)?;
    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);
    // On Window CSD, line breaks are either stripped or replaced with the linebreak symbol anyway.
    // No use rendering it as multiple lines.
    layout.set_single_paragraph_mode(true);
    layout.set_font_description(Some(&font));
    layout.set_text(title);

    let (width, height) = layout.pixel_size();
    ensure!(width > 0 && height > 0);

    // Guard against overly long window titles.
    let width = min(width, 16383);
    let height = min(height, 16383);

    let surface = ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;
    let [r, g, b, a] = color.to_array_unpremul();
    cr.set_source_rgba(r.into(), g.into(), b.into(), a.into());
    pangocairo::functions::show_layout(&cr, &layout);

    drop(cr);
    let data = surface.take_data().unwrap();
    let buffer = TextureBuffer::from_memory(
        renderer,
        &data,
        Fourcc::Argb8888,
        (width, height),
        false,
        scale,
        Transform::Normal,
        Vec::new(),
    )?;

    Ok(buffer)
}

impl ScopePanel {
    fn get(
        &mut self,
        renderer: &mut GlesRenderer,
        scale: f64,
        config: &niri_config::MruScopePanel,
        current: MruScope,
        hovered: Option<MruScope>,
    ) -> Option<PanelTexture> {
        if self.scale != scale || self.config.as_ref() != Some(config) {
            self.textures.clear();
            self.scale = scale;
            self.config = Some(config.clone());
        }

        self.textures
            .entry((current, hovered))
            .or_insert_with(|| {
                render_panel(renderer, scale, config, current, hovered)
                    .map_err(|err| warn!("error rendering MRU scope panel: {err:?}"))
                    .ok()
            })
            .clone()
    }
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    let r = r.min(w / 2.).min(h / 2.).max(0.);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.);
    cr.arc(x + w - r, y + h - r, r, 0., FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, PI + FRAC_PI_2);
    cr.close_path();
}

fn set_color(cr: &cairo::Context, color: Color, alpha: f64) {
    let [r, g, b, a] = color.to_array_unpremul();
    cr.set_source_rgba(r.into(), g.into(), b.into(), f64::from(a) * alpha);
}

fn render_panel(
    renderer: &mut GlesRenderer,
    scale: f64,
    config: &niri_config::MruScopePanel,
    current: MruScope,
    hovered: Option<MruScope>,
) -> anyhow::Result<PanelTexture> {
    let _span = tracy_client::span!("mru::render_panel");

    let (surface, pills) = draw_panel(scale, config, current, hovered)?;
    let (w, h) = (surface.width(), surface.height());
    let data = surface.take_data().unwrap();
    let buffer = TextureBuffer::from_memory(
        renderer,
        &data,
        Fourcc::Argb8888,
        (w, h),
        false,
        scale,
        Transform::Normal,
        Vec::new(),
    )?;

    Ok(PanelTexture {
        texture: buffer,
        pills,
    })
}

/// Draws the scope panel at `scale`, with the logical extent of each pill.
fn draw_panel(
    scale: f64,
    config: &niri_config::MruScopePanel,
    current: MruScope,
    hovered: Option<MruScope>,
) -> anyhow::Result<(ImageSurface, PillRanges)> {
    let mut font = FontDescription::from_string(&config.font);
    font.set_absolute_size(to_physical_precise_round(scale, font.size()));

    let px = |logical: f64| (logical * scale).round();
    // Keep the border a whole number of physical pixels to avoid blurry edges.
    let border = if config.border_width > 0. {
        px(config.border_width).max(1.)
    } else {
        0.
    };
    let inset = px(PANEL_INSET).max(border);
    let pad_x = px(PANEL_SEGMENT_PADDING) / 2.;
    let radius = px(config.corner_radius);
    // SegmentedControl: track radiusSmall + 2, pill radiusSmall + 0.5.
    let pill_radius = (radius - 1.5).max(px(2.));

    let labels = SCOPE_CYCLE.map(|scope| match scope {
        MruScope::All => (scope, "All"),
        MruScope::Workspace => (scope, "Workspace"),
        MruScope::Output => (scope, "Monitor"),
    });

    // Measure on a dummy surface.
    let surface = ImageSurface::create(cairo::Format::ARgb32, 0, 0)?;
    let cr = cairo::Context::new(&surface)?;
    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);
    layout.set_font_description(Some(&font));
    let mut text_height = 0.;
    let widths = labels.map(|(_, text)| {
        layout.set_text(text);
        let (w, h) = layout.pixel_size();
        text_height = f64::max(text_height, f64::from(h));
        f64::from(w)
    });
    drop(cr);

    // Equal segments, each as wide as the widest label plus its padding.
    let segment_w = widths.iter().copied().fold(0., f64::max) + pad_x * 2.;
    let height = px(PANEL_HEIGHT).max(text_height + inset * 2.);
    let width = inset * 2. + segment_w * labels.len() as f64;
    let pill_h = height - inset * 2.;

    let surface = ImageSurface::create(
        cairo::Format::ARgb32,
        width.ceil() as i32,
        height.ceil() as i32,
    )?;
    let cr = cairo::Context::new(&surface)?;

    // Track and its hairline border, stroked inside the bounds.
    let half = border / 2.;
    rounded_rect(
        &cr,
        half,
        half,
        width - border,
        height - border,
        (radius - half).max(0.),
    );
    set_color(&cr, config.background_color, config.background_opacity);
    cr.fill_preserve()?;
    if border > 0. {
        set_color(&cr, config.border_color, 1.);
        cr.set_line_width(border);
        cr.stroke()?;
    } else {
        cr.new_path();
    }

    // The raised pill's own edge: a faint white line on a dark theme, a
    // faint black one on a light theme.
    let [br, bg, bb, _] = config.background_color.to_array_unpremul();
    let dark = 0.299 * f64::from(br) + 0.587 * f64::from(bg) + 0.114 * f64::from(bb) < 0.5;
    let (edge, edge_alpha) = if dark { (1., 0.06) } else { (0., 0.08) };
    let current_index = labels.iter().position(|(scope, _)| *scope == current);

    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);
    layout.set_font_description(Some(&font));

    let mut pills = [(MruScope::All, 0., 0.); 3];
    let mut x = inset;
    for (i, ((scope, text), text_w)) in labels.into_iter().zip(widths).enumerate() {
        let pill_w = segment_w;
        pills[i] = (scope, x / scale, (x + pill_w) / scale);
        if scope == current {
            rounded_rect(&cr, x, inset, pill_w, pill_h, pill_radius);
            set_color(&cr, config.active_color, 1.);
            cr.fill_preserve()?;
            cr.set_source_rgba(edge, edge, edge, edge_alpha);
            cr.set_line_width(px(1.).max(1.));
            cr.stroke()?;
        } else if i > 0 && current_index != Some(i - 1) {
            // Hairline between two unchosen segments.
            let line_h = (height * PANEL_SEPARATOR_FRACTION).round();
            cr.rectangle(x, ((height - line_h) / 2.).round(), px(1.).max(1.), line_h);
            set_color(&cr, config.border_color, 1.);
            cr.fill()?;
        }
        // The current and the hovered label are bright; the others plain.
        if scope == current || Some(scope) == hovered {
            set_color(&cr, config.active_text_color, 1.);
        } else {
            set_color(&cr, config.text_color, 1.);
        }
        layout.set_text(text);
        cr.move_to(
            (x + (pill_w - text_w) / 2.).round(),
            ((height - text_height) / 2.).round(),
        );
        pangocairo::functions::show_layout(&cr, &layout);
        x += pill_w;
    }

    drop(cr);
    Ok((surface, pills))
}

/// Returns dynamic key bindings available when the MRU UI is open.
///
/// These ones are generated based on the normal bindings.
fn make_dynamic_opened_binds(config: &Config) -> Vec<Bind> {
    let mut binds: HashMap<Trigger, Vec<Bind>> = HashMap::new();

    for bind in &config.binds.0 {
        let action = match &bind.action {
            Action::FocusColumnRight
            | Action::FocusColumnRightOrFirst
            | Action::FocusColumnOrMonitorRight
            | Action::FocusWindowDownOrColumnRight => Action::MruAdvance {
                direction: MruDirection::Forward,
                scope: None,
                filter: None,
            },
            Action::FocusColumnLeft
            | Action::FocusColumnLeftOrLast
            | Action::FocusColumnOrMonitorLeft
            | Action::FocusWindowUpOrColumnLeft => Action::MruAdvance {
                direction: MruDirection::Backward,
                scope: None,
                filter: None,
            },
            Action::FocusColumnFirst => Action::MruFirst,
            Action::FocusColumnLast => Action::MruLast,
            Action::CloseWindow => Action::MruCloseCurrentWindow,
            x @ Action::Screenshot(_, _) => x.clone(),
            _ => continue,
        };

        binds.entry(bind.key.trigger).or_default().push(Bind {
            action,
            ..bind.clone()
        });
    }

    let mut rv = Vec::new();

    // For each trigger, take the bind with the lowest number of modifiers.
    for binds in binds.into_values() {
        let bind = binds
            .into_iter()
            .min_by_key(|bind| bind.key.modifiers.iter().count())
            .unwrap();

        rv.push(Bind {
            key: Key {
                trigger: bind.key.trigger,
                // The modifier is filled dynamically.
                modifiers: Modifiers::empty(),
            },
            ..bind
        });
    }

    rv
}
