//! Placements for windows that are about to open (`Request::PlaceWindows`).
//!
//! A session restorer relaunches apps and tells niri where each of their windows was. Instead of
//! opening on the focused workspace and being moved afterwards (which needs focus, and sweeps the
//! view across every workspace), a matched window opens directly on its workspace, in its column,
//! at its size, and without taking focus. Only the window that had focus gets it back, and only
//! if the user has not started doing something else in the meantime.
//!
//! This module holds the matching and ordering logic; the compositor glue is in
//! `handlers/compositor.rs` (mapping) and `handlers/xdg_shell.rs` (initial configure).

use std::time::{Duration, Instant};

use niri_config::PresetSize;
use niri_ipc::{WindowPlacement, WindowPlacements, WorkspaceReferenceArg};
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::wayland::shell::xdg::ToplevelSurface;

use crate::layout::workspace::WorkspaceId;
use crate::layout::ActivateWindow;
use crate::niri::State;
use crate::utils::{get_credentials_for_surface, output_matches_name, with_toplevel_role};
use crate::window::ResolvedWindowRules;

/// Environment variable carrying a placement token into the launched app.
pub const TOKEN_VARIABLE: &str = "NIRI_PLACEMENT_TOKEN";

/// What we know about a window that is opening.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub token: Option<String>,
    pub app_id: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug)]
struct Entry<W> {
    placement: WindowPlacement,
    window: Option<W>,
}

/// Pending placements and the windows that already took theirs.
#[derive(Debug)]
pub struct Placements<W> {
    entries: Vec<Entry<W>>,
    deadline: Instant,
    user_acted: bool,
    /// Another app's window came to the front first; it stays there.
    other_in_front: bool,
    /// Saved (output, workspace index) -> the workspace that now holds that group.
    groups: Vec<(Option<String>, u8, WorkspaceId)>,
}

/// Where a tiled window goes on its workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnTarget<W> {
    /// No restored neighbours yet: let the layout decide.
    Auto,
    /// A new column at this 0-based index.
    NewColumn(usize),
    /// Into the column of this window, at this 0-based position.
    InColumn(W, usize),
}

/// A restored window already on the target workspace: its saved and current position.
#[derive(Debug, Clone)]
pub struct Placed<W> {
    pub window: W,
    pub column: u32,
    pub tile: u32,
    /// Current 0-based column and position in it.
    pub at: (usize, usize),
}

impl<W: Clone + PartialEq> Placements<W> {
    pub fn new(request: WindowPlacements, now: Instant) -> Option<Self> {
        if request.placements.is_empty() {
            return None;
        }
        Some(Self {
            entries: request
                .placements
                .into_iter()
                .map(|placement| Entry {
                    placement,
                    window: None,
                })
                .collect(),
            deadline: now + Duration::from_millis(u64::from(request.timeout_ms)),
            user_acted: false,
            other_in_front: false,
            groups: Vec::new(),
        })
    }

    /// Whether any placement still waits for its window.
    pub fn is_live(&self, now: Instant) -> bool {
        now < self.deadline && self.entries.iter().any(|e| e.window.is_none())
    }

    /// The user pressed a key or button: restored windows no longer take focus.
    pub fn note_user_input(&mut self) {
        self.user_acted = true;
    }

    /// A window that is not being restored opened (or was already in front): restored windows
    /// leave it in front.
    pub fn note_other_window(&mut self) {
        self.other_in_front = true;
    }

    /// Whether a window comes from one of the relaunched apps (an extra window, a splash
    /// screen) rather than from another app.
    pub fn is_restored_app(&self, window: &Candidate) -> bool {
        window.token.as_ref().is_some_and(|token| {
            self.entries
                .iter()
                .any(|e| e.placement.token.as_ref() == Some(token))
        })
    }

    /// Whether a window placed with `focus` should be activated.
    pub fn may_focus(&self, placement: &WindowPlacement) -> bool {
        placement.focus && !self.user_acted && !self.other_in_front
    }

    /// The placement of the window that had focus.
    pub fn focused(&self) -> Option<&WindowPlacement> {
        self.entries.iter().map(|e| &e.placement).find(|p| p.focus)
    }

    /// The best free placement for a window, without taking it.
    pub fn find(&self, window: &Candidate) -> Option<usize> {
        let mut best: Option<(usize, u8)> = None;
        for (idx, entry) in self.entries.iter().enumerate() {
            if entry.window.is_some() {
                continue;
            }
            let Some(score) = score(&entry.placement, window) else {
                continue;
            };
            if best.is_none_or(|(_, s)| score > s) {
                best = Some((idx, score));
            }
        }
        best.map(|(idx, _)| idx)
    }

