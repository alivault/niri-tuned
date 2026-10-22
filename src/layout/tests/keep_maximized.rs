//! A maximized (full-width or maximized-to-edges) or fullscreen window keeps that state when it
//! moves to another workspace or monitor.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    FullWidth,
    ToEdges,
    Fullscreen,
    /// Maximized to edges, then fullscreen: unfullscreening goes back to maximized.
    ToEdgesAndFullscreen,
}

const KINDS: [Kind; 4] = [
    Kind::FullWidth,
    Kind::ToEdges,
    Kind::Fullscreen,
    Kind::ToEdgesAndFullscreen,
];

struct State {
    output: String,
    ws_idx: usize,
    is_full_width: bool,
    column_maximized: bool,
    column_mode: SizingMode,
    window_mode: SizingMode,
}

/// Where window `id` is and its state.
fn state(layout: &Layout<TestWindow>, id: usize) -> State {
    let (mon, ws_idx, ws) = layout
        .workspaces()
        .find(|(_, _, ws)| ws.has_window(&id))
        .unwrap();
    let col = ws
        .scrolling()
        .columns()
        .find(|col| col.contains(&id))
        .expect("window must be tiled");
    let win = ws.windows().find(|win| *win.id() == id).unwrap();
    State {
        output: mon.map(|mon| mon.output_name().clone()).unwrap_or_default(),
        ws_idx,
        is_full_width: col.is_full_width(),
        column_maximized: col.is_pending_maximized(),
        column_mode: col.pending_sizing_mode(),
        window_mode: win.pending_sizing_mode(),
    }
}

fn maximize_ops(kind: Kind) -> Vec<Op> {
    match kind {
        Kind::FullWidth => vec![Op::MaximizeColumn],
        Kind::ToEdges => vec![Op::MaximizeWindowToEdges { id: Some(1) }],
        Kind::Fullscreen => vec![Op::FullscreenWindow(1)],
        Kind::ToEdgesAndFullscreen => vec![
            Op::MaximizeWindowToEdges { id: Some(1) },
            Op::FullscreenWindow(1),
        ],
    }
}

#[track_caller]
fn assert_kept(layout: &Layout<TestWindow>, kind: Kind, output: &str, ws_idx: usize) {
    let st = state(layout, 1);
    assert_eq!(
        (st.output.as_str(), st.ws_idx),
        (output, ws_idx),
        "{kind:?}: window ended up elsewhere"
    );
    let (full_width, maximized, mode) = match kind {
        Kind::FullWidth => (true, false, SizingMode::Normal),
        Kind::ToEdges => (false, true, SizingMode::Maximized),
        Kind::Fullscreen => (false, false, SizingMode::Fullscreen),
        Kind::ToEdgesAndFullscreen => (false, true, SizingMode::Fullscreen),
    };
    assert_eq!(st.is_full_width, full_width, "{kind:?}: full width");
    assert_eq!(st.column_maximized, maximized, "{kind:?}: column maximized");
    assert_eq!(st.column_mode, mode, "{kind:?}: column sizing mode");
    assert_eq!(st.window_mode, mode, "{kind:?}: window sizing mode");
}

/// Two outputs. Output1: windows 4 and 1 (maximized) on the first workspace, window 2 on the
/// second. Output2: window 3.
fn setup(kind: Kind) -> Layout<TestWindow> {
    let ops = [
        Op::AddOutput(1),
        Op::AddOutput(2),
        Op::FocusOutput(2),
        Op::AddWindow {
            params: TestWindowParams::new(3),
        },
        Op::FocusOutput(1),
        Op::AddWindow {
            params: TestWindowParams::new(2),
        },
        Op::MoveWindowToWorkspaceDown(false),
        // Keeps the first workspace from going away when window 1 leaves it.
        Op::AddWindow {
            params: TestWindowParams::new(4),
        },
        Op::AddWindow {
            params: TestWindowParams::new(1),
        },
    ];
    let mut layout = check_ops(ops);
    let ops = maximize_ops(kind)
        .into_iter()
        .chain([Op::Communicate(1), Op::CompleteAnimations]);
    check_ops_on_layout(&mut layout, ops);
    assert_kept(&layout, kind, "output1", 0);
    layout
}

