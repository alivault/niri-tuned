//! Server-side title bar: drawn by niri above a window that agreed to server-side decorations,
//! inside its border, with the window's title and close and maximize buttons.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::PI;

use anyhow::ensure;
use niri_config::{
    Color, CornerRadius, GradientInterpolation, TitlebarButton, TitlebarButtonHighlight,
    TitlebarButtonsPosition, TitlebarCloseRed,
};
use pango::FontDescription;
use pangocairo::cairo::{self, ImageSurface};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::utils::{Logical, Point, Rectangle, Size, Transform};

use crate::niri_render_elements;
use crate::render_helpers::border::BorderRenderElement;
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::solid_color::{SolidColorBuffer, SolidColorRenderElement};
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};

/// Part of a title bar under a point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TitlebarPart {
    /// The bar itself: drag to move, double-click to maximize.
    Bar,
    Button(TitlebarButton),
}

/// The window's state the buttons show: each toggle's icon says what a click will undo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TitlebarState {
    pub floating: bool,
    pub maximized: bool,
    pub fullscreen: bool,
}

impl TitlebarState {
    fn toggled(self, button: TitlebarButton) -> bool {
        match button {
            TitlebarButton::Close | TitlebarButton::MaximizeColumn | TitlebarButton::Menu => false,
            TitlebarButton::Float => self.floating,
            TitlebarButton::Zoom | TitlebarButton::Maximize => self.maximized || self.fullscreen,
            TitlebarButton::Fullscreen => self.fullscreen,
        }
    }
}

#[derive(Debug)]
pub struct Titlebar {
    config: niri_config::Titlebar,
    background: BorderRenderElement,
    buffer: SolidColorBuffer,
    use_shader: bool,
    size: Size<f64, Logical>,
    scale: f64,
    is_active: bool,
    hovered: Option<TitlebarPart>,
    state: TitlebarState,
    title: String,
    alpha: f32,
    textures: RefCell<Textures>,
}

#[derive(Debug, Default)]
struct Textures {
    title_key: Option<TitleKey>,
    title: Option<TextureBuffer<GlesTexture>>,
    /// By button, hovered, icon hovered (`button-hover-group`), active and toggled.
    buttons: HashMap<(TitlebarButton, bool, bool, bool, bool), Option<TextureBuffer<GlesTexture>>>,
}

#[derive(Debug, Clone, PartialEq)]
struct TitleKey {
    title: String,
    color: Color,
    scale: f64,
    max_width: i32,
}

niri_render_elements! {
    TitlebarRenderElement => {
        Background = BorderRenderElement,
        SolidColor = SolidColorRenderElement,
        Texture = PrimaryGpuTextureRenderElement,
    }
}

impl Titlebar {
    pub fn new(config: niri_config::Titlebar) -> Self {
        Self {
            config,
            background: BorderRenderElement::empty(),
            buffer: SolidColorBuffer::default(),
            use_shader: false,
            size: Size::default(),
            scale: 1.,
            is_active: false,
            hovered: None,
            state: TitlebarState::default(),
            title: String::new(),
            alpha: 1.,
            textures: RefCell::default(),
        }
    }

    pub fn update_config(&mut self, config: niri_config::Titlebar) {
        if self.config != config {
            *self.textures.get_mut() = Textures::default();
            self.config = config;
        }
    }

    pub fn update_shaders(&mut self) {
        self.background.damage_all();
    }

    pub fn is_off(&self) -> bool {
        self.config.off
    }

    pub fn height(&self) -> f64 {
        self.config.height
    }

    pub fn is_transparent(&self) -> bool {
        self.config.transparent
    }

    pub fn hovered(&self) -> Option<TitlebarPart> {
        self.hovered
    }

    pub fn set_hovered(&mut self, hovered: Option<TitlebarPart>) -> bool {
        let hovered = hovered.filter(|part| *part != TitlebarPart::Bar);
        let changed = self.hovered != hovered;
        self.hovered = hovered;
        changed
    }

    /// `radius` rounds the bar's top corners; its bottom corners meet the window.
    #[allow(clippy::too_many_arguments)]
    pub fn update_render_elements(
        &mut self,
        size: Size<f64, Logical>,
        is_active: bool,
        state: TitlebarState,
        title: &str,
        radius: CornerRadius,
        scale: f64,
        alpha: f32,
    ) {
        if self.scale != scale {
            *self.textures.get_mut() = Textures::default();
        }

        self.size = size;
        self.scale = scale;
        self.is_active = is_active;
        self.state = state;
        self.alpha = alpha;
        if self.title != title {
            self.title.clear();
            self.title.push_str(title);
        }

        let color = if is_active {
            self.config.active_color
        } else {
            self.config.inactive_color
        };

        let radius = CornerRadius {
            top_left: radius.top_left,
            top_right: radius.top_right,
            bottom_right: 0.,
            bottom_left: 0.,
        }
        .fit_to(size.w as f32, size.h as f32);

        self.use_shader = radius != CornerRadius::default();
        self.buffer.update(size, color);
        self.background.update(
            size,
            Rectangle::from_size(size),
            GradientInterpolation::default(),
            color,
            color,
            0.,
            Rectangle::from_size(size),
            0.,
            radius,
            scale as f32,
            alpha,
        );
    }

