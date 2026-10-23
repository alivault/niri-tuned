use std::ops::{Mul, MulAssign};
use std::str::FromStr;

use knuffel::errors::DecodeError;
use miette::{miette, IntoDiagnostic as _};
use smithay::backend::renderer::Color32F;

use crate::utils::{Flag, MergeWith};
use crate::FloatOrInt;

pub const DEFAULT_BACKGROUND_COLOR: Color = Color::from_array_unpremul([0.25, 0.25, 0.25, 1.]);
pub const DEFAULT_BACKDROP_COLOR: Color = Color::from_array_unpremul([0.15, 0.15, 0.15, 1.]);

/// RGB color in [0, 1] with unpremultiplied alpha.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn new_unpremul(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_rgba8_unpremul(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self::from_array_unpremul([r, g, b, a].map(|x| x as f32 / 255.))
    }

    pub fn from_array_premul([r, g, b, a]: [f32; 4]) -> Self {
        let a = a.clamp(0., 1.);

        if a == 0. {
            Self::new_unpremul(0., 0., 0., 0.)
        } else {
            Self {
                r: (r / a).clamp(0., 1.),
                g: (g / a).clamp(0., 1.),
                b: (b / a).clamp(0., 1.),
                a,
            }
        }
    }

    pub const fn from_array_unpremul([r, g, b, a]: [f32; 4]) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_color32f(color: Color32F) -> Self {
        Self::from_array_premul(color.components())
    }

    pub fn to_array_unpremul(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a]
    }

    pub fn to_array_premul(self) -> [f32; 4] {
        let [r, g, b, a] = [self.r, self.g, self.b, self.a];
        [r * a, g * a, b * a, a]
    }
}

impl Mul<f32> for Color {
    type Output = Self;

    fn mul(mut self, rhs: f32) -> Self::Output {
        self.a *= rhs;
        self
    }
}

impl MulAssign<f32> for Color {
    fn mul_assign(&mut self, rhs: f32) {
        self.a *= rhs;
    }
}

impl From<Color> for Color32F {
    fn from(value: Color) -> Self {
        Color32F::from(value.to_array_premul())
    }
}

#[derive(knuffel::Decode, Debug, Clone, Copy, PartialEq)]
pub struct Gradient {
    #[knuffel(property, str)]
    pub from: Color,
    #[knuffel(property, str)]
    pub to: Color,
    #[knuffel(property, default = 180)]
    pub angle: i16,
    #[knuffel(property, default)]
    pub relative_to: GradientRelativeTo,
    #[knuffel(property(name = "in"), str, default)]
    pub in_: GradientInterpolation,
}

impl From<Color> for Gradient {
    fn from(value: Color) -> Self {
        Self {
            from: value,
            to: value,
            angle: 0,
            relative_to: GradientRelativeTo::Window,
            in_: GradientInterpolation::default(),
        }
    }
}

#[derive(knuffel::DecodeScalar, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum GradientRelativeTo {
    #[default]
    Window,
    WorkspaceView,
}