    pub fn placement(&self, idx: usize) -> &WindowPlacement {
        &self.entries[idx].placement
    }

    /// Give placement `idx` to a window that is opening.
    pub fn take(&mut self, idx: usize, window: W) {
        self.entries[idx].window = Some(window);
    }

    /// Restored windows on the same saved workspace as placement `idx`, other than its own.
    pub fn neighbours(&self, idx: usize) -> impl Iterator<Item = (&W, &WindowPlacement)> + '_ {
        let want = &self.entries[idx].placement;
        self.entries
            .iter()
            .enumerate()
            .filter(move |(i, e)| {
                *i != idx
                    && !e.placement.floating
                    && e.placement.output == want.output
                    && e.placement.workspace == want.workspace
            })
            .filter_map(|(_, e)| e.window.as_ref().map(|w| (w, &e.placement)))
    }

    /// Workspace already holding the saved workspace `index` on `output`.
    pub fn group(&self, output: &Option<String>, index: u8) -> Option<WorkspaceId> {
        self.groups
            .iter()
            .find(|(o, i, _)| o == output && *i == index)
            .map(|(_, _, id)| *id)
    }

    /// Saved index -> workspace for the other groups on `output`.
    pub fn groups_on(
        &self,
        output: &Option<String>,
    ) -> impl Iterator<Item = (u8, WorkspaceId)> + '_ {
        let output = output.clone();
        self.groups
            .iter()
            .filter(move |(o, _, _)| *o == output)
            .map(|(_, i, id)| (*i, *id))
    }

    pub fn set_group(&mut self, output: Option<String>, index: u8, id: WorkspaceId) {
        self.groups
            .retain(|(o, i, _)| !(*o == output && *i == index));
        self.groups.push((output, index, id));
    }
}

/// How well a placement fits a window; `None` if it can't be its.
fn score(placement: &WindowPlacement, window: &Candidate) -> Option<u8> {
    let app_matches = match (&placement.app_id, &window.app_id) {
        (Some(want), Some(got)) => want == got,
        (Some(_), None) => false,
        (None, _) => true,
    };
    let mut score = match (&placement.token, &window.token) {
        // Launched for a placement: only that placement's own.
        (Some(want), Some(got)) if want == got => 4,
        (_, Some(_)) => return None,
        // No token (D-Bus activation, a launcher that drops the environment): by app id only.
        (_, None) if placement.app_id.is_some() && app_matches => 1,
        (_, None) => return None,
    };
    if placement.app_id.is_some() {
        if !app_matches {
            return None;
        }
        score += 1;
    }
    if placement.title.is_some() && placement.title == window.title {
        score += 2;
    }
    Some(score)
}

/// Where a tiled window saved at (`column`, `tile`) goes among the restored windows already on
/// its workspace, so that their saved order holds whatever order they open in.
pub fn column_target<W: Clone>(column: u32, tile: u32, placed: &[Placed<W>]) -> ColumnTarget<W> {
    let same: Vec<_> = placed.iter().filter(|p| p.column == column).collect();
    if let Some(first) = same.first() {
        let above = same
            .iter()
            .filter(|p| p.tile < tile)
            .map(|p| p.at.1 + 1)
            .max();
        let below = same.iter().filter(|p| p.tile >= tile).map(|p| p.at.1).min();
        let at = above.or(below).unwrap_or(0);
        return ColumnTarget::InColumn(first.window.clone(), at);
    }
    let left = placed
        .iter()
        .filter(|p| p.column < column)
        .map(|p| p.at.0 + 1)
        .max();
    let right = placed
        .iter()
        .filter(|p| p.column > column)
        .map(|p| p.at.0)
        .min();
    match left.or(right) {
        Some(idx) => ColumnTarget::NewColumn(idx),
        None => ColumnTarget::Auto,
    }
}

/// Where a new workspace for saved index `index` goes on a monitor, given the current indices of
/// the workspaces holding the other saved indices there.
///
/// `None` means: use the monitor's last workspace, which is always empty.
pub fn workspace_slot(index: u8, others: &[(u8, usize)]) -> Option<usize> {
    let after = others
        .iter()
        .filter(|(i, _)| *i < index)
        .map(|(_, at)| at + 1)
        .max();
    let before = others
        .iter()
        .filter(|(i, _)| *i > index)
        .map(|(_, at)| *at)
        .min();
    match (after, before) {
        // Between two restored workspaces, or before the first one: a new one there.
        (_, Some(before)) => Some(after.map_or(before, |a| a.min(before))),
        // After the last restored workspace.
        (Some(after), None) => Some(after),
        (None, None) => None,
    }
}