    /// The square slot of the `idx`th button from the bar's edge, relative to a bar of `size`.
    fn button_rect(&self, size: Size<f64, Logical>, idx: usize) -> Rectangle<f64, Logical> {
        let button = self.config.button_size;
        let offset = self.config.button_edge_gap + button * idx as f64;
        let x = match self.config.buttons_position {
            TitlebarButtonsPosition::Left => offset,
            TitlebarButtonsPosition::Right => size.w - offset - button,
        };
        let loc = Point::from((x, (size.h - button) / 2.))
            .to_physical_precise_round(self.scale)
            .to_logical(self.scale);
        Rectangle::new(loc, Size::from((button, button)))
    }

    /// The part of a bar of `size` under `point`, relative to the bar.
    pub fn hit(
        &self,
        size: Size<f64, Logical>,
        point: Point<f64, Logical>,
    ) -> Option<TitlebarPart> {
        if !Rectangle::from_size(size).contains(point) {
            return None;
        }

        for (idx, button) in self.config.buttons.iter().enumerate() {
            if self.button_rect(size, idx).contains(point) {
                return Some(TitlebarPart::Button(*button));
            }
        }

        Some(TitlebarPart::Bar)
    }

    pub fn render<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        location: Point<f64, Logical>,
        push: &mut dyn FnMut(TitlebarRenderElement),
    ) {
        if self.size.w < 1. || self.size.h < 1. {
            return;
        }

        let scale = self.scale;
        let round = |p: Point<f64, Logical>| p.to_physical_precise_round(scale).to_logical(scale);

        {
            let gles = renderer.as_gles_renderer();
            let mut textures = self.textures.borrow_mut();

            for (idx, button) in self.config.buttons.iter().copied().enumerate() {
                let hovered = self.hovered == Some(TitlebarPart::Button(button));
                // With button-hover-group, any button under the pointer shows every icon's
                // hover state.
                let icon_hovered = hovered
                    || (self.config.button_hover_group
                        && matches!(self.hovered, Some(TitlebarPart::Button(_))));
                let toggled = self.state.toggled(button);
                let key = (button, hovered, icon_hovered, self.is_active, toggled);
                let texture = textures
                    .buttons
                    .entry(key)
                    .or_insert_with(|| {
                        render_button(gles, &self.config, key, scale)
                            .map_err(|err| warn!("error rendering title bar button: {err:?}"))
                            .ok()
                    })
                    .clone();
                if let Some(texture) = texture {
                    let loc = round(location + self.button_rect(self.size, idx).loc);
                    let elem = TextureRenderElement::from_texture_buffer(
                        texture,
                        loc,
                        self.alpha,
                        None,
                        None,
                        Kind::Unspecified,
                    );
                    push(PrimaryGpuTextureRenderElement(elem).into());
                }
            }

            // The title is centred on the bar, clear of the buttons on either side. A
            // transparent bar leaves the title to the app.
            let reserved = self.config.button_edge_gap
                + self.config.button_size * self.config.buttons.len() as f64
                + self.config.title_gap;
            let max_width = self.size.w - reserved * 2.;
            let max_width_px = (max_width * scale).floor() as i32;
            if max_width_px > 0 && !self.title.is_empty() && !self.config.transparent {
                let color = if self.is_active {
                    self.config.active_text_color
                } else {
                    self.config.inactive_text_color
                };
                let key = TitleKey {
                    title: self.title.clone(),
                    color,
                    scale,
                    max_width: max_width_px,
                };
                if textures.title_key.as_ref() != Some(&key) {
                    textures.title = render_title(gles, &self.config.font, &key)
                        .map_err(|err| warn!("error rendering title bar title: {err:?}"))
                        .ok();
                    textures.title_key = Some(key);
                }

                if let Some(texture) = textures.title.clone() {
                    let size = texture.logical_size();
                    let loc =
                        Point::from(((self.size.w - size.w) / 2., (self.size.h - size.h) / 2.));
                    let elem = TextureRenderElement::from_texture_buffer(
                        texture,
                        round(location + loc),
                        self.alpha,
                        None,
                        None,
                        Kind::Unspecified,
                    );
                    push(PrimaryGpuTextureRenderElement(elem).into());
                }
            }
        }

        if self.config.transparent {
            return;
        }

        if self.use_shader && BorderRenderElement::has_shader(renderer) {
            push(self.background.clone().with_location(location).into());
        } else {
            let elem = SolidColorRenderElement::from_buffer(
                &self.buffer,
                location,
                self.alpha,
                Kind::Unspecified,
            );
            push(elem.into());
        }
    }
}

fn set_color(cr: &cairo::Context, color: Color) {
    let [r, g, b, a] = color.to_array_unpremul();
    cr.set_source_rgba(r.into(), g.into(), b.into(), a.into());
}

fn upload(
    renderer: &mut GlesRenderer,
    surface: ImageSurface,
    scale: f64,
) -> anyhow::Result<TextureBuffer<GlesTexture>> {
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
    Ok(buffer)
}