#[derive(Default, Debug, Clone, Copy, PartialEq)]
pub struct GradientInterpolation {
    pub color_space: GradientColorSpace,
    pub hue_interpolation: HueInterpolation,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum GradientColorSpace {
    #[default]
    Srgb,
    SrgbLinear,
    Oklab,
    Oklch,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum HueInterpolation {
    #[default]
    Shorter,
    Longer,
    Increasing,
    Decreasing,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct CornerRadius {
    pub top_left: f32,
    pub top_right: f32,
    pub bottom_right: f32,
    pub bottom_left: f32,
}

impl From<CornerRadius> for [f32; 4] {
    fn from(value: CornerRadius) -> Self {
        [
            value.top_left,
            value.top_right,
            value.bottom_right,
            value.bottom_left,
        ]
    }
}

impl From<f32> for CornerRadius {
    fn from(value: f32) -> Self {
        Self {
            top_left: value,
            top_right: value,
            bottom_right: value,
            bottom_left: value,
        }
    }
}

impl CornerRadius {
    pub fn fit_to(self, width: f32, height: f32) -> Self {
        // Like in CSS: https://drafts.csswg.org/css-backgrounds/#corner-overlap
        let reduction = f32::min(
            f32::min(
                width / (self.top_left + self.top_right),
                width / (self.bottom_left + self.bottom_right),
            ),
            f32::min(
                height / (self.top_left + self.bottom_left),
                height / (self.top_right + self.bottom_right),
            ),
        );
        let reduction = f32::min(1., reduction);

        Self {
            top_left: self.top_left * reduction,
            top_right: self.top_right * reduction,
            bottom_right: self.bottom_right * reduction,
            bottom_left: self.bottom_left * reduction,
        }
    }

    pub fn expanded_by(mut self, width: f32) -> Self {
        // Radius = 0 is preserved, so that square corners remain square.
        if self.top_left > 0. {
            self.top_left += width;
        }
        if self.top_right > 0. {
            self.top_right += width;
        }
        if self.bottom_right > 0. {
            self.bottom_right += width;
        }
        if self.bottom_left > 0. {
            self.bottom_left += width;
        }

        if width < 0. {
            self.top_left = self.top_left.max(0.);
            self.top_right = self.top_right.max(0.);
            self.bottom_left = self.bottom_left.max(0.);
            self.bottom_right = self.bottom_right.max(0.);
        }

        self
    }

    pub fn scaled_by(self, scale: f32) -> Self {
        Self {
            top_left: self.top_left * scale,
            top_right: self.top_right * scale,
            bottom_right: self.bottom_right * scale,
            bottom_left: self.bottom_left * scale,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusRing {
    pub off: bool,
    pub width: f64,
    pub active_color: Color,
    pub inactive_color: Color,
    pub urgent_color: Color,
    pub active_gradient: Option<Gradient>,
    pub inactive_gradient: Option<Gradient>,
    pub urgent_gradient: Option<Gradient>,
}

impl Default for FocusRing {
    fn default() -> Self {
        Self {
            off: false,
            width: 4.,
            active_color: Color::from_rgba8_unpremul(127, 200, 255, 255),
            inactive_color: Color::from_rgba8_unpremul(80, 80, 80, 255),
            urgent_color: Color::from_rgba8_unpremul(155, 0, 0, 255),
            active_gradient: None,
            inactive_gradient: None,
            urgent_gradient: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Border {
    pub off: bool,
    pub width: f64,
    pub active_color: Color,
    pub inactive_color: Color,
    pub urgent_color: Color,
    pub active_gradient: Option<Gradient>,
    pub inactive_gradient: Option<Gradient>,
    pub urgent_gradient: Option<Gradient>,
}

impl Default for Border {
    fn default() -> Self {
        Self {
            off: true,
            width: 4.,
            active_color: Color::from_rgba8_unpremul(255, 200, 127, 255),
            inactive_color: Color::from_rgba8_unpremul(80, 80, 80, 255),
            urgent_color: Color::from_rgba8_unpremul(155, 0, 0, 255),
            active_gradient: None,
            inactive_gradient: None,
            urgent_gradient: None,
        }
    }
}

impl From<Border> for FocusRing {
    fn from(value: Border) -> Self {
        Self {
            off: value.off,
            width: value.width,
            active_color: value.active_color,
            inactive_color: value.inactive_color,
            urgent_color: value.urgent_color,
            active_gradient: value.active_gradient,
            inactive_gradient: value.inactive_gradient,
            urgent_gradient: value.urgent_gradient,
        }
    }
}

impl From<FocusRing> for Border {
    fn from(value: FocusRing) -> Self {
        Self {
            off: value.off,
            width: value.width,
            active_color: value.active_color,
            inactive_color: value.inactive_color,
            urgent_color: value.urgent_color,
            active_gradient: value.active_gradient,
            inactive_gradient: value.inactive_gradient,
            urgent_gradient: value.urgent_gradient,
        }
    }
}

impl MergeWith<BorderRule> for Border {
    fn merge_with(&mut self, part: &BorderRule) {
        self.off |= part.off;
        if part.on {
            self.off = false;
        }

        merge!((self, part), width);

        merge_color_gradient!(
            (self, part),
            (active_color, active_gradient),
            (inactive_color, inactive_gradient),
            (urgent_color, urgent_gradient),
        );
    }
}

impl MergeWith<BorderRule> for FocusRing {
    fn merge_with(&mut self, part: &BorderRule) {
        let mut x = Border::from(*self);
        x.merge_with(part);
        *self = FocusRing::from(x);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shadow {
    pub on: bool,
    pub offset: ShadowOffset,
    pub softness: f64,
    pub spread: f64,
    pub draw_behind_window: bool,
    pub color: Color,
    pub inactive_color: Option<Color>,
    /// Tuned: windows that aren't active may have a shadow of their own size and
    /// place, not only colour; unset, the active window's.
    pub inactive_offset: Option<ShadowOffset>,
    pub inactive_softness: Option<f64>,
    pub inactive_spread: Option<f64>,
}

impl Shadow {
    /// (softness, spread, offset) for an active or inactive window.
    pub fn geometry(&self, is_active: bool) -> (f64, f64, ShadowOffset) {
        if is_active {
            return (self.softness, self.spread, self.offset);
        }
        (
            self.inactive_softness.unwrap_or(self.softness),
            self.inactive_spread.unwrap_or(self.spread),
            self.inactive_offset.unwrap_or(self.offset),
        )
    }
}

impl Default for Shadow {
    fn default() -> Self {
        Self {
            on: false,
            offset: ShadowOffset {
                x: FloatOrInt(0.),
                y: FloatOrInt(5.),
            },
            softness: 30.,
            spread: 5.,
            draw_behind_window: false,
            color: Color::from_rgba8_unpremul(0, 0, 0, 0x77),
            inactive_color: None,
            inactive_offset: None,
            inactive_softness: None,
            inactive_spread: None,
        }
    }
}

impl MergeWith<ShadowRule> for Shadow {
    fn merge_with(&mut self, part: &ShadowRule) {
        self.on |= part.on;
        if part.off {
            self.on = false;
        }

        merge!((self, part), softness, spread);

        merge_clone!((self, part), offset, draw_behind_window, color);

        merge_clone_opt!((self, part), inactive_color, inactive_offset);
        if let Some(x) = part.inactive_softness {
            self.inactive_softness = Some(x.0);
        }
        if let Some(x) = part.inactive_spread {
            self.inactive_spread = Some(x.0);
        }
    }
}

#[derive(knuffel::Decode, Debug, Clone, Copy, PartialEq)]
pub struct ShadowOffset {
    #[knuffel(property, default)]
    pub x: FloatOrInt<-65535, 65535>,
    #[knuffel(property, default)]
    pub y: FloatOrInt<-65535, 65535>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkspaceShadow {
    pub off: bool,
    pub offset: ShadowOffset,
    pub softness: f64,
    pub spread: f64,
    pub color: Color,
}

impl Default for WorkspaceShadow {
    fn default() -> Self {
        Self {
            off: false,
            offset: ShadowOffset {
                x: FloatOrInt(0.),
                y: FloatOrInt(10.),
            },
            softness: 40.,
            spread: 10.,
            color: Color::from_rgba8_unpremul(0, 0, 0, 0x50),
        }
    }
}

impl From<WorkspaceShadow> for Shadow {
    fn from(value: WorkspaceShadow) -> Self {
        Self {
            on: !value.off,
            offset: value.offset,
            softness: value.softness,
            spread: value.spread,
            draw_behind_window: false,
            color: value.color,
            inactive_color: None,
            inactive_offset: None,
            inactive_softness: None,
            inactive_spread: None,
        }
    }
}

/// The overview's backdrop blur: grows with the overview's progress (the open and close
/// animations and the touchpad gesture), from sharp to `radius` once fully open.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverviewBackdropBlur {
    pub off: bool,
    /// Logical pixels at full strength.
    pub radius: f64,
    pub noise: f64,
    pub saturation: f64,
}

impl Default for OverviewBackdropBlur {
    fn default() -> Self {
        Self {
            off: true,
            radius: 32.,
            noise: 0.02,
            saturation: 1.5,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct OverviewBackdropBlurPart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(argument))]
    pub radius: Option<FloatOrInt<0, 1024>>,
    #[knuffel(child, unwrap(argument))]
    pub noise: Option<FloatOrInt<0, 1000>>,
    #[knuffel(child, unwrap(argument))]
    pub saturation: Option<FloatOrInt<0, 1000>>,
}

impl MergeWith<OverviewBackdropBlurPart> for OverviewBackdropBlur {
    fn merge_with(&mut self, part: &OverviewBackdropBlurPart) {
        self.off |= part.off;
        if part.on {
            self.off = false;
        }

        merge!((self, part), radius, noise, saturation);
    }
}

/// Each workspace's area in the overview: a fill inside it and a border outside it, faded
/// in and out with the overview (never drawn outside it, as in a workspace switch). Square,
/// as the workspace, its shadow and windows at its edge are.
///
/// Each colour and its opacity are separate settings, so a desktop's theme can give the
/// colours and its user change an opacity alone (or the other way round): a colour's own
/// alpha is ignored, the opacity is the only transparency.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkspaceHighlight {
    pub off: bool,
    /// Screen pixels, whatever the overview zoom; 0: no border.
    pub border_width: f64,
    pub fill_color: Color,
    pub fill_opacity: f64,
    pub active_border_color: Color,
    pub active_border_opacity: f64,
    pub inactive_border_color: Color,
    pub inactive_border_opacity: f64,
}

impl WorkspaceHighlight {
    /// The colours as drawn: each colour's RGB at its opacity.
    pub fn fill(&self) -> Color {
        with_opacity(self.fill_color, self.fill_opacity)
    }

    pub fn active_border(&self) -> Color {
        with_opacity(self.active_border_color, self.active_border_opacity)
    }

    pub fn inactive_border(&self) -> Color {
        with_opacity(self.inactive_border_color, self.inactive_border_opacity)
    }
}

fn with_opacity(color: Color, opacity: f64) -> Color {
    Color {
        a: opacity.clamp(0., 1.) as f32,
        ..color
    }
}

impl Default for WorkspaceHighlight {
    fn default() -> Self {
        Self {
            off: true,
            border_width: 1.,
            fill_color: Color::from_rgba8_unpremul(255, 255, 255, 255),
            fill_opacity: 0.06,
            active_border_color: Color::from_rgba8_unpremul(255, 255, 255, 255),
            active_border_opacity: 0.5,
            inactive_border_color: Color::from_rgba8_unpremul(255, 255, 255, 255),
            inactive_border_opacity: 0.19,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct WorkspaceHighlightPart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(argument))]
    pub border_width: Option<FloatOrInt<0, 64>>,
    #[knuffel(child)]
    pub fill_color: Option<Color>,
    #[knuffel(child, unwrap(argument))]
    pub fill_opacity: Option<FloatOrInt<0, 1>>,
    #[knuffel(child)]
    pub active_border_color: Option<Color>,
    #[knuffel(child, unwrap(argument))]
    pub active_border_opacity: Option<FloatOrInt<0, 1>>,
    #[knuffel(child)]
    pub inactive_border_color: Option<Color>,
    #[knuffel(child, unwrap(argument))]
    pub inactive_border_opacity: Option<FloatOrInt<0, 1>>,
}

impl MergeWith<WorkspaceHighlightPart> for WorkspaceHighlight {
    fn merge_with(&mut self, part: &WorkspaceHighlightPart) {
        self.off |= part.off;
        if part.on {
            self.off = false;
        }

        merge!(
            (self, part),
            border_width,
            fill_opacity,
            active_border_opacity,
            inactive_border_opacity
        );
        merge_clone!(
            (self, part),
            fill_color,
            active_border_color,
            inactive_border_color
        );
    }
}

#[derive(knuffel::Decode, Debug, Clone, Copy, PartialEq)]
pub struct WorkspaceShadowPart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child)]
    pub offset: Option<ShadowOffset>,
    #[knuffel(child, unwrap(argument))]
    pub softness: Option<FloatOrInt<0, 1024>>,
    #[knuffel(child, unwrap(argument))]
    pub spread: Option<FloatOrInt<-1024, 1024>>,
    #[knuffel(child)]
    pub color: Option<Color>,
}

impl MergeWith<WorkspaceShadowPart> for WorkspaceShadow {
    fn merge_with(&mut self, part: &WorkspaceShadowPart) {
        self.off |= part.off;
        if part.on {
            self.off = false;
        }

        merge_clone!((self, part), offset, color);
        merge!((self, part), softness, spread);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabIndicator {
    pub off: bool,
    pub hide_when_single_tab: bool,
    pub place_within_column: bool,
    pub gap: f64,
    pub width: f64,
    pub length: TabIndicatorLength,
    pub position: TabIndicatorPosition,
    pub gaps_between_tabs: f64,
    pub corner_radius: f64,
    pub active_color: Option<Color>,
    pub inactive_color: Option<Color>,
    pub urgent_color: Option<Color>,
    pub active_gradient: Option<Gradient>,
    pub inactive_gradient: Option<Gradient>,
    pub urgent_gradient: Option<Gradient>,
}

impl Default for TabIndicator {
    fn default() -> Self {
        Self {
            off: false,
            hide_when_single_tab: false,
            place_within_column: false,
            gap: 5.,
            width: 4.,
            length: TabIndicatorLength {
                total_proportion: Some(0.5),
            },
            position: TabIndicatorPosition::Left,
            gaps_between_tabs: 0.,
            corner_radius: 0.,
            active_color: None,
            inactive_color: None,
            urgent_color: None,
            active_gradient: None,
            inactive_gradient: None,
            urgent_gradient: None,
        }
    }
}

impl MergeWith<TabIndicatorPart> for TabIndicator {
    fn merge_with(&mut self, part: &TabIndicatorPart) {
        self.off |= part.off;
        if part.on {
            self.off = false;
        }

        merge!(
            (self, part),
            hide_when_single_tab,
            place_within_column,
            gap,
            width,
            gaps_between_tabs,
            corner_radius,
        );

        merge_clone!((self, part), length, position);

        merge_color_gradient_opt!(
            (self, part),
            (active_color, active_gradient),
            (inactive_color, inactive_gradient),
            (urgent_color, urgent_gradient),
        );
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct TabIndicatorPart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child)]
    pub hide_when_single_tab: Option<Flag>,
    #[knuffel(child)]
    pub place_within_column: Option<Flag>,
    #[knuffel(child, unwrap(argument))]
    pub gap: Option<FloatOrInt<-65535, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub width: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child)]
    pub length: Option<TabIndicatorLength>,
    #[knuffel(child, unwrap(argument))]
    pub position: Option<TabIndicatorPosition>,
    #[knuffel(child, unwrap(argument))]
    pub gaps_between_tabs: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub corner_radius: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child)]
    pub active_color: Option<Color>,
    #[knuffel(child)]
    pub inactive_color: Option<Color>,
    #[knuffel(child)]
    pub urgent_color: Option<Color>,
    #[knuffel(child)]
    pub active_gradient: Option<Gradient>,
    #[knuffel(child)]
    pub inactive_gradient: Option<Gradient>,
    #[knuffel(child)]
    pub urgent_gradient: Option<Gradient>,
}

#[derive(knuffel::Decode, Debug, Clone, Copy, PartialEq)]
pub struct TabIndicatorLength {
    #[knuffel(property)]
    pub total_proportion: Option<f64>,
}

#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq)]
pub enum TabIndicatorPosition {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InsertHint {
    pub off: bool,
    pub color: Color,
    pub gradient: Option<Gradient>,
}

impl Default for InsertHint {
    fn default() -> Self {
        Self {
            off: false,
            color: Color::from_rgba8_unpremul(127, 200, 255, 128),
            gradient: None,
        }
    }
}

impl MergeWith<InsertHintPart> for InsertHint {
    fn merge_with(&mut self, part: &InsertHintPart) {
        self.off |= part.off;
        if part.on {
            self.off = false;
        }

        merge_color_gradient!((self, part), (color, gradient));
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct InsertHintPart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child)]
    pub color: Option<Color>,
    #[knuffel(child)]
    pub gradient: Option<Gradient>,
}

#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockOutFrom {
    Screencast,
    ScreenCapture,
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct BorderRule {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(argument))]
    pub width: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child)]
    pub active_color: Option<Color>,
    #[knuffel(child)]
    pub inactive_color: Option<Color>,
    #[knuffel(child)]
    pub urgent_color: Option<Color>,
    #[knuffel(child)]
    pub active_gradient: Option<Gradient>,
    #[knuffel(child)]
    pub inactive_gradient: Option<Gradient>,
    #[knuffel(child)]
    pub urgent_gradient: Option<Gradient>,
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct ShadowRule {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child)]
    pub offset: Option<ShadowOffset>,
    #[knuffel(child, unwrap(argument))]
    pub softness: Option<FloatOrInt<0, 1024>>,
    #[knuffel(child, unwrap(argument))]
    pub spread: Option<FloatOrInt<-1024, 1024>>,
    #[knuffel(child, unwrap(argument))]
    pub draw_behind_window: Option<bool>,
    #[knuffel(child)]
    pub color: Option<Color>,
    #[knuffel(child)]
    pub inactive_color: Option<Color>,
    #[knuffel(child)]
    pub inactive_offset: Option<ShadowOffset>,
    #[knuffel(child, unwrap(argument))]
    pub inactive_softness: Option<FloatOrInt<0, 1024>>,
    #[knuffel(child, unwrap(argument))]
    pub inactive_spread: Option<FloatOrInt<-1024, 1024>>,
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct TabIndicatorRule {
    #[knuffel(child)]
    pub active_color: Option<Color>,
    #[knuffel(child)]
    pub inactive_color: Option<Color>,
    #[knuffel(child)]
    pub urgent_color: Option<Color>,
    #[knuffel(child)]
    pub active_gradient: Option<Gradient>,
    #[knuffel(child)]
    pub inactive_gradient: Option<Gradient>,
    #[knuffel(child)]
    pub urgent_gradient: Option<Gradient>,
}