/// Reference to the saved workspace a placement names, if it is an index.
pub fn saved_index(placement: &WindowPlacement) -> Option<u8> {
    match placement.workspace {
        Some(WorkspaceReferenceArg::Index(idx)) => Some(idx),
        _ => None,
    }
}

/// Read `NIRI_PLACEMENT_TOKEN` from a process's environment.
pub fn token_of_pid(pid: i32) -> Option<String> {
    let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    environ.split(|&b| b == 0).find_map(|entry| {
        let entry = std::str::from_utf8(entry).ok()?;
        let (key, value) = entry.split_once('=')?;
        (key == TOKEN_VARIABLE).then(|| value.to_owned())
    })
}

/// What we know about an opening toplevel.
pub fn candidate(toplevel: &ToplevelSurface) -> Candidate {
    let (app_id, title) =
        with_toplevel_role(toplevel, |role| (role.app_id.clone(), role.title.clone()));
    let token =
        get_credentials_for_surface(toplevel.wl_surface()).and_then(|c| token_of_pid(c.pid));
    Candidate {
        token,
        app_id,
        title,
    }
}

/// Before the initial configure: the placement a window will take, if any, without taking it.
pub fn peek(state: &mut State, toplevel: &ToplevelSurface) -> Option<WindowPlacement> {
    let placements = live(state)?;
    let idx = placements.find(&candidate(toplevel))?;
    Some(placements.placement(idx).clone())
}

/// Give a window that will take a placement its saved size and state, so that its first frame
/// is already right.
pub fn apply_to_initial_rules(placement: &WindowPlacement, rules: &mut ResolvedWindowRules) {
    if let Some(output) = &placement.output {
        rules.open_on_output = Some(output.clone());
    }
    if let Some(WorkspaceReferenceArg::Name(name)) = &placement.workspace {
        rules.open_on_workspace = Some(name.clone());
    }
    rules.open_floating = Some(placement.floating);
    rules.open_maximized_to_edges = Some(placement.maximized);
    rules.open_maximized = Some(false);
    rules.open_fullscreen = Some(false);
    if let (Some(width), false) = (placement.width, placement.maximized) {
        rules.default_width = Some(Some(PresetSize::Fixed(width)));
    }
}

fn live(state: &mut State) -> Option<&mut Placements<Window>> {
    let now = Instant::now();
    if !state.niri.window_placements.as_ref()?.is_live(now) {
        state.niri.window_placements = None;
        return None;
    }
    state.niri.window_placements.as_mut()
}

/// Where a restored window goes in the layout.
#[derive(Debug)]
pub enum Target {
    Workspace(WorkspaceId),
    WorkspaceColumn(WorkspaceId, usize),
    InColumn(Window, usize),
    Output(Output),
}

/// A window that took a placement as it maps.
#[derive(Debug)]
pub struct Restored {
    pub target: Option<Target>,
    pub width: Option<PresetSize>,
    pub floating: bool,
    pub maximized: bool,
    pub activate: ActivateWindow,
}

/// As a window maps: if it is one of the awaited windows, take its placement and work out where
/// it goes.
pub fn place_mapping_window(
    state: &mut State,
    toplevel: &ToplevelSurface,
    window: &Window,
) -> Option<Restored> {
    let placements = live(state)?;
    let candidate = candidate(toplevel);
    let Some(idx) = placements.find(&candidate) else {
        // Another app opened a window before the restored one that had focus: it keeps the
        // front (an app that starts with the session and is quicker than the restore).
        if !placements.is_restored_app(&candidate) {
            placements.note_other_window();
        }
        return None;
    };
    placements.take(idx, window.clone());
    let placement = placements.placement(idx).clone();
    let activate = if placements.may_focus(&placement) {
        ActivateWindow::Yes
    } else {
        ActivateWindow::No
    };

    let output = resolve_output(state, &placement.output);
    let layout = &state.niri.layout;

    let workspace = match &placement.workspace {
        Some(WorkspaceReferenceArg::Name(name)) => {
            layout.find_workspace_by_name(name).map(|(_, ws)| ws.id())
        }
        Some(WorkspaceReferenceArg::Id(id)) => {
            let id = WorkspaceId::specific(*id);
            layout.find_workspace_by_id(id).map(|_| id)
        }
        Some(WorkspaceReferenceArg::Index(index)) => output
            .as_ref()
            .and_then(|output| workspace_for_group(state, output, &placement.output, *index)),
        None => None,
    };

    let target = match (workspace, placement.column) {
        (Some(ws), Some(column)) if !placement.floating => {
            let placements = state.niri.window_placements.as_ref()?;
            let layout = &state.niri.layout;
            let placed: Vec<_> = placements
                .neighbours(idx)
                .filter_map(|(w, p)| {
                    let (on, col, tile) = layout.scrolling_position(w)?;
                    (on == ws).then(|| Placed {
                        window: w.clone(),
                        column: p.column.unwrap_or(0),
                        tile: p.tile.unwrap_or(1),
                        at: (col, tile),
                    })
                })
                .collect();
            Some(
                match column_target(column, placement.tile.unwrap_or(1), &placed) {
                    ColumnTarget::Auto => Target::Workspace(ws),
                    ColumnTarget::NewColumn(at) => Target::WorkspaceColumn(ws, at),
                    ColumnTarget::InColumn(w, at) => Target::InColumn(w, at),
                },
            )
        }
        (Some(ws), _) => Some(Target::Workspace(ws)),
        (None, _) => output.map(Target::Output),
    };

    Some(Restored {
        target,
        width: placement
            .width
            .filter(|_| !placement.maximized)
            .map(PresetSize::Fixed),
        floating: placement.floating,
        maximized: placement.maximized,
        activate,
    })
}

