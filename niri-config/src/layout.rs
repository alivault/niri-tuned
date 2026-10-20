use knuffel::errors::DecodeError;
use niri_ipc::{ColumnDisplay, SizeChange};

use crate::appearance::{
    Border, FocusRing, InsertHint, Shadow, TabIndicator, Titlebar, DEFAULT_BACKGROUND_COLOR,
};
use crate::utils::{expect_only_children, Flag, MergeWith};
use crate::{
    BorderRule, Color, FloatOrInt, InsertHintPart, ShadowRule, TabIndicatorPart, TitlebarRule,
};

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub focus_ring: FocusRing,
    pub border: Border,
    pub shadow: Shadow,
    pub tab_indicator: TabIndicator,
    pub titlebar: Titlebar,
    pub insert_hint: InsertHint,
    pub edge_resize: EdgeResize,
    pub preset_column_widths: Vec<PresetSize>,
    pub default_column_width: Option<PresetSize>,
    pub preset_window_heights: Vec<PresetSize>,
    pub center_focused_column: CenterFocusedColumn,
    pub always_center_single_column: bool,
    pub empty_workspace_above_first: bool,
    /// Switching to the next or previous preset width stops at the last or first one instead of
    /// wrapping around (`preset-widths-no-wrap`).
    pub preset_widths_no_wrap: bool,
    /// The same for preset window heights (`preset-heights-no-wrap`).
    pub preset_heights_no_wrap: bool,
    /// New windows that open in the scrolling layout open maximized to the edges
    /// (`open-maximized-to-edges`); floating windows and dialogs are left alone, and the
    /// `open-maximized-to-edges` window rule overrides it either way.
    pub open_maximized_to_edges: bool,
    pub default_column_display: ColumnDisplay,
    pub gaps: f64,
    pub struts: Struts,
    pub background_color: Color,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            focus_ring: FocusRing::default(),
            border: Border::default(),
            shadow: Shadow::default(),
            tab_indicator: TabIndicator::default(),
            titlebar: Titlebar::default(),
            insert_hint: InsertHint::default(),
            edge_resize: EdgeResize::default(),
            preset_column_widths: vec![
                PresetSize::Proportion(1. / 3.),
                PresetSize::Proportion(0.5),
                PresetSize::Proportion(2. / 3.),
            ],
            default_column_width: Some(PresetSize::Proportion(0.5)),
            center_focused_column: CenterFocusedColumn::Never,
            always_center_single_column: false,
            empty_workspace_above_first: false,
            preset_widths_no_wrap: false,
            preset_heights_no_wrap: false,
            open_maximized_to_edges: false,
            default_column_display: ColumnDisplay::Normal,
            gaps: 16.,
            struts: Struts::default(),
            preset_window_heights: vec![
                PresetSize::Proportion(1. / 3.),
                PresetSize::Proportion(0.5),
                PresetSize::Proportion(2. / 3.),
            ],
            background_color: DEFAULT_BACKGROUND_COLOR,
        }
    }
}

