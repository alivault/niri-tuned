use std::collections::HashSet;

use knuffel::errors::DecodeError;
use smithay::input::keyboard::Keysym;

use crate::utils::{expect_only_children, MergeWith};
use crate::{Action, Bind, Color, FloatOrInt, Key, Modifiers, Trigger};

#[derive(Debug, PartialEq)]
pub struct RecentWindows {
    pub on: bool,
    pub debounce_ms: u16,
    pub open_delay_ms: u16,
    pub highlight: MruHighlight,
    pub previews: MruPreviews,
    pub scope_panel: MruScopePanel,
    pub container: MruContainer,
    pub label: MruLabel,
    pub flip: MruFlip,
    pub binds: Vec<Bind>,
    /// Keys while the switcher is open, pressed with the modifier still held
    /// (the bind's own modifiers are filled in from the held ones).
    pub opened_binds: Vec<Bind>,
}

impl RecentWindows {
    /// The container's corner radius: its own `corner-radius`, else concentric
    /// with the highlight around an item at its edge (the highlight's outer
    /// radius, its corner radius plus its border, plus the container padding
    /// between them), so one roundness shapes both.
    pub fn container_radius(&self) -> f64 {
        self.container.corner_radius.unwrap_or(
            self.highlight.corner_radius + self.highlight.border_width + self.container.padding,
        )
    }
}

impl Default for RecentWindows {
    fn default() -> Self {
        RecentWindows {
            on: true,
            debounce_ms: 750,
            open_delay_ms: 150,
            highlight: MruHighlight::default(),
            previews: MruPreviews::default(),
            scope_panel: MruScopePanel::default(),
            container: MruContainer::default(),
            label: MruLabel::default(),
            flip: MruFlip::default(),
            binds: default_binds(),
            opened_binds: default_opened_binds(),
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, PartialEq)]
pub struct RecentWindowsPart {
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child, unwrap(argument))]
    pub debounce_ms: Option<u16>,
    #[knuffel(child, unwrap(argument))]
    pub open_delay_ms: Option<u16>,
    #[knuffel(child)]
    pub highlight: Option<MruHighlightPart>,
    #[knuffel(child)]
    pub previews: Option<MruPreviewsPart>,
    #[knuffel(child)]
    pub scope_panel: Option<MruScopePanelPart>,
    #[knuffel(child)]
    pub container: Option<MruContainerPart>,
    #[knuffel(child)]
    pub label: Option<MruLabelPart>,
    #[knuffel(child)]
    pub flip: Option<MruFlipPart>,
    #[knuffel(child)]
    pub binds: Option<MruBinds>,
    #[knuffel(child)]
    pub opened_binds: Option<MruOpenedBinds>,
}

impl MergeWith<RecentWindowsPart> for RecentWindows {
    fn merge_with(&mut self, part: &RecentWindowsPart) {
        self.on |= part.on;
        if part.off {
            self.on = false;
        }

        merge_clone!((self, part), debounce_ms, open_delay_ms);
        merge!(
            (self, part),
            highlight,
            previews,
            scope_panel,
            container,
            label,
            flip
        );

        if let Some(part) = &part.binds {
            // Remove existing binds matching any new bind.
            self.binds
                .retain(|bind| !part.0.iter().any(|new| new.key == bind.key));
            // Add all new binds.
            self.binds.extend(part.0.iter().cloned().map(Bind::from));
        }

        if let Some(part) = &part.opened_binds {
            // A key bound again replaces its default; `none` only removes it.
            self.opened_binds
                .retain(|bind| !part.0.iter().any(|new| new.key == bind.key));
            self.opened_binds.extend(
                part.0
                    .iter()
                    .filter(|new| new.action != MruOpenedAction::None)
                    .cloned()
                    .map(Bind::from),
            );
        }
    }
}

/// The rounded panel behind the previews: its tint over the blur, and the
/// colour of the window titles drawn on it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MruContainer {
    pub background_color: Color,
    pub background_opacity: f64,
    pub title_color: Color,
    /// Space between the outermost highlights (and labels) and the edge.
    pub padding: f64,
    /// Corner radius; unset, concentric with the highlight
    /// (`RecentWindows::container_radius`).
    pub corner_radius: Option<f64>,
    /// Least space between the container and the left and right edges of the
    /// output; the items wrap (or shrink) to stay inside it.
    pub margin_x: f64,
    /// Least space between the container and the top and bottom edges.
    pub margin_y: f64,
    /// The container covers the whole output (no corners, no margins), the
    /// desktop all blurred and tinted behind the previews or icons.
    pub fullscreen: bool,
    /// The tint's opacity, its colour's own alpha aside (so a theme's colour
    /// and this layer independently); unset, the colour's alpha times
    /// `background-opacity`.
    pub overlay_opacity: Option<f64>,
    /// How strong the blur behind the container is, in logical pixels
    /// (0: none); unset, the global `blur` settings.
    pub blur_radius: Option<f64>,
}

impl Default for MruContainer {
    fn default() -> Self {
        Self {
            background_color: Color::new_unpremul(0., 0., 0., 1.),
            background_opacity: 0.35,
            title_color: Color::new_unpremul(1., 1., 1., 1.),
            padding: 16.,
            corner_radius: None,
            margin_x: 48.,
            margin_y: 80.,
            fullscreen: false,
            overlay_opacity: None,
            blur_radius: None,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, PartialEq)]
pub struct MruContainerPart {
    #[knuffel(child)]
    pub background_color: Option<Color>,
    #[knuffel(child, unwrap(argument))]
    pub background_opacity: Option<FloatOrInt<0, 1>>,
    #[knuffel(child)]
    pub title_color: Option<Color>,
    #[knuffel(child, unwrap(argument))]
    pub padding: Option<FloatOrInt<0, 256>>,
    #[knuffel(child, unwrap(argument))]
    pub corner_radius: Option<FloatOrInt<0, 1024>>,
    #[knuffel(child, unwrap(argument))]
    pub margin_x: Option<FloatOrInt<0, 4096>>,
    #[knuffel(child, unwrap(argument))]
    pub margin_y: Option<FloatOrInt<0, 4096>>,
    #[knuffel(child, unwrap(argument))]
    pub fullscreen: Option<bool>,
    #[knuffel(child, unwrap(argument))]
    pub overlay_opacity: Option<FloatOrInt<0, 1>>,
    #[knuffel(child, unwrap(argument))]
    pub blur_radius: Option<FloatOrInt<0, 1024>>,
}

impl MergeWith<MruContainerPart> for MruContainer {
    fn merge_with(&mut self, part: &MruContainerPart) {
        merge_clone!((self, part), background_color, title_color);
        merge!(
            (self, part),
            background_opacity,
            padding,
            margin_x,
            margin_y
        );
        if let Some(radius) = part.corner_radius {
            self.corner_radius = Some(radius.0);
        }
        merge_clone!((self, part), fullscreen);
        if let Some(x) = part.overlay_opacity {
            self.overlay_opacity = Some(x.0);
        }
        if let Some(x) = part.blur_radius {
            self.blur_radius = Some(x.0);
        }
    }
}

/// What the label under a preview or icon says.
#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MruLabelText {
    /// The window title.
    Title,
    /// The app's name (its desktop entry's Name, else from its app id).
    AppName,
    /// "App — window title" (just the app name when the title adds nothing).
    AppAndTitle,
    /// No label.
    None,
}