impl MergeWith<Self> for BorderRule {
    fn merge_with(&mut self, part: &Self) {
        merge_on_off!((self, part));

        merge_clone_opt!((self, part), width);

        merge_color_gradient_opt!(
            (self, part),
            (active_color, active_gradient),
            (inactive_color, inactive_gradient),
            (urgent_color, urgent_gradient),
        );
    }
}

impl MergeWith<Self> for ShadowRule {
    fn merge_with(&mut self, part: &Self) {
        merge_on_off!((self, part));

        merge_clone_opt!(
            (self, part),
            offset,
            softness,
            spread,
            draw_behind_window,
            color,
            inactive_color,
        );
    }
}

impl MergeWith<Self> for TabIndicatorRule {
    fn merge_with(&mut self, part: &Self) {
        merge_color_gradient_opt!(
            (self, part),
            (active_color, active_gradient),
            (inactive_color, inactive_gradient),
            (urgent_color, urgent_gradient),
        );
    }
}

impl FromStr for GradientInterpolation {
    type Err = miette::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut iter = s.split_whitespace();
        let in_part1 = iter.next();
        let in_part2 = iter.next();
        let in_part3 = iter.next();

        let Some(in_part1) = in_part1 else {
            return Err(miette!("missing color space"));
        };

        let color = match in_part1 {
            "srgb" => GradientColorSpace::Srgb,
            "srgb-linear" => GradientColorSpace::SrgbLinear,
            "oklab" => GradientColorSpace::Oklab,
            "oklch" => GradientColorSpace::Oklch,
            x => {
                return Err(miette!(
                    "invalid color space {x}; can be srgb, srgb-linear, oklab or oklch"
                ))
            }
        };