fn render_title(
    renderer: &mut GlesRenderer,
    font: &str,
    key: &TitleKey,
) -> anyhow::Result<TextureBuffer<GlesTexture>> {
    let _span = tracy_client::span!("titlebar::render_title");

    let mut font = FontDescription::from_string(font);
    let size = if font.is_size_absolute() {
        f64::from(font.size())
    } else {
        // Points at 96 DPI.
        f64::from(font.size()) * 96. / 72.
    };
    font.set_absolute_size((size * key.scale).round());

    let surface = ImageSurface::create(cairo::Format::ARgb32, 0, 0)?;
    let cr = cairo::Context::new(&surface)?;
    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);
    layout.set_single_paragraph_mode(true);
    layout.set_font_description(Some(&font));
    layout.set_ellipsize(pango::EllipsizeMode::End);
    layout.set_width(key.max_width.saturating_mul(pango::SCALE));
    layout.set_text(&key.title);

    let (width, height) = layout.pixel_size();
    ensure!(width > 0 && height > 0);
    let width = width.min(key.max_width).min(16383);
    let height = height.min(16383);

    let surface = ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;
    set_color(&cr, key.color);
    pangocairo::functions::show_layout(&cr, &layout);
    drop(cr);

    upload(renderer, surface, key.scale)
}

fn render_button(
    renderer: &mut GlesRenderer,
    config: &niri_config::Titlebar,
    (button, hovered, icon_hovered, is_active, toggled): (TitlebarButton, bool, bool, bool, bool),
    scale: f64,
) -> anyhow::Result<TextureBuffer<GlesTexture>> {
    let _span = tracy_client::span!("titlebar::render_button");

    let px = (config.button_size * scale).round().max(1.) as i32;
    let surface = ImageSurface::create(cairo::Format::ARgb32, px, px)?;
    let cr = cairo::Context::new(&surface)?;
    let size = f64::from(px);
    let c = size / 2.;

    let colors = button_colors(config, button, hovered, icon_hovered, is_active);
    if let Some(fill) = colors.circle {
        set_color(&cr, fill);
        let radius = config.button_circle * scale / 2.;
        cr.arc(c, c, radius, 0., 2. * PI);
        cr.fill()?;
        if colors.brighten {
            cr.set_source_rgba(1., 1., 1., config.button_hover_brighten);
            cr.arc(c, c, radius, 0., 2. * PI);
            cr.fill()?;
        }
    }
    let glyph_color = colors.icon;

    // The configured icon (a built-in Phosphor one by default), its view box fitted to the
    // icon side and centred in the slot.
    set_color(&cr, glyph_color);
    let spec = icon_spec(&config.icons, button, toggled);
    let icon = Icon::load(&spec).unwrap_or_else(|err| {
        warn!("error loading title bar icon {spec:?}: {err:?}");
        let fallback = icon_spec(&niri_config::TitlebarIcons::default(), button, toggled);
        Icon::load(&fallback).expect("built-in icons load")
    });
    icon.draw(&cr, c, config.icon_size * scale, scale)?;
    drop(cr);

    upload(renderer, surface, scale)
}

/// A button's colours in a state.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ButtonColors {
    /// The circle, if one shows.
    circle: Option<Color>,
    /// Whether hover lightens the circle (no hover colour of its own).
    brighten: bool,
    icon: Color,
}

/// A button's colours: its own (`button-styles`), else close's red where `close-red` puts it,
/// else the title bar's; hover falls back to the plain state, an unfocused window to the
/// focused one's colours. `icon_hovered` (the button, or with `button-hover-group` any
/// button, under the pointer) gives the icon its hover colour.
fn button_colors(
    config: &niri_config::Titlebar,
    button: TitlebarButton,
    hovered: bool,
    icon_hovered: bool,
    is_active: bool,
) -> ButtonColors {
    let own = config.button_styles.get(button);
    let is_close = button == TitlebarButton::Close;
    let red_plain = is_close
        && match config.close_red {
            TitlebarCloseRed::Hover => false,
            TitlebarCloseRed::Focused => is_active,
            TitlebarCloseRed::Always => true,
        };
    let red_hover = is_close;
    let red = config.close_hover_color;
    let on_red = config.button_hover_text_color;

    // Without the pointer.
    let plain_circle = if is_active {
        own.color
            .or(red_plain.then_some(red))
            .unwrap_or(config.button_color)
    } else {
        own.inactive_color
            .or(own.color)
            .or(red_plain.then_some(red))
            .or(config.button_inactive_color)
            .unwrap_or(config.button_color)
    };
    let plain_icon = if is_active {
        own.icon_color
            .or(red_plain.then_some(on_red))
            .or(config.button_icon_color)
            .unwrap_or(config.active_text_color)
    } else {
        own.icon_inactive_color
            .or(own.icon_color)
            .or(red_plain.then_some(on_red))
            .or(config.button_icon_inactive_color)
            .unwrap_or(config.inactive_text_color)
    };
    let always = config.button_highlight == TitlebarButtonHighlight::Always;
    let icon = if icon_hovered {
        // Close's on-red icon only on its red (hovered) circle.
        own.icon_hover_color
            .or((red_hover && hovered).then_some(on_red))
            .or(config.button_icon_hover_color)
            .unwrap_or(plain_icon)
    } else {
        plain_icon
    };

    if !hovered {
        return ButtonColors {
            circle: always.then_some(plain_circle),
            brighten: false,
            icon,
        };
    }

    let hover_circle = own
        .hover_color
        .or(red_hover.then_some(red))
        .or(config.button_hover_color);
    ButtonColors {
        circle: Some(hover_circle.unwrap_or(plain_circle)),
        // No hover colour: lighten the plain circle, where it shows anyway.
        brighten: hover_circle.is_none() && always,
        icon,
    }
}

