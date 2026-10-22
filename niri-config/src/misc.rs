use crate::appearance::{
    Color, OverviewBackdropBlur, OverviewBackdropBlurPart, WorkspaceHighlight,
    WorkspaceHighlightPart, WorkspaceShadow, WorkspaceShadowPart, DEFAULT_BACKDROP_COLOR,
};
use crate::utils::{Flag, MergeWith};
use crate::FloatOrInt;

#[derive(knuffel::Decode, Debug, Clone, PartialEq, Eq)]
pub struct SpawnAtStartup {
    #[knuffel(arguments)]
    pub command: Vec<String>,
}

#[derive(knuffel::Decode, Debug, Clone, PartialEq, Eq)]
pub struct SpawnShAtStartup {
    #[knuffel(argument)]
    pub command: String,
}

#[derive(Debug, PartialEq)]
pub struct Cursor {
    pub xcursor_theme: String,
    pub xcursor_size: u8,
    pub hide_when_typing: bool,
    pub hide_after_inactive_ms: Option<u32>,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            xcursor_theme: String::from("default"),
            xcursor_size: 24,
            hide_when_typing: false,
            hide_after_inactive_ms: None,
        }
    }
}

#[derive(knuffel::Decode, Debug, PartialEq)]
pub struct CursorPart {
    #[knuffel(child, unwrap(argument))]
    pub xcursor_theme: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub xcursor_size: Option<u8>,
    #[knuffel(child)]
    pub hide_when_typing: Option<Flag>,
    #[knuffel(child, unwrap(argument))]
    pub hide_after_inactive_ms: Option<u32>,
}

impl MergeWith<CursorPart> for Cursor {
    fn merge_with(&mut self, part: &CursorPart) {
        merge_clone!((self, part), xcursor_theme, xcursor_size);
        merge!((self, part), hide_when_typing);
        merge_clone_opt!((self, part), hide_after_inactive_ms);
    }
}

#[derive(knuffel::Decode, Debug, Clone, PartialEq)]
pub struct ScreenshotPath(#[knuffel(argument)] pub Option<String>);

impl Default for ScreenshotPath {
    fn default() -> Self {
        Self(Some(String::from(
            "~/Pictures/Screenshots/Screenshot from %Y-%m-%d %H-%M-%S.png",
        )))
    }
}

/// Preferences for the native interactive screenshot UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenshotUi {
    pub remember_selection: bool,
    pub show_panel: bool,
    pub show_pointer: bool,
    pub capture_on_release: bool,
}