        let interpolation = if let Some(in_part2) = in_part2 {
            if color != GradientColorSpace::Oklch {
                return Err(miette!("only oklch color space can have hue interpolation"));
            }

            if in_part3 != Some("hue") {
                return Err(miette!(
                    "interpolation must end with \"hue\", like \"oklch shorter hue\""
                ));
            } else if iter.next().is_some() {
                return Err(miette!("unexpected text after hue interpolation"));
            } else {
                match in_part2 {
                    "shorter" => HueInterpolation::Shorter,
                    "longer" => HueInterpolation::Longer,
                    "increasing" => HueInterpolation::Increasing,
                    "decreasing" => HueInterpolation::Decreasing,
                    x => {
                        return Err(miette!(
                            "invalid hue interpolation {x}; \
                             can be shorter, longer, increasing, decreasing"
                        ))
                    }
                }
            }
        } else {
            HueInterpolation::default()
        };

        Ok(Self {
            color_space: color,
            hue_interpolation: interpolation,
        })
    }
}

impl FromStr for Color {
    type Err = miette::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let color = csscolorparser::parse(s)
            .into_diagnostic()?
            .clamp()
            .to_array();
        Ok(Self::from_array_unpremul(color))
    }
}

#[derive(knuffel::Decode)]
struct ColorRgba {
    #[knuffel(argument)]
    r: u8,
    #[knuffel(argument)]
    g: u8,
    #[knuffel(argument)]
    b: u8,
    #[knuffel(argument)]
    a: u8,
}

impl From<ColorRgba> for Color {
    fn from(value: ColorRgba) -> Self {
        let ColorRgba { r, g, b, a } = value;
        Self::from_array_unpremul([r, g, b, a].map(|x| x as f32 / 255.))
    }
}

// Manual impl to allow both one-argument string and 4-argument RGBA forms.
impl<S> knuffel::Decode<S> for Color
where
    S: knuffel::traits::ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        // Check for unexpected type name.
        if let Some(type_name) = &node.type_name {
            ctx.emit_error(DecodeError::unexpected(
                type_name,
                "type name",
                "no type name expected for this node",
            ));
        }

        // Get the first argument.
        let mut iter_args = node.arguments.iter();
        let val = iter_args
            .next()
            .ok_or_else(|| DecodeError::missing(node, "additional argument is required"))?;

        // Check for unexpected type name.
        if let Some(typ) = &val.type_name {
            ctx.emit_error(DecodeError::TypeName {
                span: typ.span().clone(),
                found: Some((**typ).clone()),
                expected: knuffel::errors::ExpectedType::no_type(),
                rust_type: "str",
            });
        }

        // Check the argument type.
        let rv = match *val.literal {
            // If it's a string, use FromStr.
            knuffel::ast::Literal::String(ref s) => {
                Color::from_str(s).map_err(|e| DecodeError::conversion(&val.literal, e))
            }
            // Otherwise, fall back to the 4-argument RGBA form.
            _ => return ColorRgba::decode_node(node, ctx).map(Color::from),
        }?;

        // Check for unexpected following arguments.
        if let Some(val) = iter_args.next() {
            ctx.emit_error(DecodeError::unexpected(
                &val.literal,
                "argument",
                "unexpected argument",
            ));
        }

        // Check for unexpected properties and children.
        for name in node.properties.keys() {
            ctx.emit_error(DecodeError::unexpected(
                name,
                "property",
                format!("unexpected property `{}`", name.escape_default()),
            ));
        }
        for child in node.children.as_ref().map(|lst| &lst[..]).unwrap_or(&[]) {
            ctx.emit_error(DecodeError::unexpected(
                child,
                "node",
                format!("unexpected node `{}`", child.node_name.escape_default()),
            ));
        }

        Ok(rv)
    }
}

