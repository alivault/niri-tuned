//! `layout { open-maximized-to-edges }`: windows that open in the scrolling layout open
//! maximized to the edges; floating windows and dialogs are left alone, and the window rule wins.

use niri_config::Config;
use smithay::reexports::wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;
use wayland_client::protocol::wl_surface::WlSurface;

use super::*;
use crate::layout::LayoutElement as _;
use crate::utils::with_toplevel_role;

fn config(extra: &str) -> Config {
    Config::parse_mem(&format!(
        "layout {{\n    open-maximized-to-edges\n}}\n{extra}"
    ))
    .unwrap()
}

/// Opens a window (with `before_initial` run before its initial commit and `before_map`
/// between the initial configure and the first buffer) and returns whether it ended up
/// maximized and whether it floats.
fn open(
    f: &mut Fixture,
    before_initial: impl FnOnce(&mut Fixture, client::ClientId, &WlSurface),
    before_map: impl FnOnce(&mut Fixture, client::ClientId, &WlSurface),
) -> (bool, bool) {
    let id = f.add_client();
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.set_title("subject");
    before_initial(f, id, &surface);
    f.client(id).window(&surface).commit();
    f.roundtrip(id);

    before_map(f, id, &surface);
    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.set_size(100, 100);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    // Answer any configure sent while mapping (unmaximizing into floating).
    let window = f.client(id).window(&surface);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    subject_state(f)
}

/// Whether the window titled "subject" is maximized, and whether it floats.
fn subject_state(f: &mut Fixture) -> (bool, bool) {
    let niri = f.niri();
    let (_, mapped) = niri
        .layout
        .windows()
        .find(|(_, m)| {
            with_toplevel_role(m.toplevel(), |role| {
                role.title.as_deref() == Some("subject")
            })
        })
        .unwrap();
    (mapped.sizing_mode().is_maximized(), mapped.is_floating())
}

fn with_output(config: Config) -> Fixture {
    let mut f = Fixture::with_config(config);
    f.add_output(1, (1920, 1080));
    f
}

fn nothing(_: &mut Fixture, _: client::ClientId, _: &WlSurface) {}

#[test]
fn tiled_window_opens_maximized() {
    let mut f = with_output(config(""));
    assert_eq!(open(&mut f, nothing, nothing), (true, false));
}

#[test]
fn off_by_default() {
    let mut f = with_output(Config::default());
    assert_eq!(open(&mut f, nothing, nothing), (false, false));
}

#[test]
fn initial_configure_is_maximized() {
    let mut f = with_output(config(""));
    let id = f.add_client();
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(id);
    let configures = f.client(id).window(&surface).format_recent_configures();
    assert!(configures.contains("Maximized"), "{configures}");
}

#[test]
fn floating_rule_stays_floating() {
    let mut f = with_output(config(
        "window-rule {\n    match title=\"^subject$\"\n    open-floating true\n}",
    ));
    assert_eq!(open(&mut f, nothing, nothing), (false, true));
}

#[test]
fn window_rule_false_wins() {
    let mut f = with_output(config(
        "window-rule {\n    open-maximized-to-edges false\n}",
    ));
    assert_eq!(open(&mut f, nothing, nothing), (false, false));
}

/// Opens a parent window and returns its toplevel.
fn parent(f: &mut Fixture) -> (client::ClientId, XdgToplevel) {
    let id = f.add_client();
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    let toplevel = window.xdg_toplevel.clone();
    window.set_title("parent");
    window.commit();
    f.roundtrip(id);
    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.ack_last_and_commit();
    f.double_roundtrip(id);
    (id, toplevel)
}

#[test]
fn dialog_stays_floating() {
    let mut f = with_output(config(""));
    let (id, toplevel) = parent(&mut f);
    // The dialog belongs to the parent's client.
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.set_title("subject");
    window.set_parent(Some(&toplevel));
    window.commit();
    f.roundtrip(id);
    let configures = f.client(id).window(&surface).format_recent_configures();
    assert!(!configures.contains("Maximized"), "{configures}");
}

#[test]
fn dialog_parented_after_initial_configure_unmaximizes_into_floating() {
    let mut f = with_output(config(""));
    let (id, toplevel) = parent(&mut f);
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.set_title("subject");
    window.commit();
    f.roundtrip(id);

    // Maximized at first, then it becomes a dialog before mapping.
    let window = f.client(id).window(&surface);
    window.set_parent(Some(&toplevel));
    window.attach_new_buffer();
    window.set_size(100, 100);
    window.ack_last_and_commit();
    f.double_roundtrip(id);
    let window = f.client(id).window(&surface);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    assert_eq!(subject_state(&mut f), (false, true));
}

#[test]
fn client_unmaximize_before_map_is_respected() {
    let mut f = with_output(config(""));
    let result = open(&mut f, nothing, |f, id, surface| {
        f.client(id).window(surface).unset_maximized();
        f.roundtrip(id);
    });
    assert_eq!(result, (false, false));
}