/// What a button shows: the configured icon, else the built-in one.
fn icon_spec(icons: &niri_config::TitlebarIcons, button: TitlebarButton, toggled: bool) -> String {
    let (plain, active, default, default_active) = match button {
        TitlebarButton::Close => (&icons.close, &None, "x", "x"),
        // Plus to maximize; maximized, cards to float.
        TitlebarButton::Zoom => (&icons.zoom, &icons.zoom_active, "plus", "cards"),
        // Floating: back into the columns.
        TitlebarButton::Float => (
            &icons.float,
            &icons.float_active,
            "cards",
            "square-split-horizontal",
        ),
        TitlebarButton::Maximize => (&icons.maximize, &icons.maximize_active, "plus", "plus"),
        TitlebarButton::MaximizeColumn => (&icons.maximize_column, &None, "square", "square"),
        TitlebarButton::Fullscreen => (
            &icons.fullscreen,
            &icons.fullscreen_active,
            "corners-out",
            "corners-in",
        ),
        TitlebarButton::Menu => (&icons.menu, &None, "dots-three", "dots-three"),
    };
    let spec = if toggled {
        // An unset active icon is the plain one when that is configured, else the built-in
        // active one.
        active
            .clone()
            .or_else(|| plain.clone())
            .unwrap_or_else(|| default_active.to_owned())
    } else {
        plain.clone().unwrap_or_else(|| default.to_owned())
    };
    spec
}

fn builtin_icon(name: &str) -> Option<&'static str> {
    Some(match name {
        "x" => phosphor::X,
        "cards" => phosphor::CARDS,
        "square" => phosphor::SQUARE,
        "square-split-horizontal" => phosphor::SQUARE_SPLIT_HORIZONTAL,
        "plus" => phosphor::PLUS,
        "minus" => phosphor::MINUS,
        "arrows-out-simple" => phosphor::ARROWS_OUT_SIMPLE,
        "arrows-in-simple" => phosphor::ARROWS_IN_SIMPLE,
        "arrows-out-line-horizontal" => phosphor::ARROWS_OUT_LINE_HORIZONTAL,
        "corners-out" => phosphor::CORNERS_OUT,
        "corners-in" => phosphor::CORNERS_IN,
        "dots-three" => phosphor::DOTS_THREE,
        _ => return None,
    })
}

/// An icon as SVG path data on its view box, filled, or stroked at a width.
#[derive(Debug, Clone, PartialEq)]
struct Icon {
    view_box: (f64, f64, f64, f64),
    paths: Vec<String>,
    stroke: Option<f64>,
}

impl Icon {
    /// A built-in name, or an SVG file (`~/...` or absolute).
    fn load(spec: &str) -> anyhow::Result<Self> {
        if let Some(d) = builtin_icon(spec) {
            return Ok(Self {
                view_box: (0., 0., 256., 256.),
                paths: vec![d.to_owned()],
                stroke: None,
            });
        }
        let path = match spec.strip_prefix("~/") {
            Some(rest) => std::path::PathBuf::from(std::env::var("HOME")?).join(rest),
            None => std::path::PathBuf::from(spec),
        };
        anyhow::ensure!(
            path.is_absolute(),
            "not a built-in icon or an absolute SVG path"
        );
        Self::parse(&std::fs::read_to_string(&path)?)
    }

    /// The paths, lines, polylines, polygons, rects and circles of an SVG, as path data.
    fn parse(svg: &str) -> anyhow::Result<Self> {
        let mut view_box = (0., 0., 24., 24.);
        let mut stroke = None;
        let mut paths = Vec::new();
        for tag in svg.split('<').skip(1) {
            let tag = tag.split('>').next().unwrap_or_default();
            let name = tag.split_whitespace().next().unwrap_or_default();
            let attr = |key: &str| svg_attr(tag, key);
            let num = |key: &str| attr(key).and_then(|v| v.trim().parse::<f64>().ok());
            match name {
                "svg" => {
                    if let Some(vb) = attr("viewBox") {
                        let v: Vec<f64> = vb
                            .split(|c: char| c == ',' || c.is_whitespace())
                            .filter_map(|n| n.parse().ok())
                            .collect();
                        if v.len() == 4 {
                            view_box = (v[0], v[1], v[2], v[3]);
                        }
                    }
                    if attr("fill").as_deref() == Some("none") {
                        stroke = Some(num("stroke-width").unwrap_or(2.));
                    }
                }
                "path" => paths.extend(attr("d")),
                "line" => paths.push(format!(
                    "M{},{}L{},{}",
                    num("x1").unwrap_or(0.),
                    num("y1").unwrap_or(0.),
                    num("x2").unwrap_or(0.),
                    num("y2").unwrap_or(0.)
                )),
                "polyline" | "polygon" => {
                    if let Some(points) = attr("points") {
                        let mut d = format!("M{points}");
                        if name == "polygon" {
                            d.push('Z');
                        }
                        paths.push(d);
                    }
                }
                "circle" => {
                    let (cx, cy, r) = (
                        num("cx").unwrap_or(0.),
                        num("cy").unwrap_or(0.),
                        num("r").unwrap_or(0.),
                    );
                    paths.push(format!(
                        "M{},{}A{r},{r},0,1,0,{},{}A{r},{r},0,1,0,{},{}Z",
                        cx - r,
                        cy,
                        cx + r,
                        cy,
                        cx - r,
                        cy
                    ));
                }
                "rect" => {
                    let (x, y) = (num("x").unwrap_or(0.), num("y").unwrap_or(0.));
                    let (w, h) = (num("width").unwrap_or(0.), num("height").unwrap_or(0.));
                    let r = num("rx")
                        .or(num("ry"))
                        .unwrap_or(0.)
                        .min(w / 2.)
                        .min(h / 2.);
                    paths.push(format!(
                        "M{},{}H{}A{r},{r},0,0,1,{},{}V{}A{r},{r},0,0,1,{},{}H{}A{r},{r},0,0,1,{},{}V{}A{r},{r},0,0,1,{},{}Z",
                        x + r, y, x + w - r, x + w, y + r, y + h - r, x + w - r, y + h,
                        x + r, x, y + h - r, y + r, x + r, y
                    ));
                }
                _ => (),
            }
        }
        anyhow::ensure!(!paths.is_empty(), "no shapes in the SVG");
        Ok(Self {
            view_box,
            paths,
            stroke,
        })
    }