impl<S> knuffel::Decode<S> for CornerRadius
where
    S: knuffel::traits::ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        // Check for unexpected type name.
        if let Some(type_name) = &node.type_name {
            ctx.emit_error(DecodeError::unexpected(
                type_name,
                "type name",
                "no type name expected for this node",
            ));
        }

        let decode_radius = |ctx: &mut knuffel::decode::Context<S>,
                             val: &knuffel::ast::Value<S>| {
            // Check for unexpected type name.
            if let Some(typ) = &val.type_name {
                ctx.emit_error(DecodeError::TypeName {
                    span: typ.span().clone(),
                    found: Some((**typ).clone()),
                    expected: knuffel::errors::ExpectedType::no_type(),
                    rust_type: "str",
                });
            }

            // Decode both integers and floats.
            let radius = match *val.literal {
                knuffel::ast::Literal::Int(ref x) => f32::from(match x.try_into() {
                    Ok(x) => x,
                    Err(err) => {
                        ctx.emit_error(DecodeError::conversion(&val.literal, err));
                        0i16
                    }
                }),
                knuffel::ast::Literal::Decimal(ref x) => match x.try_into() {
                    Ok(x) => x,
                    Err(err) => {
                        ctx.emit_error(DecodeError::conversion(&val.literal, err));
                        0.
                    }
                },
                _ => {
                    ctx.emit_error(DecodeError::scalar_kind(
                        knuffel::decode::Kind::Int,
                        &val.literal,
                    ));
                    0.
                }
            };

            if radius < 0. {
                ctx.emit_error(DecodeError::conversion(&val.literal, "radius must be >= 0"));
            }

            radius
        };

        // Get the first argument.
        let mut iter_args = node.arguments.iter();
        let val = iter_args
            .next()
            .ok_or_else(|| DecodeError::missing(node, "additional argument is required"))?;

        let top_left = decode_radius(ctx, val);

        let mut rv = CornerRadius {
            top_left,
            top_right: top_left,
            bottom_right: top_left,
            bottom_left: top_left,
        };

        if let Some(val) = iter_args.next() {
            rv.top_right = decode_radius(ctx, val);

            let val = iter_args.next().ok_or_else(|| {
                DecodeError::missing(node, "either 1 or 4 arguments are required")
            })?;
            rv.bottom_right = decode_radius(ctx, val);

            let val = iter_args.next().ok_or_else(|| {
                DecodeError::missing(node, "either 1 or 4 arguments are required")
            })?;
            rv.bottom_left = decode_radius(ctx, val);

            // Check for unexpected following arguments.
            if let Some(val) = iter_args.next() {
                ctx.emit_error(DecodeError::unexpected(
                    &val.literal,
                    "argument",
                    "unexpected argument",
                ));
            }
        }

        // Check for unexpected properties and children.
        for name in node.properties.keys() {
            ctx.emit_error(DecodeError::unexpected(
                name,
                "property",
                format!("unexpected property `{}`", name.escape_default()),
            ));
        }
        for child in node.children.as_ref().map(|lst| &lst[..]).unwrap_or(&[]) {
            ctx.emit_error(DecodeError::unexpected(
                child,
                "node",
                format!("unexpected node `{}`", child.node_name.escape_default()),
            ));
        }

        Ok(rv)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Blur {
    pub off: bool,
    pub passes: u8,
    pub offset: f64,
    /// How far the blur spreads, in logical pixels (as the overview's backdrop blur). Set,
    /// it replaces `passes` and `offset`: they are worked out from it at each output's
    /// scale (spread ≈ offset × 2^passes). 0: no blur, as `off`.
    pub radius: Option<f64>,
    pub noise: f64,
    pub saturation: f64,
}

impl Default for Blur {
    fn default() -> Self {
        Self {
            off: false,
            passes: 3,
            offset: 3.,
            radius: None,
            noise: 0.02,
            saturation: 1.5,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct BlurPart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(argument))]
    pub passes: Option<u8>,
    #[knuffel(child, unwrap(argument))]
    pub offset: Option<FloatOrInt<0, 100>>,
    #[knuffel(child, unwrap(argument))]
    pub radius: Option<FloatOrInt<0, 1024>>,
    #[knuffel(child, unwrap(argument))]
    pub noise: Option<FloatOrInt<0, 1000>>,
    #[knuffel(child, unwrap(argument))]
    pub saturation: Option<FloatOrInt<0, 1000>>,
}

impl Blur {
    /// No blur at all: `off`, or a `radius` of 0.
    pub fn is_off(&self) -> bool {
        self.off || self.radius.is_some_and(|radius| radius <= 0.)
    }
}