#[track_caller]
fn check(kind: Kind, ops: impl IntoIterator<Item = Op>, output: &str, ws_idx: usize) {
    let mut layout = setup(kind);
    check_ops_on_layout(&mut layout, ops);
    check_ops_on_layout(&mut layout, [Op::Communicate(1), Op::CompleteAnimations]);
    assert_kept(&layout, kind, output, ws_idx);
}

#[test]
fn move_window_to_workspace_down_keeps_maximized() {
    for kind in KINDS {
        check(kind, [Op::MoveWindowToWorkspaceDown(true)], "output1", 1);
    }
}

#[test]
fn move_window_to_workspace_up_keeps_maximized() {
    for kind in KINDS {
        check(
            kind,
            [
                Op::MoveWindowToWorkspaceDown(true),
                Op::MoveWindowToWorkspaceUp(true),
            ],
            "output1",
            0,
        );
    }
}

#[test]
fn move_window_to_workspace_index_keeps_maximized() {
    for kind in KINDS {
        check(
            kind,
            [Op::MoveWindowToWorkspace {
                window_id: Some(1),
                workspace_idx: 1,
            }],
            "output1",
            1,
        );
        check(
            kind,
            [Op::MoveWindowToWorkspace {
                window_id: None,
                workspace_idx: 2,
            }],
            "output1",
            2,
        );
    }
}

#[test]
fn move_column_to_workspace_keeps_maximized() {
    for kind in KINDS {
        check(kind, [Op::MoveColumnToWorkspaceDown(true)], "output1", 1);
        check(kind, [Op::MoveColumnToWorkspace(2, true)], "output1", 2);
        check(
            kind,
            [
                Op::MoveColumnToWorkspaceDown(true),
                Op::MoveColumnToWorkspaceUp(true),
            ],
            "output1",
            0,
        );
    }
}

#[test]
fn move_window_to_monitor_keeps_maximized() {
    for kind in KINDS {
        check(
            kind,
            [Op::MoveWindowToOutput {
                window_id: None,
                output_id: 2,
                target_ws_idx: None,
            }],
            "output2",
            0,
        );
        check(
            kind,
            [Op::MoveWindowToOutput {
                window_id: Some(1),
                output_id: 2,
                target_ws_idx: Some(1),
            }],
            "output2",
            1,
        );
    }
}

#[test]
fn move_column_to_monitor_keeps_maximized() {
    for kind in KINDS {
        check(
            kind,
            [Op::MoveColumnToOutput {
                output_id: 2,
                target_ws_idx: None,
                activate: true,
            }],
            "output2",
            0,
        );
        check(
            kind,
            [Op::MoveColumnToOutput {
                output_id: 2,
                target_ws_idx: Some(1),
                activate: false,
            }],
            "output2",
            1,
        );
    }
}

fn interactive_move(output_idx: usize, px: f64, py: f64) -> [Op; 4] {
    [
        Op::InteractiveMoveBegin {
            window: 1,
            output_idx: 1,
            px: 100.,
            py: 100.,
        },
        // Past the threshold: the window leaves the layout.
        Op::InteractiveMoveUpdate {
            window: 1,
            dx: 1000.,
            dy: 1000.,
            output_idx: 1,
            px: 100.,
            py: 100.,
        },
        Op::InteractiveMoveUpdate {
            window: 1,
            dx: 0.,
            dy: 0.,
            output_idx,
            px,
            py,
        },
        Op::InteractiveMoveEnd { window: 1 },
    ]
}

#[test]
fn interactive_move_to_monitor_keeps_maximized() {
    for kind in KINDS {
        check(kind, interactive_move(2, 5., 360.), "output2", 0);
    }
}

#[test]
fn interactive_move_on_same_workspace_keeps_maximized() {
    for kind in KINDS {
        check(kind, interactive_move(1, 5., 360.), "output1", 0);
    }
}

#[test]
fn overview_drag_to_workspace_keeps_maximized() {
    for kind in KINDS {
        let mut layout = setup(kind);
        check_ops_on_layout(&mut layout, [Op::ToggleOverview, Op::CompleteAnimations]);
        // Find the second workspace's render geometry on output1.
        let mon = layout
            .monitors()
            .find(|m| m.output_name() == "output1")
            .unwrap();
        let geo: Vec<_> = mon.workspaces_render_geo().collect();
        let ws1 = geo[1];
        // At the left edge: a new column, not into window 2's column.
        let y = ws1.loc.y + ws1.size.h / 2.;
        check_ops_on_layout(&mut layout, interactive_move(1, ws1.loc.x + 2., y));
        check_ops_on_layout(&mut layout, [Op::Communicate(1), Op::CompleteAnimations]);
        assert_kept(&layout, kind, "output1", 1);
    }
}