impl MergeWith<LayoutPart> for Layout {
    fn merge_with(&mut self, part: &LayoutPart) {
        merge!(
            (self, part),
            focus_ring,
            border,
            shadow,
            tab_indicator,
            titlebar,
            insert_hint,
            edge_resize,
            always_center_single_column,
            empty_workspace_above_first,
            preset_widths_no_wrap,
            preset_heights_no_wrap,
            open_maximized_to_edges,
            gaps,
        );

        merge_clone!(
            (self, part),
            preset_column_widths,
            preset_window_heights,
            center_focused_column,
            default_column_display,
            struts,
            background_color,
        );

        if let Some(x) = part.default_column_width {
            self.default_column_width = x.0;
        }

        if self.preset_column_widths.is_empty() {
            self.preset_column_widths = Layout::default().preset_column_widths;
        }

        if self.preset_window_heights.is_empty() {
            self.preset_window_heights = Layout::default().preset_window_heights;
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct LayoutPart {
    #[knuffel(child)]
    pub focus_ring: Option<BorderRule>,
    #[knuffel(child)]
    pub border: Option<BorderRule>,
    #[knuffel(child)]
    pub shadow: Option<ShadowRule>,
    #[knuffel(child)]
    pub tab_indicator: Option<TabIndicatorPart>,
    #[knuffel(child)]
    pub titlebar: Option<TitlebarRule>,
    #[knuffel(child)]
    pub insert_hint: Option<InsertHintPart>,
    #[knuffel(child)]
    pub edge_resize: Option<EdgeResizePart>,
    #[knuffel(child, unwrap(children))]
    pub preset_column_widths: Option<Vec<PresetSize>>,
    #[knuffel(child)]
    pub default_column_width: Option<DefaultPresetSize>,
    #[knuffel(child, unwrap(children))]
    pub preset_window_heights: Option<Vec<PresetSize>>,
    #[knuffel(child, unwrap(argument))]
    pub center_focused_column: Option<CenterFocusedColumn>,
    #[knuffel(child)]
    pub always_center_single_column: Option<Flag>,
    #[knuffel(child)]
    pub empty_workspace_above_first: Option<Flag>,
    #[knuffel(child)]
    pub preset_widths_no_wrap: Option<Flag>,
    #[knuffel(child)]
    pub preset_heights_no_wrap: Option<Flag>,
    #[knuffel(child)]
    pub open_maximized_to_edges: Option<Flag>,
    #[knuffel(child, unwrap(argument, str))]
    pub default_column_display: Option<ColumnDisplay>,
    #[knuffel(child, unwrap(argument))]
    pub gaps: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child)]
    pub struts: Option<Struts>,
    #[knuffel(child)]
    pub background_color: Option<Color>,
}

/// Resizing windows by dragging their edges: the band from `outside` beyond a window's edge to
/// `inside` past its border, and `corner` along each edge from a corner for both edges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EdgeResize {
    pub off: bool,
    pub outside: f64,
    pub inside: f64,
    pub corner: f64,
}

impl Default for EdgeResize {
    fn default() -> Self {
        Self {
            off: false,
            outside: 12.,
            inside: 4.,
            corner: 28.,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct EdgeResizePart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(argument))]
    pub outside: Option<FloatOrInt<0, 1024>>,
    #[knuffel(child, unwrap(argument))]
    pub inside: Option<FloatOrInt<0, 1024>>,
    #[knuffel(child, unwrap(argument))]
    pub corner: Option<FloatOrInt<0, 1024>>,
}

impl MergeWith<EdgeResizePart> for EdgeResize {
    fn merge_with(&mut self, part: &EdgeResizePart) {
        self.off |= part.off;
        if part.on {
            self.off = false;
        }
        merge!((self, part), outside, inside, corner);
    }
}

#[derive(knuffel::Decode, Debug, Clone, Copy, PartialEq)]
pub enum PresetSize {
    Proportion(#[knuffel(argument)] f64),
    Fixed(#[knuffel(argument)] i32),
}

impl From<PresetSize> for SizeChange {
    fn from(value: PresetSize) -> Self {
        match value {
            PresetSize::Proportion(prop) => SizeChange::SetProportion(prop * 100.),
            PresetSize::Fixed(fixed) => SizeChange::SetFixed(fixed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DefaultPresetSize(pub Option<PresetSize>);

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct Struts {
    #[knuffel(child, unwrap(argument), default)]
    pub left: FloatOrInt<-65535, 65535>,
    #[knuffel(child, unwrap(argument), default)]
    pub right: FloatOrInt<-65535, 65535>,
    #[knuffel(child, unwrap(argument), default)]
    pub top: FloatOrInt<-65535, 65535>,
    #[knuffel(child, unwrap(argument), default)]
    pub bottom: FloatOrInt<-65535, 65535>,
}

#[derive(knuffel::DecodeScalar, Debug, Default, PartialEq, Eq, Clone, Copy)]
pub enum CenterFocusedColumn {
    /// Focusing a column will not center the column.
    #[default]
    Never,
    /// The focused column will always be centered.
    Always,
    /// Focusing a column will center it if it doesn't fit on the screen together with the
    /// previously focused column.
    OnOverflow,
}

impl<S> knuffel::Decode<S> for DefaultPresetSize
where
    S: knuffel::traits::ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        expect_only_children(node, ctx);

        let mut children = node.children();

        if let Some(child) = children.next() {
            if let Some(unwanted_child) = children.next() {
                ctx.emit_error(DecodeError::unexpected(
                    unwanted_child,
                    "node",
                    "expected no more than one child",
                ));
            }
            PresetSize::decode_node(child, ctx).map(Some).map(Self)
        } else {
            Ok(Self(None))
        }
    }
}