impl MergeWith<BlurPart> for Blur {
    fn merge_with(&mut self, part: &BlurPart) {
        self.off |= part.off;
        if part.on {
            self.off = false;
        }

        merge_clone!((self, part), passes);
        merge!((self, part), offset, noise, saturation);

        // The strength set last wins: a radius, or passes and offset over an earlier
        // layer's radius.
        if let Some(radius) = part.radius {
            self.radius = Some(radius.0);
        } else if part.passes.is_some() || part.offset.is_some() {
            self.radius = None;
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct BackgroundEffectRule {
    #[knuffel(child, unwrap(argument))]
    pub xray: Option<bool>,
    #[knuffel(child, unwrap(argument))]
    pub blur: Option<bool>,
    #[knuffel(child, unwrap(argument))]
    pub noise: Option<FloatOrInt<0, 1000>>,
    #[knuffel(child, unwrap(argument))]
    pub saturation: Option<FloatOrInt<0, 1000>>,
}

/// Resolved background effect rule.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct BackgroundEffect {
    /// Whether to render with xray effect (see through).
    ///
    /// - `None`: xray if any background effect is active
    /// - `Some(false)`: no xray
    /// - `Some(true)`: xray even if no other background effect is active
    pub xray: Option<bool>,

    /// Whether to blur the background.
    ///
    /// - `None`: blur when the window/layer requests it (e.g. through ext-background-effect
    ///   protocol)
    /// - `Some(false)`: never blur
    /// - `Some(true)`: always blur
    pub blur: Option<bool>,

    pub noise: Option<f64>,
    pub saturation: Option<f64>,
}

impl MergeWith<BackgroundEffectRule> for BackgroundEffect {
    fn merge_with(&mut self, part: &BackgroundEffectRule) {
        merge_clone_opt!((self, part), xray, blur);

        if let Some(x) = part.noise {
            self.noise = Some(x.0);
        }

        if let Some(x) = part.saturation {
            self.saturation = Some(x.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use insta::{assert_debug_snapshot, assert_snapshot};

    use super::*;
    use crate::Config;

    #[test]
    fn parse_gradient_interpolation() {
        assert_eq!(
            "srgb".parse::<GradientInterpolation>().unwrap(),
            GradientInterpolation {
                color_space: GradientColorSpace::Srgb,
                ..Default::default()
            }
        );
        assert_eq!(
            "srgb-linear".parse::<GradientInterpolation>().unwrap(),
            GradientInterpolation {
                color_space: GradientColorSpace::SrgbLinear,
                ..Default::default()
            }
        );
        assert_eq!(
            "oklab".parse::<GradientInterpolation>().unwrap(),
            GradientInterpolation {
                color_space: GradientColorSpace::Oklab,
                ..Default::default()
            }
        );
        assert_eq!(
            "oklch".parse::<GradientInterpolation>().unwrap(),
            GradientInterpolation {
                color_space: GradientColorSpace::Oklch,
                ..Default::default()
            }
        );
        assert_eq!(
            "oklch shorter hue"
                .parse::<GradientInterpolation>()
                .unwrap(),
            GradientInterpolation {
                color_space: GradientColorSpace::Oklch,
                hue_interpolation: HueInterpolation::Shorter,
            }
        );
        assert_eq!(
            "oklch longer hue".parse::<GradientInterpolation>().unwrap(),
            GradientInterpolation {
                color_space: GradientColorSpace::Oklch,
                hue_interpolation: HueInterpolation::Longer,
            }
        );
        assert_eq!(
            "oklch decreasing hue"
                .parse::<GradientInterpolation>()
                .unwrap(),
            GradientInterpolation {
                color_space: GradientColorSpace::Oklch,
                hue_interpolation: HueInterpolation::Decreasing,
            }
        );
        assert_eq!(
            "oklch increasing hue"
                .parse::<GradientInterpolation>()
                .unwrap(),
            GradientInterpolation {
                color_space: GradientColorSpace::Oklch,
                hue_interpolation: HueInterpolation::Increasing,
            }
        );

        assert!("".parse::<GradientInterpolation>().is_err());
        assert!("srgb shorter hue".parse::<GradientInterpolation>().is_err());
        assert!("oklch shorter".parse::<GradientInterpolation>().is_err());
        assert!("oklch shorter h".parse::<GradientInterpolation>().is_err());
        assert!("oklch a hue".parse::<GradientInterpolation>().is_err());
        assert!("oklch shorter hue a"
            .parse::<GradientInterpolation>()
            .is_err());
    }

    #[test]
    fn test_border_rule_on_off_merging() {
        fn is_on(config: &str, rules: &[&str]) -> String {
            let mut resolved = Border {
                off: config == "off",
                ..Default::default()
            };

            for rule in rules.iter().copied() {
                let rule = BorderRule {
                    off: rule == "off" || rule == "off,on",
                    on: rule == "on" || rule == "off,on",
                    ..Default::default()
                };

                resolved.merge_with(&rule);
            }

            if resolved.off { "off" } else { "on" }.to_owned()
        }

        assert_snapshot!(is_on("off", &[]), @"off");
        assert_snapshot!(is_on("off", &["off"]), @"off");
        assert_snapshot!(is_on("off", &["on"]), @"on");
        assert_snapshot!(is_on("off", &["off,on"]), @"on");

        assert_snapshot!(is_on("on", &[]), @"on");
        assert_snapshot!(is_on("on", &["off"]), @"off");
        assert_snapshot!(is_on("on", &["on"]), @"on");
        assert_snapshot!(is_on("on", &["off,on"]), @"on");

        assert_snapshot!(is_on("off", &["off", "off"]), @"off");
        assert_snapshot!(is_on("off", &["off", "on"]), @"on");
        assert_snapshot!(is_on("off", &["on", "off"]), @"off");
        assert_snapshot!(is_on("off", &["on", "on"]), @"on");

        assert_snapshot!(is_on("on", &["off", "off"]), @"off");
        assert_snapshot!(is_on("on", &["off", "on"]), @"on");
        assert_snapshot!(is_on("on", &["on", "off"]), @"off");
        assert_snapshot!(is_on("on", &["on", "on"]), @"on");
    }

    #[test]
    fn rule_color_can_override_base_gradient() {
        let config = Config::parse_mem(
            r##"
            // Start with gradient set.
            layout {
                border {
                    active-gradient from="#101010" to="#202020"
                    inactive-gradient from="#111111" to="#212121"
                    urgent-gradient from="#121212" to="#222222"
                }
            }

            // Override with color.
            window-rule {
                border {
                    active-color "#abcdef"
                    inactive-color "#123456"
                    urgent-color "#fedcba"
                }
            }
            "##,
        )
        .unwrap();

        let mut border = config.layout.border;
        for rule in &config.window_rules {
            border.merge_with(&rule.border);
        }

        // Gradient should be None because it's overwritten.
        assert_debug_snapshot!(
            (
                border.active_gradient.is_some(),
                border.inactive_gradient.is_some(),
                border.urgent_gradient.is_some(),
            ),
            @r"
        (
            false,
            false,
            false,
        )
        "
        );
    }

    #[test]
    fn rule_color_can_override_rule_gradient() {
        let config = Config::parse_mem(
            r##"
            // Start with gradient set.
            layout {
                border {
                    active-gradient from="#101010" to="#202020"
                    inactive-gradient from="#111111" to="#212121"
                    urgent-gradient from="#121212" to="#222222"
                }
            }

            // Window rule with gradients set.
            window-rule {
                border {
                    active-gradient from="#303030" to="#404040"
                    inactive-gradient from="#313131" to="#414141"
                    urgent-gradient from="#323232" to="#424242"
                }

                tab-indicator {
                    active-gradient from="#505050" to="#606060"
                    inactive-gradient from="#515151" to="#616161"
                    urgent-gradient from="#525252" to="#626262"
                }
            }

            // Override with color.
            window-rule {
                border {
                    active-color "#abcdef"
                    inactive-color "#123456"
                    urgent-color "#fedcba"
                }

                tab-indicator {
                    active-color "#abcdef"
                    inactive-color "#123456"
                    urgent-color "#fedcba"
                }
            }
            "##,
        )
        .unwrap();

        let mut border = config.layout.border;
        let mut tab_indicator_rule = TabIndicatorRule::default();
        for rule in &config.window_rules {
            border.merge_with(&rule.border);
            tab_indicator_rule.merge_with(&rule.tab_indicator);
        }

        // Gradient should be None because it's overwritten.
        assert_debug_snapshot!(
            (
                border.active_gradient.is_some(),
                border.inactive_gradient.is_some(),
                border.urgent_gradient.is_some(),
                tab_indicator_rule.active_gradient.is_some(),
                tab_indicator_rule.inactive_gradient.is_some(),
                tab_indicator_rule.urgent_gradient.is_some(),
            ),
            @r"
        (
            false,
            false,
            false,
            false,
            false,
            false,
        )
        "
        );
    }
}

/// Server-side title bar drawn by niri above windows that use server-side decorations.
#[derive(Debug, Clone, PartialEq)]
pub struct Titlebar {
    pub off: bool,
    pub height: f64,
    /// Pango font description, e.g. "Inter Medium 13px".
    pub font: String,
    pub active_color: Color,
    pub inactive_color: Color,
    pub active_text_color: Color,
    pub inactive_text_color: Color,
    /// The buttons' circles: on the focused window, an unfocused one (unset: the same) and
    /// under the pointer (unset: lightened by `button_hover_brighten`).
    pub button_color: Color,
    pub button_inactive_color: Option<Color>,
    pub button_hover_color: Option<Color>,
    /// The buttons' icons: on the focused window (unset: `active_text_color`), an unfocused one
    /// (unset: `inactive_text_color`) and under the pointer (unset: as without it).
    pub button_icon_color: Option<Color>,
    pub button_icon_inactive_color: Option<Color>,
    pub button_icon_hover_color: Option<Color>,
    /// Close's red circle (when `close_red` says) and its icon on it.
    pub close_hover_color: Color,
    pub button_hover_text_color: Color,
    /// Each button's own colours, over the ones above.
    pub button_styles: TitlebarButtonStyles,
    /// When the buttons' circles show: always (the default) or on hover.
    pub button_highlight: TitlebarButtonHighlight,
    /// When the close button's circle is red: on hover (the default), on the focused window,
    /// or always.
    pub close_red: TitlebarCloseRed,
    /// Each button's shape: inset in its slot (the default), or the whole slot.
    pub button_shape: TitlebarButtonShape,
    /// Side of each button's square slot, centred on the bar's height; with `button-shape
    /// "full-height"`, the slot's width (it is as tall as the bar).
    pub button_size: f64,
    /// Side of a button's shape (a circle's diameter).
    pub button_circle: f64,
    /// Corner radius of a button's shape, at most half its side; unset: a circle.
    pub button_radius: Option<f64>,
    /// Side of a button's icon (its whole view box).
    pub icon_size: f64,
    /// Gap between the outermost button and the bar's edge.
    pub button_edge_gap: f64,
    /// Extra space between two neighbouring buttons' slots.
    pub button_gap: f64,
    /// Gap between the title and the buttons.
    pub title_gap: f64,
    /// How much hover brightens an always-shown circle (white over it, 0 to 1).
    pub button_hover_brighten: f64,
    pub buttons_position: TitlebarButtonsPosition,
    /// The buttons, from the bar's edge inwards.
    pub buttons: Vec<TitlebarButton>,
    /// The `menu` button's command: spawned on a click with the window's id, its output's name,
    /// the pointer's position on that output (logical x and y) and the window's state
    /// (`floating`, `maximized`, `fullscreen`, comma-separated, or `-`) appended, for a shell to
    /// open the window's menu there. Empty: the button does nothing.
    pub menu_command: Vec<String>,
    /// Run when a view scrolls, switches workspace or the overview opens or closes while a
    /// layer surface named `menu_namespace` is open (a shell menu), so it closes rather than
    /// stay put away from what it belongs to. Empty: nothing.
    pub menu_close_command: Vec<String>,
    /// The layer-shell namespace of the shell's menus, for `menu_close_command`.
    pub menu_namespace: String,
    /// Each button's icon, unset for the built-in one.
    pub icons: TitlebarIcons,
    /// A unified toolbar (macOS's name): the window's content extends under the bar, which
    /// draws only its buttons, for apps that paint their own top area (a sidebar running to the
    /// top, their toolbar in the bar's row). Set with `unified` (or its old name `transparent`).
    pub transparent: bool,
    /// The pointer on any button shows every button's hover icon (macOS: hovering the
    /// traffic lights shows all three glyphs); the circles still follow the one hovered.
    pub button_hover_group: bool,
}

impl Default for Titlebar {
    fn default() -> Self {
        Self {
            off: true,
            height: 30.,
            font: String::from("sans Medium 12px"),
            active_color: Color::from_rgba8_unpremul(0x2c, 0x2c, 0x2e, 0xff),
            inactive_color: Color::from_rgba8_unpremul(0x1c, 0x1c, 0x1e, 0xff),
            active_text_color: Color::from_rgba8_unpremul(0xf2, 0xf2, 0xf7, 0xff),
            inactive_text_color: Color::from_rgba8_unpremul(0x8e, 0x8e, 0x93, 0xff),
            button_color: Color::from_rgba8_unpremul(0xff, 0xff, 0xff, 0x26),
            button_inactive_color: None,
            button_hover_color: None,
            button_icon_color: None,
            button_icon_inactive_color: None,
            button_icon_hover_color: None,
            button_styles: TitlebarButtonStyles::default(),
            close_hover_color: Color::from_rgba8_unpremul(0xff, 0x45, 0x3a, 0xff),
            button_hover_text_color: Color::from_rgba8_unpremul(0xff, 0xff, 0xff, 0xff),
            button_highlight: TitlebarButtonHighlight::Always,
            close_red: TitlebarCloseRed::Hover,
            button_shape: TitlebarButtonShape::Inset,
            button_size: 30.,
            button_circle: 22.,
            button_radius: None,
            icon_size: 15.,
            button_edge_gap: 4.,
            button_gap: 0.,
            title_gap: 8.,
            button_hover_brighten: 0.14,
            buttons_position: TitlebarButtonsPosition::Left,
            buttons: vec![TitlebarButton::Close, TitlebarButton::MaximizeColumn],
            menu_command: Vec::new(),
            menu_close_command: Vec::new(),
            menu_namespace: String::new(),
            icons: TitlebarIcons::default(),
            transparent: false,
            button_hover_group: false,
        }
    }
}

/// Title bar button icons: a built-in Phosphor name (`x`, `square`, `cards`,
/// `square-split-horizontal`, `plus`, `minus`,
/// `arrows-out-simple`, `arrows-in-simple`, `corners-out`, `corners-in`,
/// `arrows-out-line-horizontal`, `dots-three`) or an SVG file (`~/...` or absolute: its `<path>`s,
/// filled, or stroked when the SVG says `fill="none"`), read when the config loads. The `-active`
/// ones are shown while the toggle is on (zoom and maximize: maximized or fullscreen; float:
/// floating; fullscreen: fullscreen), the plain one when unset.
#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct TitlebarIcons {
    #[knuffel(child, unwrap(argument))]
    pub close: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub zoom: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub zoom_active: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub float: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub float_active: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub maximize: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub maximize_active: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub maximize_column: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub fullscreen: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub fullscreen_active: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub menu: Option<String>,
}

impl MergeWith<TitlebarIcons> for TitlebarIcons {
    fn merge_with(&mut self, part: &TitlebarIcons) {
        merge_clone_opt!(
            (self, part),
            close,
            zoom,
            zoom_active,
            float,
            float_active,
            maximize,
            maximize_active,
            maximize_column,
            fullscreen,
            fullscreen_active,
            menu,
        );
    }
}

/// One button's colours, each unset for the title bar's.
#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct TitlebarButtonStyle {
    #[knuffel(child)]
    pub color: Option<Color>,
    #[knuffel(child)]
    pub inactive_color: Option<Color>,
    #[knuffel(child)]
    pub hover_color: Option<Color>,
    #[knuffel(child)]
    pub icon_color: Option<Color>,
    #[knuffel(child)]
    pub icon_inactive_color: Option<Color>,
    #[knuffel(child)]
    pub icon_hover_color: Option<Color>,
}

impl MergeWith<Self> for TitlebarButtonStyle {
    fn merge_with(&mut self, part: &Self) {
        merge_clone_opt!(
            (self, part),
            color,
            inactive_color,
            hover_color,
            icon_color,
            icon_inactive_color,
            icon_hover_color,
        );
    }
}

/// Per-button colours (`button-styles { close { hover-color ...; } }`).
#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct TitlebarButtonStyles {
    #[knuffel(child)]
    pub close: Option<TitlebarButtonStyle>,
    #[knuffel(child)]
    pub zoom: Option<TitlebarButtonStyle>,
    #[knuffel(child)]
    pub float: Option<TitlebarButtonStyle>,
    #[knuffel(child)]
    pub maximize: Option<TitlebarButtonStyle>,
    #[knuffel(child)]
    pub maximize_column: Option<TitlebarButtonStyle>,
    #[knuffel(child)]
    pub fullscreen: Option<TitlebarButtonStyle>,
    #[knuffel(child)]
    pub menu: Option<TitlebarButtonStyle>,
}

impl TitlebarButtonStyles {
    pub fn get(&self, button: TitlebarButton) -> TitlebarButtonStyle {
        match button {
            TitlebarButton::Close => self.close,
            TitlebarButton::Zoom => self.zoom,
            TitlebarButton::Float => self.float,
            TitlebarButton::Maximize => self.maximize,
            TitlebarButton::MaximizeColumn => self.maximize_column,
            TitlebarButton::Fullscreen => self.fullscreen,
            TitlebarButton::Menu => self.menu,
        }
        .unwrap_or_default()
    }
}

impl MergeWith<Self> for TitlebarButtonStyles {
    fn merge_with(&mut self, part: &Self) {
        for (mine, theirs) in [
            (&mut self.close, &part.close),
            (&mut self.zoom, &part.zoom),
            (&mut self.float, &part.float),
            (&mut self.maximize, &part.maximize),
            (&mut self.maximize_column, &part.maximize_column),
            (&mut self.fullscreen, &part.fullscreen),
            (&mut self.menu, &part.menu),
        ] {
            if let Some(x) = theirs {
                mine.get_or_insert_with(Default::default).merge_with(x);
            }
        }
    }
}

/// A title bar button.
#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TitlebarButton {
    Close,
    /// Between floating and maximized: on a floating (or tiled) window it maximizes it into the
    /// columns, on a maximized one it floats it (Alt-click: fullscreen).
    Zoom,
    /// Toggles floating.
    Float,
    /// Toggles maximized to the edges (Alt-click: fullscreen).
    Maximize,
    /// Toggles the column's full width.
    MaximizeColumn,
    Fullscreen,
    /// Opens the window's menu: spawns `menu-command` (see `Titlebar::menu_command`).
    Menu,
}

#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq)]
pub enum TitlebarCloseRed {
    Hover,
    Focused,
    Always,
    /// Never red: close's colours are the other buttons'.
    Never,
}

/// A title bar button's shape.
#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq)]
pub enum TitlebarButtonShape {
    /// A circle or rounded square (`button-circle`, `button-radius`) centred in its slot.
    Inset,
    /// The whole slot, `button-size` wide and as tall as the bar, clipped to the window's
    /// rounded top corners (so the corner button takes the window's corner).
    FullHeight,
}

#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq)]
pub enum TitlebarButtonHighlight {
    Always,
    Hover,
}

#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq)]
pub enum TitlebarButtonsPosition {
    Left,
    Right,
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct TitlebarRule {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(argument))]
    pub height: Option<FloatOrInt<0, 1024>>,
    #[knuffel(child, unwrap(argument))]
    pub font: Option<String>,
    #[knuffel(child)]
    pub active_color: Option<Color>,
    #[knuffel(child)]
    pub inactive_color: Option<Color>,
    #[knuffel(child)]
    pub active_text_color: Option<Color>,
    #[knuffel(child)]
    pub inactive_text_color: Option<Color>,
    #[knuffel(child)]
    pub button_color: Option<Color>,
    #[knuffel(child)]
    pub button_inactive_color: Option<Color>,
    #[knuffel(child)]
    pub button_hover_color: Option<Color>,
    #[knuffel(child)]
    pub button_icon_color: Option<Color>,
    #[knuffel(child)]
    pub button_icon_inactive_color: Option<Color>,
    #[knuffel(child)]
    pub button_icon_hover_color: Option<Color>,
    #[knuffel(child)]
    pub button_styles: Option<TitlebarButtonStyles>,
    #[knuffel(child)]
    pub close_hover_color: Option<Color>,
    #[knuffel(child)]
    pub button_hover_text_color: Option<Color>,
    #[knuffel(child, unwrap(argument))]
    pub button_highlight: Option<TitlebarButtonHighlight>,
    #[knuffel(child, unwrap(argument))]
    pub close_red: Option<TitlebarCloseRed>,
    #[knuffel(child, unwrap(argument))]
    pub button_shape: Option<TitlebarButtonShape>,
    #[knuffel(child, unwrap(argument))]
    pub button_size: Option<FloatOrInt<1, 256>>,
    #[knuffel(child, unwrap(argument))]
    pub button_circle: Option<FloatOrInt<0, 256>>,
    #[knuffel(child, unwrap(argument))]
    pub button_radius: Option<FloatOrInt<0, 256>>,
    #[knuffel(child, unwrap(argument))]
    pub icon_size: Option<FloatOrInt<1, 256>>,
    #[knuffel(child, unwrap(argument))]
    pub button_edge_gap: Option<FloatOrInt<0, 256>>,
    #[knuffel(child, unwrap(argument))]
    pub button_gap: Option<FloatOrInt<0, 256>>,
    #[knuffel(child, unwrap(argument))]
    pub title_gap: Option<FloatOrInt<0, 256>>,
    #[knuffel(child, unwrap(argument))]
    pub button_hover_brighten: Option<FloatOrInt<0, 1>>,
    #[knuffel(child, unwrap(argument))]
    pub buttons_position: Option<TitlebarButtonsPosition>,
    #[knuffel(child, unwrap(arguments))]
    pub buttons: Option<Vec<TitlebarButton>>,
    #[knuffel(child, unwrap(arguments))]
    pub menu_command: Option<Vec<String>>,
    #[knuffel(child, unwrap(arguments))]
    pub menu_close_command: Option<Vec<String>>,
    #[knuffel(child, unwrap(argument))]
    pub menu_namespace: Option<String>,
    #[knuffel(child)]
    pub icons: Option<TitlebarIcons>,
    #[knuffel(child)]
    pub unified: Option<Flag>,
    /// The old name of `unified`.
    #[knuffel(child)]
    pub transparent: Option<Flag>,
    #[knuffel(child)]
    pub button_hover_group: Option<Flag>,
}