#[test]
fn overview_drag_to_new_workspace_keeps_maximized() {
    for kind in KINDS {
        let mut layout = setup(kind);
        check_ops_on_layout(&mut layout, [Op::ToggleOverview, Op::CompleteAnimations]);
        // The gap between the first and second workspaces inserts a new workspace.
        let mon = layout
            .monitors()
            .find(|m| m.output_name() == "output1")
            .unwrap();
        let geo: Vec<_> = mon.workspaces_render_geo().collect();
        let y = (geo[0].loc.y + geo[0].size.h + geo[1].loc.y) / 2.;
        check_ops_on_layout(&mut layout, interactive_move(1, geo[0].loc.x + 2., y));
        check_ops_on_layout(&mut layout, [Op::Communicate(1), Op::CompleteAnimations]);
        assert_kept(&layout, kind, "output1", 1);
    }
}

#[test]
fn moved_maximized_fullscreen_window_unfullscreens_to_maximized() {
    for ops in [
        vec![Op::MoveWindowToWorkspaceDown(true)],
        vec![Op::MoveWindowToOutput {
            window_id: Some(1),
            output_id: 2,
            target_ws_idx: None,
        }],
        interactive_move(2, 5., 360.).to_vec(),
    ] {
        let mut layout = setup(Kind::ToEdgesAndFullscreen);
        check_ops_on_layout(&mut layout, ops);
        check_ops_on_layout(
            &mut layout,
            [
                Op::FullscreenWindow(1),
                Op::Communicate(1),
                Op::CompleteAnimations,
            ],
        );
        let st = state(&layout, 1);
        assert_eq!(st.column_mode, SizingMode::Maximized);
        assert_eq!(st.window_mode, SizingMode::Maximized);
    }
}

#[test]
fn interactive_move_of_maximized_floating_window_keeps_maximized() {
    for kind in [Kind::ToEdges, Kind::Fullscreen] {
        let mut layout = check_ops([
            Op::AddOutput(1),
            Op::AddOutput(2),
            Op::FocusOutput(1),
            Op::AddWindow {
                params: TestWindowParams::new(1),
            },
            Op::ToggleWindowFloating { id: Some(1) },
        ]);
        check_ops_on_layout(&mut layout, maximize_ops(kind));
        check_ops_on_layout(&mut layout, interactive_move(2, 640., 360.));
        check_ops_on_layout(&mut layout, [Op::Communicate(1), Op::CompleteAnimations]);
        assert_kept(&layout, kind, "output2", 0);

        // Unmaximizing still returns it to floating.
        let op = match kind {
            Kind::Fullscreen => Op::FullscreenWindow(1),
            _ => Op::MaximizeWindowToEdges { id: Some(1) },
        };
        check_ops_on_layout(&mut layout, [op]);
        let ws = layout.active_workspace().unwrap();
        assert!(ws.is_floating(&1), "{kind:?}: did not return to floating");
    }
}

#[test]
fn interactive_move_into_column_takes_its_state() {
    // Dropped into another window's column, the window joins it as a normal tile.
    for kind in [Kind::ToEdges, Kind::Fullscreen] {
        let mut layout = setup(kind);
        // Window 3 is 100×200 at (16, 16) on output2; aim at its bottom edge to go below it.
        let ops = interactive_move(2, 66., 220.);
        check_ops_on_layout(&mut layout, ops);
        check_ops_on_layout(&mut layout, [Op::Communicate(1), Op::CompleteAnimations]);
        let (_, _, ws) = layout
            .workspaces()
            .find(|(_, _, ws)| ws.has_window(&1))
            .unwrap();
        let col = ws
            .scrolling()
            .columns()
            .find(|col| col.contains(&1))
            .unwrap();
        assert!(
            col.contains(&3),
            "{kind:?}: not dropped into window 3's column"
        );
        assert_eq!(col.pending_sizing_mode(), SizingMode::Normal);
        let st = state(&layout, 1);
        assert_eq!(st.window_mode, SizingMode::Normal);
    }
}