    /// Draws the icon centred on (`c`, `c`), its view box fitted to `side` (pixels).
    fn draw(&self, cr: &cairo::Context, c: f64, side: f64, scale: f64) -> anyhow::Result<()> {
        let (vx, vy, vw, vh) = self.view_box;
        let k = side / vw.max(vh).max(1.);
        cr.save()?;
        cr.translate(c - vw * k / 2., c - vh * k / 2.);
        cr.scale(k, k);
        cr.translate(-vx, -vy);
        for d in &self.paths {
            svg_path(cr, d)?;
        }
        cr.restore()?;
        match self.stroke {
            // The SVG's stroke width, on its view box.
            Some(width) => {
                cr.set_line_width((width * k).max(scale * 0.75));
                cr.set_line_cap(cairo::LineCap::Round);
                cr.set_line_join(cairo::LineJoin::Round);
                cr.stroke()?;
            }
            None => cr.fill()?,
        }
        Ok(())
    }
}

/// An attribute of an SVG tag's text (`key="value"`).
fn svg_attr(tag: &str, key: &str) -> Option<String> {
    let mut rest = tag;
    while let Some(i) = rest.find(key) {
        let before = rest[..i].chars().last();
        let after = &rest[i + key.len()..];
        if before.is_some_and(char::is_whitespace) {
            if let Some(after) = after.trim_start().strip_prefix('=') {
                let after = after.trim_start();
                let quote = after.chars().next()?;
                if quote == '"' || quote == '\'' {
                    let value = &after[1..];
                    return value.find(quote).map(|end| value[..end].to_owned());
                }
            }
        }
        rest = after;
    }
    None
}