/// The connected output a placement names, else the active one.
fn resolve_output(state: &State, saved: &Option<String>) -> Option<Output> {
    let layout = &state.niri.layout;
    saved
        .as_deref()
        .and_then(|name| {
            state
                .niri
                .global_space
                .outputs()
                .find(|o| output_matches_name(o, name))
                .cloned()
        })
        .filter(|o| layout.monitor_for_output(o).is_some())
        .or_else(|| layout.active_output().cloned())
}

/// Take a `PlaceWindows` request.
///
/// The workspace being looked at, if it is still empty (as at login), becomes the saved
/// workspace that had focus: the user lands where they left off, and every other restored
/// workspace is made above or below it, in the saved order, without moving the view.
pub fn start(state: &mut State, request: WindowPlacements) {
    state.niri.window_placements = Placements::new(request, Instant::now());
    // A window already in front (an app that started with the session) stays there.
    let has_focus = state.niri.layout.focus().is_some();
    let Some(placements) = &mut state.niri.window_placements else {
        return;
    };
    if has_focus {
        placements.note_other_window();
    }
    let Some(focused) = placements.focused() else {
        return;
    };
    let Some(index) = saved_index(focused) else {
        return;
    };
    let saved_output = focused.output.clone();
    let Some(output) = resolve_output(state, &saved_output) else {
        return;
    };
    let Some(mon) = state.niri.layout.monitor_for_output(&output) else {
        return;
    };
    let ws = mon.active_workspace_ref();
    if ws.has_windows() || ws.name().is_some() {
        return;
    }
    let id = ws.id();
    if let Some(placements) = &mut state.niri.window_placements {
        placements.set_group(saved_output, index, id);
    }
}