/// Which previews or icons carry a label.
#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MruLabelShow {
    /// Only the selected one, centred under it (macOS's app switcher).
    Selected,
    /// Every one.
    All,
}

/// The label under each preview or icon. `text` and `show` default by mode:
/// window previews show every title, icons only the selected app's name.
#[derive(Debug, Clone, PartialEq)]
pub struct MruLabel {
    pub text: Option<MruLabelText>,
    pub show: Option<MruLabelShow>,
    /// Pango font description, e.g. "sans 14px".
    pub font: String,
    /// Space between a preview's app icon (or an icon) and its label.
    pub gap: f64,
}

impl MruLabel {
    pub fn text_for(&self, icons_only: bool) -> MruLabelText {
        self.text.unwrap_or(if icons_only {
            MruLabelText::AppName
        } else {
            MruLabelText::Title
        })
    }

    /// Whether only the selected preview or icon carries a label.
    pub fn selected_only(&self, icons_only: bool) -> bool {
        self.show.unwrap_or(if icons_only {
            MruLabelShow::Selected
        } else {
            MruLabelShow::All
        }) == MruLabelShow::Selected
    }
}

impl Default for MruLabel {
    fn default() -> Self {
        Self {
            text: None,
            show: None,
            font: String::from("sans 14px"),
            gap: 8.,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, PartialEq)]
pub struct MruLabelPart {
    #[knuffel(child, unwrap(argument))]
    pub text: Option<MruLabelText>,
    #[knuffel(child, unwrap(argument))]
    pub show: Option<MruLabelShow>,
    #[knuffel(child, unwrap(argument))]
    pub font: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub gap: Option<FloatOrInt<0, 256>>,
}

impl MergeWith<MruLabelPart> for MruLabel {
    fn merge_with(&mut self, part: &MruLabelPart) {
        if part.text.is_some() {
            self.text = part.text;
        }
        if part.show.is_some() {
            self.show = part.show;
        }
        merge_clone!((self, part), font);
        merge!((self, part), gap);
    }
}

#[derive(Debug, PartialEq)]
pub struct MruHighlight {
    pub active_color: Color,
    pub urgent_color: Color,
    pub border_color: Option<Color>,
    pub border_opacity: f64,
    pub background_color: Color,
    pub background_opacity: f64,
    pub padding: f64,
    pub corner_radius: f64,
    /// Width of the border drawn outside the highlight.
    pub border_width: f64,
}

impl Default for MruHighlight {
    fn default() -> Self {
        Self {
            active_color: Color::new_unpremul(0.6, 0.6, 0.6, 1.),
            urgent_color: Color::new_unpremul(1., 0.6, 0.6, 1.),
            border_color: None,
            border_opacity: 1.,
            background_color: Color::new_unpremul(0., 0., 0., 1.),
            background_opacity: 0.2,
            padding: 30.,
            corner_radius: 0.,
            border_width: 2.,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, PartialEq)]
pub struct MruHighlightPart {
    #[knuffel(child)]
    pub active_color: Option<Color>,
    #[knuffel(child)]
    pub urgent_color: Option<Color>,
    #[knuffel(child)]
    pub border_color: Option<Color>,
    #[knuffel(child, unwrap(argument))]
    pub border_opacity: Option<FloatOrInt<0, 1>>,
    #[knuffel(child)]
    pub background_color: Option<Color>,
    #[knuffel(child, unwrap(argument))]
    pub background_opacity: Option<FloatOrInt<0, 1>>,
    #[knuffel(child, unwrap(argument))]
    pub padding: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub corner_radius: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub border_width: Option<FloatOrInt<0, 64>>,
}

impl MergeWith<MruHighlightPart> for MruHighlight {
    fn merge_with(&mut self, part: &MruHighlightPart) {
        merge_clone!((self, part), active_color, urgent_color);
        if let Some(color) = part.border_color {
            self.border_color = Some(color);
        }
        merge_clone!((self, part), background_color);
        merge!(
            (self, part),
            border_opacity,
            background_opacity,
            padding,
            corner_radius,
            border_width
        );
    }
}

/// The scope indicator (All / Workspace / Output) shown above the switcher.
#[derive(Debug, Clone, PartialEq)]
pub struct MruScopePanel {
    pub off: bool,
    pub background_color: Color,
    pub background_opacity: f64,
    pub text_color: Color,
    /// Fill of the pill behind the current scope.
    pub active_color: Color,
    /// Text colour on the current scope's pill.
    pub active_text_color: Color,
    pub border_color: Color,
    pub border_width: f64,
    pub corner_radius: f64,
    /// Pango font description, e.g. "Inter 13px".
    pub font: String,
}

impl Default for MruScopePanel {
    fn default() -> Self {
        Self {
            off: false,
            background_color: Color::new_unpremul(0.1, 0.1, 0.1, 1.),
            background_opacity: 0.97,
            text_color: Color::new_unpremul(0.9, 0.9, 0.9, 1.),
            active_color: Color::new_unpremul(0.5, 0.5, 0.5, 1.),
            active_text_color: Color::new_unpremul(0.1, 0.1, 0.1, 1.),
            border_color: Color::new_unpremul(0.35, 0.35, 0.35, 1.),
            border_width: 1.,
            corner_radius: 7.,
            font: String::from("sans Medium 12px"),
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, PartialEq)]
pub struct MruScopePanelPart {
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub background_color: Option<Color>,
    #[knuffel(child, unwrap(argument))]
    pub background_opacity: Option<FloatOrInt<0, 1>>,
    #[knuffel(child)]
    pub text_color: Option<Color>,
    #[knuffel(child)]
    pub active_color: Option<Color>,
    #[knuffel(child)]
    pub active_text_color: Option<Color>,
    #[knuffel(child)]
    pub border_color: Option<Color>,
    #[knuffel(child, unwrap(argument))]
    pub border_width: Option<FloatOrInt<0, 64>>,
    #[knuffel(child, unwrap(argument))]
    pub corner_radius: Option<FloatOrInt<0, 64>>,
    #[knuffel(child, unwrap(argument))]
    pub font: Option<String>,
}

impl MergeWith<MruScopePanelPart> for MruScopePanel {
    fn merge_with(&mut self, part: &MruScopePanelPart) {
        if part.on {
            self.off = false;
        }
        if part.off {
            self.off = true;
        }
        merge_clone!(
            (self, part),
            background_color,
            text_color,
            active_color,
            active_text_color,
            border_color,
            font
        );
        merge!(
            (self, part),
            background_opacity,
            border_width,
            corner_radius
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MruPreviews {
    pub max_height: f64,
    pub max_scale: f64,
    pub antialiasing: bool,
    /// Draw each window as its app icon instead of a live preview.
    pub icons_only: bool,
    /// Draw the windows as a stack of tilted live windows receding into the
    /// screen (Windows Vista's Flip 3D) instead of a grid; never with
    /// `icons_only`.
    pub flip: bool,
    /// Logical size of the icons in `icons_only` mode.
    pub icon_size: f64,
    /// Fill rows at full size and start a new one when a row is full (`true`,
    /// the default) or keep one row, shrinking it to fit (`false`).
    pub wrap: Option<bool>,
    /// Space between two previews or icons (icons shrink it with themselves).
    pub gap: f64,
}

impl MruPreviews {
    /// Whether the switcher wraps onto more rows rather than keeping one.
    pub fn wraps(&self) -> bool {
        self.wrap.unwrap_or(true)
    }
}

impl Default for MruPreviews {
    fn default() -> Self {
        Self {
            max_height: 480.,
            max_scale: 0.5,
            antialiasing: true,
            icons_only: false,
            flip: false,
            icon_size: 96.,
            wrap: None,
            gap: 16.,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, PartialEq)]
pub struct MruPreviewsPart {
    #[knuffel(child, unwrap(argument))]
    pub max_height: Option<FloatOrInt<1, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub max_scale: Option<FloatOrInt<0, 1>>,
    #[knuffel(child, unwrap(argument))]
    pub antialiasing: Option<bool>,
    #[knuffel(child, unwrap(argument))]
    pub icons_only: Option<bool>,
    /// How the windows are drawn; `icons-only` is the older switch for icons,
    /// and a `style` beside it wins.
    #[knuffel(child, unwrap(argument))]
    pub style: Option<MruStyle>,
    #[knuffel(child, unwrap(argument))]
    pub icon_size: Option<FloatOrInt<16, 512>>,
    #[knuffel(child, unwrap(argument))]
    pub wrap: Option<bool>,
    #[knuffel(child, unwrap(argument))]
    pub gap: Option<FloatOrInt<0, 256>>,
}

impl MergeWith<MruPreviewsPart> for MruPreviews {
    fn merge_with(&mut self, part: &MruPreviewsPart) {
        merge!((self, part), max_height, max_scale, icon_size, gap);
        merge_clone!((self, part), antialiasing, icons_only);
        if part.icons_only.is_some() {
            self.flip = false;
        }
        if let Some(style) = part.style {
            self.icons_only = style == MruStyle::Icons;
            self.flip = style == MruStyle::Flip;
        }
        if part.wrap.is_some() {
            self.wrap = part.wrap;
        }
    }
}

/// The shape of the Flip 3D stack (`previews.style "flip"`), in real 3D: every
/// window faces the same way, and each one sits straight behind the one in
/// front of it, so the stack goes back opposite to where the windows face.
/// Lengths are percentages of the output's width (`spacing`, `front-x`) or
/// height (`front-y`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MruFlip {
    /// Degrees the windows turn to face right (positive, their left edges
    /// nearer) or left (negative).
    pub tilt_x: f64,
    /// Degrees the windows turn to face down (positive, their top edges
    /// nearer) or up (negative).
    pub tilt_y: f64,
    /// How far behind one another the windows are, along the stack.
    pub spacing: f64,
    /// How far each window is shifted from the one in front of it, along the
    /// windows' own right (positive) and down (positive) directions, so the
    /// shift turns with the tilt; percentages of the output's width.
    pub offset_x: f64,
    pub offset_y: f64,
    /// The centre of the front window.
    pub front_x: f64,
    pub front_y: f64,
    /// Where the stack is looked at from, relative to the front window's
    /// centre (percentages of the output's width and height): 0 and 0 look
    /// straight at it; negative from the left and from above.
    pub camera_x: f64,
    pub camera_y: f64,
    /// The largest a window is drawn, as a percentage of the output's width
    /// and height (smaller windows keep their size).
    pub card_size: f64,
    /// How many windows show at once, the front one included (Vista's 10);
    /// the rest are a step away as the stack scrolls.
    pub visible_windows: f64,
}

impl Default for MruFlip {
    fn default() -> Self {
        Self {
            // Vista's Flip 3D, measured: facing almost straight ahead, looked
            // at from above, so the stack rises towards the top.
            tilt_x: 23.,
            tilt_y: 2.,
            spacing: 44.,
            offset_x: 0.,
            offset_y: 0.,
            front_x: 69.,
            front_y: 60.,
            camera_x: -15.,
            camera_y: -32.,
            card_size: 43.,
            visible_windows: 10.,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, PartialEq)]
pub struct MruFlipPart {
    #[knuffel(child, unwrap(argument))]
    pub tilt_x: Option<FloatOrInt<-80, 80>>,
    #[knuffel(child, unwrap(argument))]
    pub tilt_y: Option<FloatOrInt<-80, 80>>,
    /// The older name of `tilt-x`.
    #[knuffel(child, unwrap(argument))]
    pub tilt: Option<FloatOrInt<-80, 80>>,
    #[knuffel(child, unwrap(argument))]
    pub spacing: Option<FloatOrInt<0, 200>>,
    #[knuffel(child, unwrap(argument))]
    pub offset_x: Option<FloatOrInt<-100, 100>>,
    #[knuffel(child, unwrap(argument))]
    pub offset_y: Option<FloatOrInt<-100, 100>>,
    /// No longer used: the tilt decides where the stack goes. Accepted so an
    /// older config still loads.
    #[knuffel(child, unwrap(argument))]
    pub shift_x: Option<FloatOrInt<-100, 100>>,
    #[knuffel(child, unwrap(argument))]
    pub shift_y: Option<FloatOrInt<-100, 100>>,
    #[knuffel(child, unwrap(argument))]
    pub front_x: Option<FloatOrInt<0, 100>>,
    #[knuffel(child, unwrap(argument))]
    pub front_y: Option<FloatOrInt<0, 100>>,
    #[knuffel(child, unwrap(argument))]
    pub camera_x: Option<FloatOrInt<-100, 100>>,
    #[knuffel(child, unwrap(argument))]
    pub camera_y: Option<FloatOrInt<-100, 100>>,
    #[knuffel(child, unwrap(argument))]
    pub card_size: Option<FloatOrInt<5, 100>>,
    #[knuffel(child, unwrap(argument))]
    pub visible_windows: Option<FloatOrInt<1, 50>>,
}

impl MergeWith<MruFlipPart> for MruFlip {
    fn merge_with(&mut self, part: &MruFlipPart) {
        if let Some(tilt) = part.tilt {
            self.tilt_x = tilt.0;
        }
        merge!(
            (self, part),
            tilt_x,
            tilt_y,
            spacing,
            offset_x,
            offset_y,
            front_x,
            front_y,
            camera_x,
            camera_y,
            card_size,
            visible_windows
        );
    }
}

/// How the recent-windows switcher draws the windows.
#[derive(knuffel::DecodeScalar, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MruStyle {
    /// Each window's app icon, in rows.
    Icons,
    /// Live window previews, in rows.
    Previews,
    /// A stack of tilted live windows receding into the screen.
    Flip,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MruBind {
    // MRU bind keys must have a modifier, this is enforced during parsing. The switcher will close
    // once all modifiers are released.
    pub key: Key,
    pub action: MruAction,
    pub allow_inhibiting: bool,
    pub hotkey_overlay_title: Option<Option<String>>,
}

impl From<MruBind> for Bind {
    fn from(x: MruBind) -> Self {
        Self {
            key: x.key,
            action: Action::from(x.action),
            repeat: true,
            cooldown: None,
            allow_when_locked: false,
            allow_inhibiting: x.allow_inhibiting,
            hotkey_overlay_title: x.hotkey_overlay_title,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum MruDirection {
    /// Most recently used to least.
    #[default]
    Forward,
    /// Least recently used to most.
    Backward,
    /// One row up in the grid.
    Up,
    /// One row down in the grid.
    Down,
}

#[derive(knuffel::DecodeScalar, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MruScope {
    /// All windows.
    #[default]
    All,
    /// Windows on the active output.
    Output,
    /// Windows on the active workspace.
    Workspace,
}

#[derive(knuffel::DecodeScalar, Clone, Copy, Debug, Default, PartialEq)]
pub enum MruFilter {
    /// All windows.
    #[default]
    #[knuffel(skip)]
    All,
    /// Windows with the same app id as the active window.
    AppId,
}

#[derive(knuffel::Decode, Debug, Clone, PartialEq)]
pub enum MruAction {
    NextWindow(
        #[knuffel(property(name = "scope"))] Option<MruScope>,
        #[knuffel(property(name = "filter"), default)] MruFilter,
    ),
    PreviousWindow(
        #[knuffel(property(name = "scope"))] Option<MruScope>,
        #[knuffel(property(name = "filter"), default)] MruFilter,
    ),
}

impl From<MruAction> for Action {
    fn from(x: MruAction) -> Self {
        match x {
            MruAction::NextWindow(scope, filter) => Self::MruAdvance {
                direction: MruDirection::Forward,
                scope,
                filter: Some(filter),
            },
            MruAction::PreviousWindow(scope, filter) => Self::MruAdvance {
                direction: MruDirection::Backward,
                scope,
                filter: Some(filter),
            },
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct MruBinds(pub Vec<MruBind>);

fn default_binds() -> Vec<Bind> {
    let mut rv = Vec::new();

    let mut push = |trigger, base_mod, filter| {
        rv.push(Bind::from(MruBind {
            key: Key {
                trigger: Trigger::Keysym(trigger),
                modifiers: base_mod,
            },
            action: MruAction::NextWindow(None, filter),
            allow_inhibiting: true,
            hotkey_overlay_title: None,
        }));
        rv.push(Bind::from(MruBind {
            key: Key {
                trigger: Trigger::Keysym(trigger),
                modifiers: base_mod | Modifiers::SHIFT,
            },
            action: MruAction::PreviousWindow(None, filter),
            allow_inhibiting: true,
            hotkey_overlay_title: None,
        }));
    };

    for base_mod in [Modifiers::ALT, Modifiers::COMPOSITOR] {
        push(Keysym::Tab, base_mod, MruFilter::All);
        push(Keysym::grave, base_mod, MruFilter::AppId);
    }

    rv
}

impl<S> knuffel::Decode<S> for MruBinds
where
    S: knuffel::traits::ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        expect_only_children(node, ctx);

        let mut seen_keys = HashSet::new();

        let mut binds = Vec::new();

        for child in node.children() {
            match MruBind::decode_node(child, ctx) {
                Ok(bind) => {
                    if !seen_keys.insert(bind.key) {
                        ctx.emit_error(DecodeError::unexpected(
                            &child.node_name,
                            "keybind",
                            "duplicate keybind",
                        ));
                        continue;
                    }

                    binds.push(bind);
                }
                Err(e) => {
                    ctx.emit_error(e);
                }
            }
        }

        Ok(Self(binds))
    }
}

impl<S> knuffel::Decode<S> for MruBind
where
    S: knuffel::traits::ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        if let Some(type_name) = &node.type_name {
            ctx.emit_error(DecodeError::unexpected(
                type_name,
                "type name",
                "no type name expected for this node",
            ));
        }

        for val in node.arguments.iter() {
            ctx.emit_error(DecodeError::unexpected(
                &val.literal,
                "argument",
                "no arguments expected for this node",
            ));
        }

        let key = node
            .node_name
            .parse::<Key>()
            .map_err(|e| DecodeError::conversion(&node.node_name, e.wrap_err("invalid keybind")))?;

        // A modifier is required because MRU remains on screen as long as any modifier is held.
        if key.modifiers.is_empty() {
            ctx.emit_error(DecodeError::unexpected(
                &node.node_name,
                "keybind",
                "keybind must have a modifier key",
            ));
        }

        // FIXME: To support this, all the mods_with_mouse_binds()/mods_with_wheel_binds()/etc.
        // will need to learn about recent-windows bindings.
        if !matches!(key.trigger, Trigger::Keysym(_)) {
            ctx.emit_error(DecodeError::unexpected(
                &node.node_name,
                "key",
                "key must be a keyboard key (others are unsupported here for now)",
            ));
        }

        let mut allow_inhibiting = true;
        let mut hotkey_overlay_title = None;
        for (name, val) in &node.properties {
            match &***name {
                "allow-inhibiting" => {
                    allow_inhibiting = knuffel::traits::DecodeScalar::decode(val, ctx)?;
                }
                "hotkey-overlay-title" => {
                    hotkey_overlay_title = Some(knuffel::traits::DecodeScalar::decode(val, ctx)?);
                }
                name_str => {
                    ctx.emit_error(DecodeError::unexpected(
                        name,
                        "property",
                        format!("unexpected property `{}`", name_str.escape_default()),
                    ));
                }
            }
        }

        let mut children = node.children();

        // If the action is invalid but the key is fine, we still want to return something.
        // That way, the parent can handle the existence of duplicate keybinds,
        // even if their contents are not valid.
        let dummy = Self {
            key,
            action: MruAction::NextWindow(None, MruFilter::All),
            allow_inhibiting: true,
            hotkey_overlay_title: None,
        };

        if let Some(child) = children.next() {
            for unwanted_child in children {
                ctx.emit_error(DecodeError::unexpected(
                    unwanted_child,
                    "node",
                    "only one action is allowed per keybind",
                ));
            }
            match MruAction::decode_node(child, ctx) {
                Ok(action) => Ok(Self {
                    key,
                    action,
                    allow_inhibiting,
                    hotkey_overlay_title,
                }),
                Err(e) => {
                    ctx.emit_error(e);
                    Ok(dummy)
                }
            }
        } else {
            ctx.emit_error(DecodeError::missing(
                node,
                "expected an action for this keybind",
            ));
            Ok(dummy)
        }
    }
}

/// What a key does while the switcher is open.
#[derive(knuffel::Decode, Debug, Clone, PartialEq)]
pub enum MruOpenedAction {
    Confirm,
    Cancel,
    NextWindow,
    PreviousWindow,
    /// One row up in the grid.
    RowUp,
    /// One row down in the grid.
    RowDown,
    First,
    Last,
    SetScope(#[knuffel(argument)] MruScope),
    CycleScope,
    /// Close the selected window.
    CloseWindow,
    /// Run a command with the selected window's id appended as its last argument.
    SpawnForWindow(#[knuffel(arguments)] Vec<String>),
    /// Unbind a default key.
    None,
}

impl From<MruOpenedAction> for Action {
    fn from(x: MruOpenedAction) -> Self {
        let advance = |direction| Self::MruAdvance {
            direction,
            scope: None,
            filter: None,
        };
        match x {
            MruOpenedAction::Confirm => Self::MruConfirm,
            MruOpenedAction::Cancel => Self::MruCancel,
            MruOpenedAction::NextWindow => advance(MruDirection::Forward),
            MruOpenedAction::PreviousWindow => advance(MruDirection::Backward),
            MruOpenedAction::RowUp => advance(MruDirection::Up),
            MruOpenedAction::RowDown => advance(MruDirection::Down),
            MruOpenedAction::First => Self::MruFirst,
            MruOpenedAction::Last => Self::MruLast,
            MruOpenedAction::SetScope(scope) => Self::MruSetScope(scope),
            MruOpenedAction::CycleScope => Self::MruCycleScope,
            MruOpenedAction::CloseWindow => Self::MruCloseCurrentWindow,
            MruOpenedAction::SpawnForWindow(command) => Self::MruSpawnForCurrentWindow(command),
            // Filtered out when merging; never reaches the bind list.
            MruOpenedAction::None => Self::MruCancel,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MruOpenedBind {
    /// The key alone: the switcher fills in the modifiers being held.
    pub key: Key,
    pub action: MruOpenedAction,
}

impl From<MruOpenedBind> for Bind {
    fn from(x: MruOpenedBind) -> Self {
        Self {
            key: x.key,
            action: Action::from(x.action),
            repeat: true,
            cooldown: None,
            allow_when_locked: false,
            allow_inhibiting: false,
            hotkey_overlay_title: None,
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct MruOpenedBinds(pub Vec<MruOpenedBind>);

fn default_opened_binds() -> Vec<Bind> {
    use MruOpenedAction::*;
    [
        (Keysym::Escape, Cancel),
        (Keysym::Return, Confirm),
        (Keysym::space, Confirm),
        (Keysym::_1, SetScope(MruScope::All)),
        (Keysym::_2, SetScope(MruScope::Workspace)),
        (Keysym::_3, SetScope(MruScope::Output)),
        (Keysym::s, CycleScope),
        // The most expected keys, so they work even without matching normal binds.
        (Keysym::Home, First),
        (Keysym::End, Last),
        (Keysym::Left, PreviousWindow),
        (Keysym::Right, NextWindow),
        (Keysym::Up, RowUp),
        (Keysym::Down, RowDown),
    ]
    .into_iter()
    .map(|(keysym, action)| {
        Bind::from(MruOpenedBind {
            key: Key {
                trigger: Trigger::Keysym(keysym),
                modifiers: Modifiers::empty(),
            },
            action,
        })
    })
    .collect()
}

impl<S> knuffel::Decode<S> for MruOpenedBinds
where
    S: knuffel::traits::ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        expect_only_children(node, ctx);
        let mut seen_keys = HashSet::new();
        let mut binds = Vec::new();
        for child in node.children() {
            match MruOpenedBind::decode_node(child, ctx) {
                Ok(bind) => {
                    if !seen_keys.insert(bind.key) {
                        ctx.emit_error(DecodeError::unexpected(
                            &child.node_name,
                            "keybind",
                            "duplicate keybind",
                        ));
                        continue;
                    }
                    binds.push(bind);
                }
                Err(e) => ctx.emit_error(e),
            }
        }
        Ok(Self(binds))
    }
}

impl<S> knuffel::Decode<S> for MruOpenedBind
where
    S: knuffel::traits::ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        if let Some(type_name) = &node.type_name {
            ctx.emit_error(DecodeError::unexpected(
                type_name,
                "type name",
                "no type name expected for this node",
            ));
        }
        for val in node.arguments.iter() {
            ctx.emit_error(DecodeError::unexpected(
                &val.literal,
                "argument",
                "no arguments expected for this node",
            ));
        }
        for (name, _) in &node.properties {
            ctx.emit_error(DecodeError::unexpected(
                name,
                "property",
                "no properties expected for this node",
            ));
        }

        let key = node
            .node_name
            .parse::<Key>()
            .map_err(|e| DecodeError::conversion(&node.node_name, e.wrap_err("invalid keybind")))?;
        if !key.modifiers.is_empty() {
            ctx.emit_error(DecodeError::unexpected(
                &node.node_name,
                "keybind",
                "a key alone: the modifier is the one holding the switcher open",
            ));
        }
        if !matches!(key.trigger, Trigger::Keysym(_)) {
            ctx.emit_error(DecodeError::unexpected(
                &node.node_name,
                "key",
                "key must be a keyboard key",
            ));
        }

        let dummy = Self {
            key,
            action: MruOpenedAction::None,
        };
        let mut children = node.children();
        let Some(child) = children.next() else {
            ctx.emit_error(DecodeError::missing(
                node,
                "expected an action for this keybind",
            ));
            return Ok(dummy);
        };
        for unwanted_child in children {
            ctx.emit_error(DecodeError::unexpected(
                unwanted_child,
                "node",
                "only one action is allowed per keybind",
            ));
        }
        match MruOpenedAction::decode_node(child, ctx) {
            Ok(MruOpenedAction::SpawnForWindow(command)) if command.is_empty() => {
                ctx.emit_error(DecodeError::missing(child, "expected a command to run"));
                Ok(dummy)
            }
            Ok(action) => Ok(Self { key, action }),
            Err(e) => {
                ctx.emit_error(e);
                Ok(dummy)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    fn opened(config: &Config, key: &str) -> Option<Action> {
        let key: Key = key.parse().unwrap();
        config
            .recent_windows
            .opened_binds
            .iter()
            .find(|bind| bind.key == key)
            .map(|bind| bind.action.clone())
    }

    #[test]
    fn opened_binds_default_to_numbered_scopes() {
        let config = Config::parse_mem("").unwrap();
        assert_eq!(
            opened(&config, "1"),
            Some(Action::MruSetScope(MruScope::All))
        );
        assert_eq!(
            opened(&config, "2"),
            Some(Action::MruSetScope(MruScope::Workspace))
        );
        assert_eq!(
            opened(&config, "3"),
            Some(Action::MruSetScope(MruScope::Output))
        );
        assert_eq!(opened(&config, "S"), Some(Action::MruCycleScope));
        assert_eq!(opened(&config, "Escape"), Some(Action::MruCancel));
        // The letters are free for the user's own keys.
        for key in ["A", "O", "W", "Q"] {
            assert_eq!(opened(&config, key), None, "{key}");
        }
    }

    #[test]
    fn opened_binds_replace_add_and_remove_by_key() {
        let config = Config::parse_mem(
            r#"recent-windows { opened-binds {
                W { close-window; }
                Q { spawn-for-window "quit-app" "--id"; }
                S { none; }
                "1" { set-scope "workspace"; }
            }; }"#,
        )
        .unwrap();
        assert_eq!(opened(&config, "W"), Some(Action::MruCloseCurrentWindow));
        assert_eq!(
            opened(&config, "Q"),
            Some(Action::MruSpawnForCurrentWindow(vec![
                "quit-app".into(),
                "--id".into()
            ]))
        );
        assert_eq!(opened(&config, "S"), None);
        assert_eq!(
            opened(&config, "1"),
            Some(Action::MruSetScope(MruScope::Workspace))
        );
        assert_eq!(opened(&config, "Return"), Some(Action::MruConfirm));
        let ones = config
            .recent_windows
            .opened_binds
            .iter()
            .filter(|b| b.key == "1".parse().unwrap());
        assert_eq!(ones.count(), 1);
    }

    #[test]
    fn opened_binds_reject_modifiers_and_empty_commands() {
        assert!(
            Config::parse_mem("recent-windows { opened-binds { Alt+W { close-window; }; }; }")
                .is_err()
        );
        assert!(
            Config::parse_mem("recent-windows { opened-binds { Q { spawn-for-window; }; }; }")
                .is_err()
        );
        assert!(Config::parse_mem(
            "recent-windows { opened-binds { W { close-window; }; W { cancel; }; }; }"
        )
        .is_err());
    }

    #[test]
    fn the_stack_is_shaped_by_flip() {
        let config = Config::parse_mem(
            "recent-windows { flip { tilt-x -30; tilt-y -5; spacing 10; front-x 50; }; }",
        )
        .unwrap();
        let flip = config.recent_windows.flip;
        assert_eq!((flip.tilt_x, flip.tilt_y, flip.spacing), (-30., -5., 10.));
        assert_eq!((flip.front_x, flip.front_y), (50., 60.));
        assert_eq!((flip.offset_x, flip.offset_y), (0., 0.));
        assert_eq!(
            (flip.camera_x, flip.camera_y, flip.card_size),
            (-15., -32., 43.)
        );
        let config = Config::parse_mem(
            "recent-windows { flip { camera-x -15; camera-y -32; card-size 43; }; }",
        )
        .unwrap();
        let flip = config.recent_windows.flip;
        assert_eq!(
            (flip.camera_x, flip.camera_y, flip.card_size),
            (-15., -32., 43.)
        );
        assert!(Config::parse_mem("recent-windows { flip { card-size 2; }; }").is_err());
        assert_eq!(flip.visible_windows, 10.);
        let config = Config::parse_mem("recent-windows { flip { visible-windows 4; }; }").unwrap();
        assert_eq!(config.recent_windows.flip.visible_windows, 4.);
        assert!(Config::parse_mem("recent-windows { flip { visible-windows 0; }; }").is_err());
        let config =
            Config::parse_mem("recent-windows { flip { offset-x -8; offset-y 4; }; }").unwrap();
        let flip = config.recent_windows.flip;
        assert_eq!((flip.offset_x, flip.offset_y), (-8., 4.));
        // The older names still load: tilt is tilt-x, the shifts are ignored.
        let config =
            Config::parse_mem("recent-windows { flip { tilt -12; shift-x 10; shift-y -8; }; }")
                .unwrap();
        let flip = config.recent_windows.flip;
        assert_eq!((flip.tilt_x, flip.tilt_y), (-12., 2.));
        assert!(Config::parse_mem("recent-windows { flip { tilt-x 90; }; }").is_err());
        assert!(Config::parse_mem("recent-windows { flip { front-x 101; }; }").is_err());
    }

    #[test]
    fn style_picks_icons_previews_or_flip() {
        let style = |kdl: &str| {
            let previews = Config::parse_mem(kdl).unwrap().recent_windows.previews;
            (previews.icons_only, previews.flip)
        };
        assert_eq!(
            style("recent-windows { previews { style \"flip\"; }; }"),
            (false, true)
        );
        assert_eq!(
            style("recent-windows { previews { style \"icons\"; }; }"),
            (true, false)
        );
        assert_eq!(
            style("recent-windows { previews { style \"previews\"; }; }"),
            (false, false)
        );
        // Beside the older icons-only, style wins.
        assert_eq!(
            style("recent-windows { previews { icons-only true; style \"flip\"; }; }"),
            (false, true)
        );
        // A later icons-only (another file) replaces an earlier style.
        let mut previews = MruPreviews::default();
        previews.merge_with(&MruPreviewsPart {
            style: Some(MruStyle::Flip),
            ..Default::default()
        });
        assert!(previews.flip);
        previews.merge_with(&MruPreviewsPart {
            icons_only: Some(false),
            ..Default::default()
        });
        assert!(!previews.flip && !previews.icons_only);
        assert!(Config::parse_mem("recent-windows { previews { style \"cube\"; }; }").is_err());
    }

    #[test]
    fn the_container_can_fill_the_screen_with_its_own_tint_and_blur() {
        let container = Config::parse_mem("").unwrap().recent_windows.container;
        assert!(!container.fullscreen);
        assert_eq!(
            (container.overlay_opacity, container.blur_radius),
            (None, None)
        );
        let config = Config::parse_mem(
            "recent-windows { container { fullscreen true; overlay-opacity 0.7; blur-radius 40; }; }",
        )
        .unwrap();
        let container = config.recent_windows.container;
        assert!(container.fullscreen);
        assert_eq!(
            (container.overlay_opacity, container.blur_radius),
            (Some(0.7), Some(40.))
        );
        assert!(Config::parse_mem("recent-windows { container { overlay-opacity 2; }; }").is_err());
    }

    #[test]
    fn roundness_is_concentric_and_margins_configurable() {
        let config = Config::parse_mem(
            "recent-windows { highlight { corner-radius 14; border-width 3; }; container { padding 10; margin-x 20; margin-y 0; }; }",
        )
        .unwrap();
        let recent = &config.recent_windows;
        assert_eq!(recent.highlight.border_width, 3.);
        assert_eq!(recent.container.margin_x, 20.);
        assert_eq!(recent.container.margin_y, 0.);
        // The highlight's outer radius plus the padding between them.
        assert_eq!(recent.container_radius(), 14. + 3. + 10.);
        let config =
            Config::parse_mem("recent-windows { container { corner-radius 9; }; }").unwrap();
        assert_eq!(config.recent_windows.container_radius(), 9.);
        assert!(Config::parse_mem("recent-windows { container { margin-x -1; }; }").is_err());
    }

    #[test]
    fn container_tint_and_title_colour_are_configurable() {
        let config = Config::parse_mem("").unwrap();
        assert_eq!(config.recent_windows.container, MruContainer::default());
        assert_eq!(config.recent_windows.container.background_opacity, 0.35);
        let config = Config::parse_mem(
            r##"recent-windows { container { background-color "#ffffff"; background-opacity 0.6; title-color "#1d1d1f"; }; }"##,
        )
        .unwrap();
        let c = config.recent_windows.container;
        assert_eq!(c.background_color, Color::new_unpremul(1., 1., 1., 1.));
        assert_eq!(c.background_opacity, 0.6);
        assert_eq!(c.title_color, "#1d1d1f".parse().unwrap());
    }

    #[test]
    fn thumbnail_antialiasing_defaults_on_and_merges_independently() {
        let mut previews = MruPreviews::default();
        assert!(previews.antialiasing);
        previews.merge_with(&MruPreviewsPart {
            max_scale: Some(FloatOrInt(0.22)),
            antialiasing: Some(false),
            ..Default::default()
        });
        assert!(!previews.antialiasing);
        previews.merge_with(&MruPreviewsPart {
            antialiasing: Some(true),
            ..Default::default()
        });
        assert!(previews.antialiasing);
        assert_eq!(previews.max_scale, 0.22);
        let config =
            Config::parse_mem("recent-windows { previews { antialiasing false; }; }").unwrap();
        assert!(!config.recent_windows.previews.antialiasing);
        assert!(Config::parse_mem("recent-windows { previews { antialiasing 2; }; }").is_err());
    }

    #[test]
    fn icons_only_defaults_off_and_parses() {
        let previews = MruPreviews::default();
        assert!(!previews.icons_only);
        assert_eq!(previews.icon_size, 96.);
        let config =
            Config::parse_mem("recent-windows { previews { icons-only true; icon-size 128; }; }")
                .unwrap();
        assert!(config.recent_windows.previews.icons_only);
        assert_eq!(config.recent_windows.previews.icon_size, 128.);
        assert!(Config::parse_mem("recent-windows { previews { icon-size 8; }; }").is_err());
    }

    #[test]
    fn label_defaults_by_mode_and_parses() {
        let label = MruLabel::default();
        assert_eq!(label.text_for(false), MruLabelText::Title);
        assert_eq!(label.text_for(true), MruLabelText::AppName);
        assert!(!label.selected_only(false));
        assert!(label.selected_only(true));
        assert_eq!(label.font, "sans 14px");
        assert_eq!(label.gap, 8.);

        let config = Config::parse_mem(
            r#"recent-windows {
                label { text "app-and-title"; show "all"; font "Inter 13px"; gap 4; }
                previews { gap 24; }
                container { padding 20; }
            }"#,
        )
        .unwrap();
        let recent = &config.recent_windows;
        assert_eq!(recent.label.text_for(true), MruLabelText::AppAndTitle);
        assert!(!recent.label.selected_only(true));
        assert_eq!(recent.label.font, "Inter 13px");
        assert_eq!(recent.label.gap, 4.);
        assert_eq!(recent.previews.gap, 24.);
        assert_eq!(recent.container.padding, 20.);

        // A later part keeps earlier choices it does not mention.
        let mut label = recent.label.clone();
        label.merge_with(&MruLabelPart {
            text: Some(MruLabelText::None),
            ..Default::default()
        });
        assert_eq!(label.text_for(false), MruLabelText::None);
        assert!(!label.selected_only(true));

        assert!(Config::parse_mem(r#"recent-windows { label { text "app"; }; }"#).is_err());
        assert!(Config::parse_mem(r#"recent-windows { label { show "some"; }; }"#).is_err());
        assert!(Config::parse_mem("recent-windows { previews { gap -1; }; }").is_err());
    }

    #[test]
    fn wrap_defaults_on_in_either_mode_and_can_be_turned_off() {
        let mut previews = MruPreviews::default();
        assert!(previews.wraps());
        previews.icons_only = true;
        assert!(previews.wraps());
        previews.merge_with(&MruPreviewsPart {
            wrap: Some(true),
            ..Default::default()
        });
        assert!(previews.wraps());
        // Unrelated parts keep the explicit choice.
        previews.merge_with(&MruPreviewsPart::default());
        assert_eq!(previews.wrap, Some(true));
        let config = Config::parse_mem("recent-windows { previews { wrap false; }; }").unwrap();
        assert!(!config.recent_windows.previews.wraps());
    }

    #[test]
    fn highlight_defaults_preserve_tuned_appearance_and_legacy_color() {
        let config =
            Config::parse_mem("recent-windows { highlight { active-color \"#00ff00\"; }; }")
                .unwrap();
        let highlight = config.recent_windows.highlight;
        assert_eq!(highlight.border_color, None);
        assert_eq!(highlight.active_color, Color::new_unpremul(0., 1., 0., 1.));
        assert_eq!(highlight.border_opacity, 1.);
        assert_eq!(
            highlight.background_color,
            Color::new_unpremul(0., 0., 0., 1.)
        );
        assert_eq!(highlight.background_opacity, 0.2);
    }

    #[test]
    fn highlight_settings_merge_independently() {
        let mut config = Config::parse_mem(
            r##"
            recent-windows {
                highlight {
                    border-color "#ff000080"
                    background-color "#0000ff"
                    border-opacity 0.75
                    background-opacity 0.3
                    padding 16
                }
                previews { max-scale 0.24; }
            }
        "##,
        )
        .unwrap();
        assert_eq!(config.recent_windows.highlight.border_opacity, 0.75);
        assert_eq!(config.recent_windows.highlight.background_opacity, 0.3);
        // The same partial merge is used for subsequent included files.
        config
            .recent_windows
            .highlight
            .merge_with(&MruHighlightPart {
                border_opacity: Some(FloatOrInt(0.25)),
                background_opacity: Some(FloatOrInt(0.5)),
                ..Default::default()
            });
        let highlight = config.recent_windows.highlight;
        assert_eq!(
            highlight.border_color,
            Some(Color::from_rgba8_unpremul(255, 0, 0, 128))
        );
        assert_eq!(highlight.border_opacity, 0.25);
        assert_eq!(
            highlight.background_color,
            Color::new_unpremul(0., 0., 1., 1.)
        );
        assert_eq!(highlight.background_opacity, 0.5);
        assert_eq!(highlight.padding, 16.);
        assert_eq!(config.recent_windows.previews.max_scale, 0.24);
    }

    #[test]
    fn highlight_opacity_accepts_endpoints_and_rejects_invalid_values() {
        for key in ["border-opacity", "background-opacity"] {
            for value in ["0", "1", "0.0", "1.0", "0.5"] {
                assert!(Config::parse_mem(&format!(
                    "recent-windows {{ highlight {{ {key} {value}; }}; }}"
                ))
                .is_ok());
            }
            for value in ["-0.1", "1.1", "2", "\"50%\"", "true"] {
                assert!(Config::parse_mem(&format!(
                    "recent-windows {{ highlight {{ {key} {value}; }}; }}"
                ))
                .is_err());
            }
        }
        assert!(Config::parse_mem(
            "recent-windows { highlight { border-color \"not-a-color\"; }; }"
        )
        .is_err());
        assert!(
            Config::parse_mem("recent-windows { highlight { background-color \"#xyz\"; }; }")
                .is_err()
        );
    }
}