/// The Phosphor icons built in (Phosphor Icons 2.1.1, regular weight, MIT,
/// https://phosphoricons.com), whatever icon set the desktop uses.
mod phosphor {
    pub const DOTS_THREE: &str = "M140,128a12,12,0,1,1-12-12A12,12,0,0,1,140,128Zm56-12a12,12,0,1,0,12,12A12,12,0,0,0,196,116ZM60,116a12,12,0,1,0,12,12A12,12,0,0,0,60,116Z";
    pub const PLUS: &str = "M224,128a8,8,0,0,1-8,8H136v80a8,8,0,0,1-16,0V136H40a8,8,0,0,1,0-16h80V40a8,8,0,0,1,16,0v80h80A8,8,0,0,1,224,128Z";
    pub const SQUARE_SPLIT_HORIZONTAL: &str = "M200,40H56A16,16,0,0,0,40,56V200a16,16,0,0,0,16,16H200a16,16,0,0,0,16-16V56A16,16,0,0,0,200,40ZM56,56h64V200H56ZM200,200H136V56h64V200Z";
    pub const SQUARE: &str = "M208,32H48A16,16,0,0,0,32,48V208a16,16,0,0,0,16,16H208a16,16,0,0,0,16-16V48A16,16,0,0,0,208,32Zm0,176H48V48H208V208Z";
    pub const MINUS: &str = "M224,128a8,8,0,0,1-8,8H40a8,8,0,0,1,0-16H216A8,8,0,0,1,224,128Z";
    pub const X: &str = "M205.66,194.34a8,8,0,0,1-11.32,11.32L128,139.31,61.66,205.66a8,8,0,0,1-11.32-11.32L116.69,128,50.34,61.66A8,8,0,0,1,61.66,50.34L128,116.69l66.34-66.35a8,8,0,0,1,11.32,11.32L139.31,128Z";
    pub const CARDS: &str = "M184,72H40A16,16,0,0,0,24,88V200a16,16,0,0,0,16,16H184a16,16,0,0,0,16-16V88A16,16,0,0,0,184,72Zm0,128H40V88H184V200ZM232,56V176a8,8,0,0,1-16,0V56H64a8,8,0,0,1,0-16H216A16,16,0,0,1,232,56Z";
    pub const ARROWS_OUT_SIMPLE: &str = "M216,48V96a8,8,0,0,1-16,0V67.31l-50.34,50.35a8,8,0,0,1-11.32-11.32L188.69,56H160a8,8,0,0,1,0-16h48A8,8,0,0,1,216,48ZM106.34,138.34,56,188.69V160a8,8,0,0,0-16,0v48a8,8,0,0,0,8,8H96a8,8,0,0,0,0-16H67.31l50.35-50.34a8,8,0,0,0-11.32-11.32Z";
    pub const ARROWS_IN_SIMPLE: &str = "M213.66,53.66,163.31,104H192a8,8,0,0,1,0,16H144a8,8,0,0,1-8-8V64a8,8,0,0,1,16,0V92.69l50.34-50.35a8,8,0,0,1,11.32,11.32ZM112,136H64a8,8,0,0,0,0,16H92.69L42.34,202.34a8,8,0,0,0,11.32,11.32L104,163.31V192a8,8,0,0,0,16,0V144A8,8,0,0,0,112,136Z";
    pub const ARROWS_OUT_LINE_HORIZONTAL: &str = "M136,40V216a8,8,0,0,1-16,0V40a8,8,0,0,1,16,0ZM96,120H35.31l18.35-18.34A8,8,0,0,0,42.34,90.34l-32,32a8,8,0,0,0,0,11.32l32,32a8,8,0,0,0,11.32-11.32L35.31,136H96a8,8,0,0,0,0-16Zm149.66,2.34-32-32a8,8,0,0,0-11.32,11.32L220.69,120H160a8,8,0,0,0,0,16h60.69l-18.35,18.34a8,8,0,0,0,11.32,11.32l32-32A8,8,0,0,0,245.66,122.34Z";
    pub const CORNERS_OUT: &str = "M216,48V88a8,8,0,0,1-16,0V56H168a8,8,0,0,1,0-16h40A8,8,0,0,1,216,48ZM88,200H56V168a8,8,0,0,0-16,0v40a8,8,0,0,0,8,8H88a8,8,0,0,0,0-16Zm120-40a8,8,0,0,0-8,8v32H168a8,8,0,0,0,0,16h40a8,8,0,0,0,8-8V168A8,8,0,0,0,208,160ZM88,40H48a8,8,0,0,0-8,8V88a8,8,0,0,0,16,0V56H88a8,8,0,0,0,0-16Z";
    pub const CORNERS_IN: &str = "M152,96V48a8,8,0,0,1,16,0V88h40a8,8,0,0,1,0,16H160A8,8,0,0,1,152,96ZM96,152H48a8,8,0,0,0,0,16H88v40a8,8,0,0,0,16,0V160A8,8,0,0,0,96,152Zm112,0H160a8,8,0,0,0-8,8v48a8,8,0,0,0,16,0V168h40a8,8,0,0,0,0-16ZM96,40a8,8,0,0,0-8,8V88H48a8,8,0,0,0,0,16H96a8,8,0,0,0,8-8V48A8,8,0,0,0,96,40Z";
}

