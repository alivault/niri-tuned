//! Windows reopened at login open straight where they were (`Request::PlaceWindows`).

use niri_ipc::{WindowPlacement, WindowPlacements, WorkspaceReferenceArg};

use super::client::ClientId;
use super::*;
use crate::layout::workspace::WorkspaceId;
use wayland_client::protocol::wl_surface::WlSurface;

fn saved(app: &str, workspace: u8, column: u32, tile: u32) -> WindowPlacement {
    WindowPlacement {
        app_id: Some(app.into()),
        workspace: Some(WorkspaceReferenceArg::Index(workspace)),
        column: Some(column),
        tile: Some(tile),
        ..Default::default()
    }
}

/// Opens a window with this app id and maps it.
fn open(f: &mut Fixture, id: ClientId, app: &str) -> WlSurface {
    let window = f.client(id).create_window();
    window.set_app_id(app);
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(id);
    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.ack_last_and_commit();
    f.double_roundtrip(id);
    surface
}

/// Workspaces of output 1, top to bottom: columns of app ids.
fn layout(f: &mut Fixture) -> Vec<Vec<Vec<String>>> {
    let niri = f.niri();
    let mut workspaces = Vec::new();
    for (_, _, ws) in niri.layout.workspaces() {
        let mut columns: Vec<Vec<String>> = Vec::new();
        let mut tiles: Vec<_> = ws
            .tiles_with_ipc_layouts()
            .filter_map(|(tile, l)| {
                let app = crate::utils::with_toplevel_role(tile.window().toplevel(), |r| {
                    r.app_id.clone()
                });
                Some((l.pos_in_scrolling_layout?, app))
            })
            .collect();
        tiles.sort();
        for ((col, _), app) in tiles {
            if columns.len() < col {
                columns.push(Vec::new());
            }
            columns[col - 1].push(app.unwrap_or_default());
        }
        workspaces.push(columns);
    }
    workspaces
}

fn focused_app(f: &mut Fixture) -> Option<String> {
    let niri = f.niri();
    let mapped = niri.layout.focus()?;
    crate::utils::with_toplevel_role(mapped.toplevel(), |r| r.app_id.clone())
}

fn expect(f: &mut Fixture, placements: Vec<WindowPlacement>) {
    let request = WindowPlacements {
        placements,
        timeout_ms: 60_000,
    };
    crate::window_placement::start(f.niri_state(), request);
}

fn active_workspace(f: &mut Fixture) -> WorkspaceId {
    f.niri().layout.active_workspace().unwrap().id()
}

#[test]
fn windows_open_in_their_saved_places_in_any_order() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    expect(
        &mut f,
        vec![
            saved("a", 1, 1, 1),
            saved("b", 1, 2, 1),
            saved("d", 1, 1, 2),
            WindowPlacement {
                focus: true,
                ..saved("c", 2, 1, 1)
            },
            saved("e", 3, 1, 1),
        ],
    );

    let looking_at = active_workspace(&mut f);
    for app in ["e", "b", "c", "d", "a"] {
        let _ = open(&mut f, id, app);
        // The view stays on the workspace the user left off on, whatever opens elsewhere.
        assert_eq!(active_workspace(&mut f), looking_at);
    }

    let s = |x: &[&[&str]]| -> Vec<Vec<String>> {
        x.iter()
            .map(|c| c.iter().map(|a| a.to_string()).collect())
            .collect()
    };
    assert_eq!(
        layout(&mut f),
        vec![
            s(&[&["a", "d"], &["b"]]),
            s(&[&["c"]]),
            s(&[&["e"]]),
            s(&[]),
        ]
    );
    // Only the window that had focus took it, on the workspace being looked at.
    assert_eq!(focused_app(&mut f).as_deref(), Some("c"));
    let niri = f.niri();
    let (_, c_ws) = niri
        .layout
        .workspaces()
        .map(|(_, i, ws)| (i, ws.id()))
        .nth(1)
        .unwrap();
    assert_eq!(c_ws, looking_at);
}

#[test]
fn restored_windows_never_steal_focus_after_the_user_acts() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();
    let _ = open(&mut f, id, "mine");

    expect(
        &mut f,
        vec![
            WindowPlacement {
                focus: true,
                ..saved("x", 2, 1, 1)
            },
            saved("y", 2, 2, 1),
        ],
    );
    f.niri()
        .window_placements
        .as_mut()
        .unwrap()
        .note_user_input();
    let _ = open(&mut f, id, "y");
    let _ = open(&mut f, id, "x");

    assert_eq!(focused_app(&mut f).as_deref(), Some("mine"));
    let niri = f.niri();
    assert_eq!(niri.layout.active_workspace().unwrap().windows().count(), 1);
}

#[test]
fn saved_width_and_floating_apply() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    expect(
        &mut f,
        vec![
            WindowPlacement {
                width: Some(600),
                ..saved("w", 1, 1, 1)
            },
            WindowPlacement {
                floating: true,
                ..saved("f", 1, 1, 1)
            },
        ],
    );
    let w = open(&mut f, id, "w");
    let _ = open(&mut f, id, "f");

    // The first configure already carries the saved width.
    let configures = f.client(id).window(&w).format_recent_configures();
    assert!(configures.starts_with("size: 600 × "), "{configures}");

    let niri = f.niri();
    let ws = niri.layout.active_workspace().unwrap();
    let floating: Vec<_> = ws
        .tiles_with_ipc_layouts()
        .filter(|(_, l)| l.pos_in_scrolling_layout.is_none())
        .map(|(tile, _)| {
            crate::utils::with_toplevel_role(tile.window().toplevel(), |r| r.app_id.clone())
        })
        .collect();
    assert_eq!(floating, vec![Some("f".to_string())]);
}

#[test]
fn an_app_already_in_front_stays_in_front() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();
    // Started with the session, quicker than the restore.
    let _ = open(&mut f, id, "autostart");

    expect(
        &mut f,
        vec![
            WindowPlacement {
                focus: true,
                ..saved("herdr", 3, 1, 1)
            },
            saved("email", 1, 1, 1),
        ],
    );
    let _ = open(&mut f, id, "email");
    let _ = open(&mut f, id, "herdr");

    assert_eq!(focused_app(&mut f).as_deref(), Some("autostart"));
}

#[test]
fn an_app_opening_before_the_focused_window_stays_in_front() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    expect(
        &mut f,
        vec![
            WindowPlacement {
                focus: true,
                ..saved("herdr", 1, 1, 1)
            },
            saved("email", 2, 1, 1),
        ],
    );
    let _ = open(&mut f, id, "email");
    let _ = open(&mut f, id, "autostart");
    let _ = open(&mut f, id, "herdr");

    assert_eq!(focused_app(&mut f).as_deref(), Some("autostart"));
}