/// The workspace holding saved index `index` on an output, made if this is its first window.
fn workspace_for_group(
    state: &mut State,
    output: &Output,
    saved_output: &Option<String>,
    index: u8,
) -> Option<WorkspaceId> {
    let placements = state.niri.window_placements.as_ref()?;
    let layout = &state.niri.layout;
    if let Some(id) = placements.group(saved_output, index) {
        if layout.find_workspace_by_id(id).is_some() {
            return Some(id);
        }
    }
    let ids = layout.workspace_ids_on(output);
    let others: Vec<_> = placements
        .groups_on(saved_output)
        .filter_map(|(i, id)| ids.iter().position(|x| *x == id).map(|at| (i, at)))
        .collect();
    let slot = workspace_slot(index, &others);
    let id = state.niri.layout.empty_workspace_on(output, slot)?;
    state
        .niri
        .window_placements
        .as_mut()?
        .set_group(saved_output.clone(), index, id);
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(token: &str, app: &str, title: &str) -> WindowPlacement {
        WindowPlacement {
            token: Some(token.into()),
            app_id: Some(app.into()),
            title: Some(title.into()),
            ..Default::default()
        }
    }

    fn candidate(token: Option<&str>, app: &str, title: &str) -> Candidate {
        Candidate {
            token: token.map(Into::into),
            app_id: Some(app.into()),
            title: Some(title.into()),
        }
    }

    fn set(placements: Vec<WindowPlacement>) -> Placements<u32> {
        let request = WindowPlacements {
            placements,
            timeout_ms: 1000,
        };
        Placements::new(request, Instant::now()).unwrap()
    }

    #[test]
    fn token_decides_and_title_breaks_ties() {
        let mut p = set(vec![
            placement("1", "chromium", "Mail"),
            placement("1", "chromium", "Docs"),
            placement("2", "kitty", "~"),
        ]);
        assert_eq!(p.find(&candidate(Some("1"), "chromium", "Docs")), Some(1));
        assert_eq!(
            p.find(&candidate(Some("1"), "chromium", "New Tab")),
            Some(0)
        );
        assert_eq!(p.find(&candidate(Some("2"), "chromium", "Docs")), None);
        assert_eq!(p.find(&candidate(Some("3"), "kitty", "~")), None);
        p.take(0, 10);
        assert_eq!(
            p.find(&candidate(Some("1"), "chromium", "New Tab")),
            Some(1)
        );
    }

    #[test]
    fn untokened_windows_match_by_app_id() {
        let p = set(vec![placement("1", "nautilus", "Home")]);
        assert_eq!(p.find(&candidate(None, "nautilus", "x")), Some(0));
        assert_eq!(p.find(&candidate(None, "kitty", "x")), None);
    }

    #[test]
    fn live_until_every_window_came_or_time_ran_out() {
        let mut p = set(vec![placement("1", "a", "")]);
        let now = Instant::now();
        assert!(p.is_live(now));
        assert!(!p.is_live(now + Duration::from_secs(2)));
        p.take(0, 1);
        assert!(!p.is_live(now));
        assert!(Placements::<u32>::new(WindowPlacements::default(), now).is_none());
    }

    #[test]
    fn focus_only_while_the_user_is_idle() {
        let mut p = set(vec![WindowPlacement {
            focus: true,
            ..placement("1", "a", "")
        }]);
        assert!(p.may_focus(p.placement(0)));
        p.note_user_input();
        assert!(!p.may_focus(p.placement(0)));
    }

    #[test]
    fn another_app_in_front_keeps_it() {
        let mut p = set(vec![WindowPlacement {
            focus: true,
            ..placement("1", "a", "")
        }]);
        // A splash screen or extra window of a relaunched app is not another app.
        assert!(p.is_restored_app(&candidate(Some("1"), "a", "splash")));
        assert!(!p.is_restored_app(&candidate(None, "slack", "")));
        assert!(!p.is_restored_app(&candidate(Some("9"), "b", "")));
        p.note_other_window();
        assert!(!p.may_focus(p.placement(0)));
    }

    fn placed(window: u32, column: u32, tile: u32, at: (usize, usize)) -> Placed<u32> {
        Placed {
            window,
            column,
            tile,
            at,
        }
    }

    #[test]
    fn columns_keep_their_saved_order_in_any_opening_order() {
        assert_eq!(column_target::<u32>(2, 1, &[]), ColumnTarget::Auto);
        // Saved 1 and 3 are at 0 and 1; 2 goes between them.
        let both = [placed(1, 1, 1, (0, 0)), placed(3, 3, 1, (1, 0))];
        assert_eq!(column_target(2, 1, &both), ColumnTarget::NewColumn(1));
        // Only 3 is there: 1 goes before it, 4 after it.
        let three = [placed(3, 3, 1, (2, 0))];
        assert_eq!(column_target(1, 1, &three), ColumnTarget::NewColumn(2));
        assert_eq!(column_target(4, 1, &three), ColumnTarget::NewColumn(3));
    }

    #[test]
    fn stacked_windows_join_their_column_in_order() {
        let column = [placed(7, 2, 1, (1, 0)), placed(9, 2, 3, (1, 1))];
        assert_eq!(column_target(2, 2, &column), ColumnTarget::InColumn(7, 1));
        assert_eq!(column_target(2, 4, &column), ColumnTarget::InColumn(7, 2));
        let lower = [placed(9, 2, 3, (1, 0))];
        assert_eq!(column_target(2, 1, &lower), ColumnTarget::InColumn(9, 0));
    }

    #[test]
    fn workspaces_keep_their_saved_order() {
        assert_eq!(workspace_slot(1, &[]), None);
        // Saved 2 is at 0: 1 goes before it, 3 after it.
        assert_eq!(workspace_slot(1, &[(2, 0)]), Some(0));
        assert_eq!(workspace_slot(3, &[(2, 0)]), Some(1));
        // Between 1 (at 0) and 4 (at 1).
        assert_eq!(workspace_slot(2, &[(1, 0), (4, 1)]), Some(1));
    }

    #[test]
    fn groups_are_per_output_and_index() {
        let mut p = set(vec![placement("1", "a", "")]);
        let a = WorkspaceId::specific(5);
        p.set_group(Some("DP-1".into()), 2, a);
        assert_eq!(p.group(&Some("DP-1".into()), 2), Some(a));
        assert_eq!(p.group(&None, 2), None);
    }
}