impl MergeWith<TitlebarRule> for Titlebar {
    fn merge_with(&mut self, part: &TitlebarRule) {
        self.off |= part.off;
        if part.on {
            self.off = false;
        }

        if let Some(x) = part.unified.or(part.transparent) {
            self.transparent = x.0;
        }
        if let Some(x) = part.button_hover_group {
            self.button_hover_group = x.0;
        }
        if let Some(x) = part.button_radius {
            self.button_radius = Some(x.0);
        }
        merge!(
            (self, part),
            height,
            icons,
            button_size,
            button_circle,
            icon_size,
            button_edge_gap,
            button_gap,
            title_gap,
            button_hover_brighten,
        );

        merge_clone!(
            (self, part),
            font,
            active_color,
            inactive_color,
            active_text_color,
            inactive_text_color,
            button_color,
            close_hover_color,
            button_hover_text_color,
            button_highlight,
            close_red,
            button_shape,
            buttons_position,
            buttons,
            menu_command,
            menu_close_command,
            menu_namespace,
        );

        merge_clone_opt!(
            (self, part),
            button_inactive_color,
            button_hover_color,
            button_icon_color,
            button_icon_inactive_color,
            button_icon_hover_color,
        );

        if let Some(x) = &part.button_styles {
            self.button_styles.merge_with(x);
        }
    }
}

impl MergeWith<Self> for TitlebarRule {
    fn merge_with(&mut self, part: &Self) {
        merge_on_off!((self, part));

        merge_clone_opt!(
            (self, part),
            height,
            font,
            active_color,
            inactive_color,
            active_text_color,
            inactive_text_color,
            button_color,
            button_inactive_color,
            button_hover_color,
            button_icon_color,
            button_icon_inactive_color,
            button_icon_hover_color,
            close_hover_color,
            button_hover_text_color,
            button_highlight,
            close_red,
            button_shape,
            button_size,
            button_circle,
            button_radius,
            icon_size,
            button_edge_gap,
            button_gap,
            title_gap,
            button_hover_brighten,
            buttons_position,
            buttons,
            menu_command,
            menu_close_command,
            menu_namespace,
            unified,
            transparent,
            button_hover_group,
        );

        if let Some(x) = &part.button_styles {
            self.button_styles
                .get_or_insert_with(Default::default)
                .merge_with(x);
        }

        if let Some(x) = &part.icons {
            self.icons
                .get_or_insert_with(Default::default)
                .merge_with(x);
        }
    }
}