/// Splits SVG path data into commands and their numbers.
fn svg_tokens(d: &str) -> Vec<(char, Vec<f64>)> {
    let mut out: Vec<(char, Vec<f64>)> = Vec::new();
    let bytes = d.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        if ch.is_ascii_alphabetic() && ch != 'e' && ch != 'E' {
            out.push((ch, Vec::new()));
            i += 1;
        } else if ch == '-' || ch == '+' || ch == '.' || ch.is_ascii_digit() {
            let start = i;
            i += 1;
            let mut seen_dot = ch == '.';
            while i < bytes.len() {
                let c = bytes[i] as char;
                if c.is_ascii_digit() {
                    i += 1;
                } else if c == '.' && !seen_dot {
                    seen_dot = true;
                    i += 1;
                } else if (c == 'e' || c == 'E') && i + 1 < bytes.len() {
                    i += 1;
                    if bytes[i] == b'-' || bytes[i] == b'+' {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            if let (Some(last), Ok(n)) = (out.last_mut(), d[start..i].parse()) {
                last.1.push(n);
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Adds SVG path data to cairo's path: the commands Phosphor uses (M L H V C A Z, absolute and
/// relative), elliptical arcs without rotation.
fn svg_path(cr: &cairo::Context, d: &str) -> anyhow::Result<()> {
    let (mut x, mut y) = (0., 0.);
    let (mut sx, mut sy) = (0., 0.);
    for (cmd, nums) in svg_tokens(d) {
        let rel = cmd.is_ascii_lowercase();
        match cmd.to_ascii_uppercase() {
            'M' => {
                for (i, p) in nums.chunks_exact(2).enumerate() {
                    let (ox, oy) = if rel { (x, y) } else { (0., 0.) };
                    x = ox + p[0];
                    y = oy + p[1];
                    if i == 0 {
                        cr.move_to(x, y);
                        (sx, sy) = (x, y);
                    } else {
                        cr.line_to(x, y);
                    }
                }
            }
            'L' => {
                for p in nums.chunks_exact(2) {
                    let (ox, oy) = if rel { (x, y) } else { (0., 0.) };
                    x = ox + p[0];
                    y = oy + p[1];
                    cr.line_to(x, y);
                }
            }
            'H' => {
                for v in &nums {
                    x = if rel { x + v } else { *v };
                    cr.line_to(x, y);
                }
            }
            'V' => {
                for v in &nums {
                    y = if rel { y + v } else { *v };
                    cr.line_to(x, y);
                }
            }
            'C' => {
                for p in nums.chunks_exact(6) {
                    let (ox, oy) = if rel { (x, y) } else { (0., 0.) };
                    cr.curve_to(
                        ox + p[0],
                        oy + p[1],
                        ox + p[2],
                        oy + p[3],
                        ox + p[4],
                        oy + p[5],
                    );
                    x = ox + p[4];
                    y = oy + p[5];
                }
            }
            'A' => {
                for p in nums.chunks_exact(7) {
                    let (ox, oy) = if rel { (x, y) } else { (0., 0.) };
                    let (x2, y2) = (ox + p[5], oy + p[6]);
                    svg_arc(cr, (x, y), (p[0], p[1]), p[3] != 0., p[4] != 0., (x2, y2))?;
                    x = x2;
                    y = y2;
                }
            }
            'Z' => {
                cr.close_path();
                (x, y) = (sx, sy);
            }
            _ => anyhow::bail!("unsupported SVG path command {cmd}"),
        }
    }
    Ok(())
}

/// An SVG arc (no rotation) from `from` to `to`, as cairo's: the centre parametrisation of the
/// SVG spec's implementation notes (F.6.5).
fn svg_arc(
    cr: &cairo::Context,
    from: (f64, f64),
    (mut rx, mut ry): (f64, f64),
    large: bool,
    sweep: bool,
    to: (f64, f64),
) -> anyhow::Result<()> {
    let (x1, y1) = from;
    let (x2, y2) = to;
    if rx == 0. || ry == 0. || (x1 == x2 && y1 == y2) {
        cr.line_to(x2, y2);
        return Ok(());
    }
    rx = rx.abs();
    ry = ry.abs();
    let (xp, yp) = ((x1 - x2) / 2., (y1 - y2) / 2.);
    let lambda = xp * xp / (rx * rx) + yp * yp / (ry * ry);
    if lambda > 1. {
        rx *= lambda.sqrt();
        ry *= lambda.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * yp * yp - ry * ry * xp * xp;
    let den = rx * rx * yp * yp + ry * ry * xp * xp;
    let sign = if large == sweep { -1. } else { 1. };
    let coef = sign * (num / den).max(0.).sqrt();
    let (cxp, cyp) = (coef * rx * yp / ry, -coef * ry * xp / rx);
    let (cx, cy) = (cxp + (x1 + x2) / 2., cyp + (y1 + y2) / 2.);
    let start = ((yp - cyp) / ry).atan2((xp - cxp) / rx);
    let end = ((-yp - cyp) / ry).atan2((-xp - cxp) / rx);
    let mut delta = end - start;
    if sweep && delta < 0. {
        delta += 2. * PI;
    } else if !sweep && delta > 0. {
        delta -= 2. * PI;
    }
    cr.save()?;
    cr.translate(cx, cy);
    cr.scale(rx, ry);
    if sweep {
        cr.arc(0., 0., 1., start, start + delta);
    } else {
        cr.arc_negative(0., 0., 1., start, start + delta);
    }
    cr.restore()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_parses() {
        for d in [
            phosphor::PLUS,
            phosphor::SQUARE,
            phosphor::SQUARE_SPLIT_HORIZONTAL,
            phosphor::X,
            phosphor::CARDS,
            phosphor::ARROWS_OUT_SIMPLE,
            phosphor::ARROWS_IN_SIMPLE,
            phosphor::ARROWS_OUT_LINE_HORIZONTAL,
            phosphor::CORNERS_OUT,
            phosphor::CORNERS_IN,
        ] {
            let surface = ImageSurface::create(cairo::Format::ARgb32, 256, 256).unwrap();
            let cr = cairo::Context::new(&surface).unwrap();
            svg_path(&cr, d).unwrap();
            let (x1, y1, x2, y2) = cr.fill_extents().unwrap();
            // Every icon stays on Phosphor's grid, inside its padding.
            assert!(
                x1 >= 7. && y1 >= 7. && x2 <= 249. && y2 <= 249.,
                "{x1} {y1} {x2} {y2}"
            );
            assert!(x2 - x1 > 100. && y2 - y1 > 100.);
        }
    }

    #[test]
    fn button_colors_fall_back() {
        let c = |r: u8| Color::from_rgba8_unpremul(r, 0, 0, 255);
        let mut config = niri_config::Titlebar::default();
        config.button_color = c(1);
        config.active_text_color = c(2);
        config.inactive_text_color = c(3);
        config.close_hover_color = c(4);
        config.button_hover_text_color = c(5);
        let get = |config: &niri_config::Titlebar, b, hovered, active| {
            button_colors(config, b, hovered, hovered, active)
        };
        let square = TitlebarButton::MaximizeColumn;
        let close = TitlebarButton::Close;

        // Defaults: grey circles, icons in the title's colours, hover lightens, close red on hover.
        let plain = get(&config, square, false, true);
        assert_eq!(
            (plain.circle, plain.brighten, plain.icon),
            (Some(c(1)), false, c(2))
        );
        assert_eq!(get(&config, square, false, false).icon, c(3));
        let hover = get(&config, square, true, true);
        assert_eq!(
            (hover.circle, hover.brighten, hover.icon),
            (Some(c(1)), true, c(2))
        );
        let hover = get(&config, close, true, true);
        assert_eq!(
            (hover.circle, hover.brighten, hover.icon),
            (Some(c(4)), false, c(5))
        );
        assert_eq!(get(&config, close, false, true).circle, Some(c(1)));

        // close-red "focused": red on the focused window only.
        config.close_red = TitlebarCloseRed::Focused;
        assert_eq!(get(&config, close, false, true).circle, Some(c(4)));
        assert_eq!(get(&config, close, false, true).icon, c(5));
        assert_eq!(get(&config, close, false, false).circle, Some(c(1)));
        config.close_red = TitlebarCloseRed::Hover;

        // The bar's own button colours, then each button's.
        config.button_inactive_color = Some(c(6));
        config.button_hover_color = Some(c(7));
        config.button_icon_color = Some(c(8));
        config.button_icon_hover_color = Some(c(9));
        assert_eq!(get(&config, square, false, false).circle, Some(c(6)));
        let hover = get(&config, square, true, true);
        assert_eq!(
            (hover.circle, hover.brighten, hover.icon),
            (Some(c(7)), false, c(9))
        );
        assert_eq!(get(&config, square, false, true).icon, c(8));
        config.button_styles.close = Some(niri_config::TitlebarButtonStyle {
            hover_color: Some(c(10)),
            color: Some(c(11)),
            ..Default::default()
        });
        assert_eq!(get(&config, close, true, true).circle, Some(c(10)));
        // An unfocused window takes the button's colour when it has no inactive one.
        assert_eq!(get(&config, close, false, false).circle, Some(c(11)));
        assert_eq!(get(&config, square, false, false).circle, Some(c(6)));

        // Circles on hover only.
        config.button_highlight = TitlebarButtonHighlight::Hover;
        assert_eq!(get(&config, square, false, true).circle, None);
        config.button_highlight = TitlebarButtonHighlight::Always;

        // button-hover-group: another button hovered gives the icon its hover colour, the
        // circle its plain one.
        let group = button_colors(&config, square, false, true, true);
        assert_eq!(
            (group.circle, group.brighten, group.icon),
            (Some(c(1)), false, c(9))
        );
        let group = button_colors(&config, close, false, true, true);
        assert_eq!((group.circle, group.icon), (Some(c(11)), c(9)));
    }

    #[test]
    fn icons_come_from_config() {
        let mut icons = niri_config::TitlebarIcons::default();
        assert_eq!(
            icon_spec(&icons, TitlebarButton::MaximizeColumn, false),
            "square"
        );
        assert_eq!(
            icon_spec(&icons, TitlebarButton::MaximizeColumn, true),
            "square"
        );
        assert_eq!(icon_spec(&icons, TitlebarButton::Menu, false), "dots-three");
        assert!(builtin_icon("dots-three").is_some());
        assert_eq!(icon_spec(&icons, TitlebarButton::Zoom, false), "plus");
        assert_eq!(icon_spec(&icons, TitlebarButton::Zoom, true), "cards");
        assert_eq!(icon_spec(&icons, TitlebarButton::Maximize, false), "plus");
        assert_eq!(icon_spec(&icons, TitlebarButton::Maximize, true), "plus");
        assert_eq!(
            icon_spec(&icons, TitlebarButton::Fullscreen, true),
            "corners-in"
        );
        assert_eq!(icon_spec(&icons, TitlebarButton::Float, false), "cards");
        assert_eq!(
            icon_spec(&icons, TitlebarButton::Float, true),
            "square-split-horizontal"
        );
        icons.maximize = Some("arrows-out-simple".into());
        // The active icon follows a configured plain one unless set itself.
        assert_eq!(
            icon_spec(&icons, TitlebarButton::Maximize, true),
            "arrows-out-simple"
        );
        icons.maximize_active = Some("arrows-in-simple".into());
        assert_eq!(
            icon_spec(&icons, TitlebarButton::Maximize, true),
            "arrows-in-simple"
        );
        assert!(Icon::load("plus").is_ok());
        assert!(Icon::load("no-such-icon").is_err());
    }

    #[test]
    fn svg_files_parse() {
        // Phosphor: filled paths on 256.
        let icon = Icon::parse(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" fill="currentColor"><path d="M1,2L3,4Z"/></svg>"#).unwrap();
        assert_eq!(icon.view_box, (0., 0., 256., 256.));
        assert_eq!(icon.paths, vec!["M1,2L3,4Z".to_owned()]);
        assert_eq!(icon.stroke, None);
        // Lucide: stroked shapes on 24.
        let icon = Icon::parse(r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect width="18" height="18" x="3" y="3" rx="2"/><line x1="9" y1="3" x2="9" y2="21"/><circle cx="12" cy="12" r="4"/><polyline points="1 2 3 4"/></svg>"#).unwrap();
        assert_eq!(icon.stroke, Some(2.));
        assert_eq!(icon.paths.len(), 4);
        let surface = ImageSurface::create(cairo::Format::ARgb32, 64, 64).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        icon.draw(&cr, 32., 48., 2.).unwrap();
        assert!(Icon::parse("<svg></svg>").is_err());
    }

    #[test]
    fn tokens_split_packed_numbers() {
        let t = svg_tokens("M1.5-2.25a8,8,0,0,1-11.32.5Z");
        assert_eq!(t[0], ('M', vec![1.5, -2.25]));
        assert_eq!(t[1], ('a', vec![8., 8., 0., 0., 1., -11.32, 0.5]));
        assert_eq!(t[2], ('Z', vec![]));
    }
}