impl Default for ScreenshotUi {
    fn default() -> Self {
        Self {
            remember_selection: false,
            show_panel: false,
            show_pointer: false,
            capture_on_release: true,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScreenshotUiPart {
    #[knuffel(child, unwrap(argument))]
    pub remember_selection: Option<bool>,
    #[knuffel(child, unwrap(argument))]
    pub show_panel: Option<bool>,
    #[knuffel(child, unwrap(argument))]
    pub show_pointer: Option<bool>,
    #[knuffel(child, unwrap(argument))]
    pub capture_on_release: Option<bool>,
}

impl MergeWith<ScreenshotUiPart> for ScreenshotUi {
    fn merge_with(&mut self, part: &ScreenshotUiPart) {
        merge_clone!(
            (self, part),
            remember_selection,
            show_panel,
            show_pointer,
            capture_on_release
        );
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyOverlay {
    pub skip_at_startup: bool,
    pub hide_not_bound: bool,
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyOverlayPart {
    #[knuffel(child)]
    pub skip_at_startup: Option<Flag>,
    #[knuffel(child)]
    pub hide_not_bound: Option<Flag>,
}

impl MergeWith<HotkeyOverlayPart> for HotkeyOverlay {
    fn merge_with(&mut self, part: &HotkeyOverlayPart) {
        merge!((self, part), skip_at_startup, hide_not_bound);
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConfigNotification {
    pub disable_failed: bool,
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConfigNotificationPart {
    #[knuffel(child)]
    pub disable_failed: Option<Flag>,
}

impl MergeWith<ConfigNotificationPart> for ConfigNotification {
    fn merge_with(&mut self, part: &ConfigNotificationPart) {
        merge!((self, part), disable_failed);
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Clipboard {
    pub disable_primary: bool,
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardPart {
    #[knuffel(child)]
    pub disable_primary: Option<Flag>,
}

impl MergeWith<ClipboardPart> for Clipboard {
    fn merge_with(&mut self, part: &ClipboardPart) {
        merge!((self, part), disable_primary);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Overview {
    pub zoom: f64,
    pub backdrop_color: Color,
    pub workspace_shadow: WorkspaceShadow,
    pub backdrop_blur: OverviewBackdropBlur,
    pub workspace_highlight: WorkspaceHighlight,
}

impl Default for Overview {
    fn default() -> Self {
        Self {
            zoom: 0.5,
            backdrop_color: DEFAULT_BACKDROP_COLOR,
            workspace_shadow: WorkspaceShadow::default(),
            backdrop_blur: OverviewBackdropBlur::default(),
            workspace_highlight: WorkspaceHighlight::default(),
        }
    }
}

#[derive(knuffel::Decode, Debug, Clone, Copy, PartialEq)]
pub struct OverviewPart {
    #[knuffel(child, unwrap(argument))]
    pub zoom: Option<FloatOrInt<0, 1>>,
    #[knuffel(child)]
    pub backdrop_color: Option<Color>,
    #[knuffel(child)]
    pub workspace_shadow: Option<WorkspaceShadowPart>,
    #[knuffel(child)]
    pub backdrop_blur: Option<OverviewBackdropBlurPart>,
    #[knuffel(child)]
    pub workspace_highlight: Option<WorkspaceHighlightPart>,
}

impl MergeWith<OverviewPart> for Overview {
    fn merge_with(&mut self, part: &OverviewPart) {
        merge!(
            (self, part),
            zoom,
            workspace_shadow,
            backdrop_blur,
            workspace_highlight
        );
        merge_clone!((self, part), backdrop_color);
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq, Eq)]
pub struct Environment(#[knuffel(children)] pub Vec<EnvironmentVariable>);

#[derive(knuffel::Decode, Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentVariable {
    #[knuffel(node_name)]
    pub name: String,
    #[knuffel(argument)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XwaylandSatellite {
    pub off: bool,
    pub path: String,
}

impl Default for XwaylandSatellite {
    fn default() -> Self {
        Self {
            off: false,
            path: String::from("xwayland-satellite"),
        }
    }
}

#[derive(knuffel::Decode, Debug, Clone, PartialEq, Eq)]
pub struct XwaylandSatellitePart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(argument))]
    pub path: Option<String>,
}

impl MergeWith<XwaylandSatellitePart> for XwaylandSatellite {
    fn merge_with(&mut self, part: &XwaylandSatellitePart) {
        self.off |= part.off;
        if part.on {
            self.off = false;
        }

        merge_clone!((self, part), path);
    }
}

#[cfg(test)]
mod tests {
    use crate::{Color, Config};

    #[test]
    fn overview_backdrop_blur_and_workspace_highlight() {
        let config = Config::parse_mem("").unwrap();
        assert!(config.overview.backdrop_blur.off);
        assert!(config.overview.workspace_highlight.off);

        let config = Config::parse_mem(
            r##"
            overview {
                backdrop-blur {
                    radius 48
                    saturation 1.2
                }
                workspace-highlight {
                    border-width 2
                    active-border-color "#ff000080"
                    inactive-border-opacity 0.4
                }
            }
            "##,
        )
        .unwrap();
        // Setting anything in a block doesn't turn it on; `on` does.
        assert!(config.overview.backdrop_blur.off);
        assert_eq!(config.overview.backdrop_blur.radius, 48.);
        assert_eq!(config.overview.backdrop_blur.saturation, 1.2);
        assert_eq!(config.overview.backdrop_blur.noise, 0.02);
        let h = config.overview.workspace_highlight;
        assert_eq!(h.border_width, 2.);
        // A colour's own alpha is ignored: the opacity is the only transparency.
        assert_eq!(
            h.active_border().a,
            0.5,
            "the default opacity, not the colour's 0x80"
        );
        assert_eq!(h.inactive_border_opacity, 0.4);
        assert_eq!(
            h.inactive_border(),
            Color::from_rgba8_unpremul(255, 255, 255, 0x66)
        );
    }
}

/// Whole-screen zoom (a screen magnifier), driven by the modifier held with a touchpad pinch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenZoom {
    /// No screen zoom: pinches go to clients (or the overview) as before.
    pub off: bool,
    /// The highest zoom level.
    pub max_zoom: f64,
    /// The modifiers to hold down while pinching. `Modifiers::COMPOSITOR` is the Mod key.
    pub modifiers: crate::binds::Modifiers,
    /// The cursor grows with the zoom up to this scale.
    pub max_cursor_scale: f64,
}

impl Default for ScreenZoom {
    fn default() -> Self {
        Self {
            off: false,
            max_zoom: 16.,
            modifiers: crate::binds::Modifiers::COMPOSITOR,
            max_cursor_scale: 3.,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct ScreenZoomPart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(argument))]
    pub max_zoom: Option<FloatOrInt<1, 16>>,
    #[knuffel(child, unwrap(argument, str))]
    pub modifier: Option<ModifierSet>,
    #[knuffel(child, unwrap(argument))]
    pub max_cursor_scale: Option<FloatOrInt<1, 16>>,
}

impl MergeWith<ScreenZoomPart> for ScreenZoom {
    fn merge_with(&mut self, part: &ScreenZoomPart) {
        self.off |= part.off;
        if part.on {
            self.off = false;
        }
        merge!((self, part), max_zoom, max_cursor_scale);
        if let Some(modifier) = part.modifier {
            self.modifiers = modifier.0;
        }
    }
}

/// Modifier keys joined with `+`, as in a bind without its key: `"Mod"`, `"Ctrl+Alt"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModifierSet(pub crate::binds::Modifiers);

impl std::str::FromStr for ModifierSet {
    type Err = miette::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use crate::binds::Modifiers;

        let mut modifiers = Modifiers::empty();
        for part in s.split('+') {
            let part = part.trim().to_ascii_lowercase();
            modifiers |= match part.as_str() {
                "mod" => Modifiers::COMPOSITOR,
                "ctrl" | "control" => Modifiers::CTRL,
                "shift" => Modifiers::SHIFT,
                "alt" => Modifiers::ALT,
                "super" | "win" => Modifiers::SUPER,
                "iso_level3_shift" | "mod5" => Modifiers::ISO_LEVEL3_SHIFT,
                "iso_level5_shift" | "mod3" => Modifiers::ISO_LEVEL5_SHIFT,
                _ => return Err(miette::miette!("invalid modifier: {part:?}")),
            };
        }
        Ok(Self(modifiers))
    }
}
