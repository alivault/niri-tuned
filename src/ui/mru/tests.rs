use proptest::prelude::*;
use proptest_derive::Arbitrary;

use super::*;

fn create_thumbnail() -> Thumbnail {
    Thumbnail {
        id: MappedId::next(),
        timestamp: None,
        on_current_output: false,
        on_current_workspace: false,
        app_id: None,
        size: Size::new(100, 100),
        clock: Clock::with_time(Duration::ZERO),
        config: niri_config::MruPreviews::default(),
        open_animation: None,
        move_animation: None,
        title_texture: Default::default(),
        background: RefCell::new(FocusRing::new(Default::default())),
        border: RefCell::new(FocusRing::new(Default::default())),
        preview_damage: ExtraDamage::new(),
    }
}

#[test]
fn antialiasing_toggle_damages_even_a_static_thumbnail() {
    use smithay::backend::renderer::element::Element as _;

    let mut thumbnail = create_thumbnail();
    let original = thumbnail.preview_damage.current_commit();
    thumbnail.update_config(thumbnail.config);
    assert_eq!(thumbnail.preview_damage.current_commit(), original);
    let mut config = thumbnail.config;
    config.antialiasing = false;
    thumbnail.update_config(config);
    let changed = thumbnail.preview_damage.current_commit();
    assert_ne!(changed, original);
    thumbnail.update_config(config);
    assert_eq!(thumbnail.preview_damage.current_commit(), changed);
    config.antialiasing = true;
    thumbnail.update_config(config);
    assert_ne!(thumbnail.preview_damage.current_commit(), changed);
}

#[test]
fn backdrop_blur_is_live_fullscreen_and_hot_reloadable() {
    use smithay::backend::renderer::element::Element;

    let mut blur = BackdropBlur::new();
    let mut config = niri_config::Blur::default();
    let geometry = Rectangle::from_size(Size::new(1280., 800.));
    let radius = CornerRadius::default();
    let (elem, alpha) = blur.render(config, None, geometry, radius, 2.).unwrap();
    assert!(elem.is_framebuffer_effect());
    assert_eq!(alpha, 1.);
    assert_eq!(
        elem.geometry(Scale::from(2.)),
        geometry.to_physical_precise_round(2.)
    );
    assert_eq!(elem.alpha(), 1.);
    let commit = elem.current_commit();
    assert_eq!(elem.with_alpha(0.25).alpha(), 0.25);
    let again = |blur: &mut BackdropBlur, config, strength| {
        blur.render(config, strength, geometry, radius, 2.)
            .map(|(elem, alpha)| (elem.current_commit(), alpha))
    };
    assert_eq!(again(&mut blur, config, None).unwrap().0, commit);
    config.offset += 1.;
    assert_ne!(again(&mut blur, config, None).unwrap().0, commit);
    config.off = true;
    assert!(again(&mut blur, config, None).is_none());
    config.off = false;
    assert!(again(&mut blur, config, None).is_some());

    // A strength of its own: a new look (damaged), full alpha once strong
    // enough, faded in when faint, none at 0.
    let before = again(&mut blur, config, None).unwrap().0;
    let (strong, alpha) = again(&mut blur, config, Some(40.)).unwrap();
    assert_ne!(strong, before);
    assert_eq!(alpha, 1.);
    let (_, faint) = again(&mut blur, config, Some(0.5)).unwrap();
    assert!(faint > 0. && faint < 1.);
    assert!(again(&mut blur, config, Some(0.)).is_none());
}

#[test]
fn a_small_preview_gets_a_tile_wide_enough_for_its_title() {
    let small = Rectangle::new(Point::new(500., 100.), Size::new(35., 70.));
    // Widened about its centre, as far as the cells allow.
    let tile = tile_of(small, 300., false);
    assert_eq!(tile.size, Size::new(MIN_TILE_WIDTH, 70.));
    assert_eq!(tile.loc.x + tile.size.w / 2., 500. + 35. / 2.);
    assert_eq!(tile.loc.y, 100.);
    assert_eq!(tile_of(small, 120., false).size.w, 120.);
    // A preview wide enough already, and icons, keep their own width.
    let wide = Rectangle::new(Point::new(0., 0.), Size::new(400., 250.));
    assert_eq!(tile_of(wide, 400., false), wide);
    assert_eq!(tile_of(small, 300., true), small);
}

#[test]
fn header_puts_icon_and_label_above_the_preview() {
    let labels = LabelLayout {
        shown: true,
        selected_only: false,
        gap: 8.,
        height: 17.,
        header: 24. + 8.,
    };
    let preview = Rectangle::new(Point::new(100., 200.), Size::new(360., 224.));
    for scale in [1., 1.25, 2.] {
        let icon = header_icon_geometry(preview, &labels, scale);
        assert_eq!(icon.size, Size::new(24., 24.));
        // At the left edge, in the row above the preview, the gap under it.
        assert!((icon.loc.x - preview.loc.x).abs() <= 1. / scale);
        assert!((icon.loc.y - (preview.loc.y - 32.)).abs() <= 1. / scale);
        assert!(icon.loc.y + icon.size.h <= preview.loc.y - 8. + 1. / scale);
    }
    // The label beside the icon, centred on the row, as wide as is left.
    let (loc, width) = header_label_geometry(preview, &labels, 17.);
    assert_eq!(loc.x, 100. + 24. + 8.);
    assert_eq!(loc.y, 200. - 32. + (24. - 17.) / 2.);
    assert_eq!(width, 360. - 32.);
}

#[test]
fn remove_last_window_out_of_two() {
    let ops = [Op::Backward, Op::Remove(1)];

    let thumbnails = vec![create_thumbnail(), create_thumbnail()];
    let current_id = thumbnails.first().map(|t| t.id);
    let mut mru = WindowMru {
        thumbnails,
        current_id,
        scope: MruScope::All,
        app_id_filter: None,
    };

    check_ops(&mut mru, &ops);
}

fn arbitrary_scope() -> impl Strategy<Value = MruScope> {
    prop_oneof![
        Just(MruScope::All),
        Just(MruScope::Output),
        Just(MruScope::Workspace),
    ]
}

fn arbitrary_filter() -> impl Strategy<Value = MruFilter> {
    prop_oneof![Just(MruFilter::All), Just(MruFilter::AppId)]
}

fn arbitrary_app_id() -> impl Strategy<Value = Option<String>> {
    prop_oneof![Just(None), Just(Some(1)), Just(Some(2))]
        .prop_map(|id| id.map(|id| format!("app-{id}")))
}

prop_compose! {
    fn arbitrary_thumbnail()(
        timestamp: Option<Duration>,
        on_current_output: bool,
        on_current_workspace: bool,
        app_id in arbitrary_app_id(),
    ) -> Thumbnail {
        let mut thumbnail = create_thumbnail();
        thumbnail.timestamp = timestamp;
        thumbnail.on_current_workspace = on_current_workspace;
        thumbnail.on_current_output = on_current_output;
        thumbnail.app_id = app_id;
        thumbnail
    }
}

prop_compose! {
    fn arbitrary_mru()(
        thumbnails in proptest::collection::vec(arbitrary_thumbnail(), 1..10),
    ) -> WindowMru {
        let current_id = thumbnails.first().map(|t| t.id);
        WindowMru {
            thumbnails,
            current_id,
            scope: MruScope::All,
            app_id_filter: None,
        }
    }
}

#[derive(Debug, Clone, Arbitrary)]
enum Op {
    Forward,
    Backward,
    First,
    Last,
    SetScope(#[proptest(strategy = "arbitrary_scope()")] MruScope),
    SetFilter(#[proptest(strategy = "arbitrary_filter()")] MruFilter),
    Remove(#[proptest(strategy = "1..10usize")] usize),
}

impl Op {
    fn apply(&self, mru: &mut WindowMru) {
        match self {
            Op::Forward => mru.forward(),
            Op::Backward => mru.backward(),
            Op::First => mru.first(),
            Op::Last => mru.last(),
            Op::SetScope(scope) => {
                mru.set_scope(*scope);
            }
            Op::SetFilter(filter) => {
                mru.set_filter(*filter);
            }
            Op::Remove(idx) => {
                if *idx < mru.thumbnails.len() {
                    mru.remove_by_idx(*idx);
                }
            }
        }
    }
}

fn check_ops(mru: &mut WindowMru, ops: &[Op]) {
    for op in ops {
        op.apply(mru);
        mru.verify_invariants();
    }
}

proptest! {
    #[test]
    fn random_operations_dont_panic(
        mut mru in arbitrary_mru(),
        ops: Vec<Op>,
    ) {
        check_ops(&mut mru, &ops);
    }
}

#[test]
fn grid_shrinks_as_windows_are_added() {
    let output = Size::new(1920., 1080.);
    let natural = Size::new(1280., 720.);
    let layout = |n: usize| {
        let sizes = vec![natural; n];
        compute_grid_layout(
            &sizes,
            output,
            1.,
            16.,
            16.,
            32.,
            0.6,
            720.,
            false,
            false,
            Size::new(64., 96.),
        )
    };

    let one = layout(1);
    let two = layout(2);
    let seven = layout(7);
    let twenty = layout(20);

    // More windows never produce larger previews.
    assert!(one.1[0].w >= two.1[0].w);
    assert!(two.1[0].w >= seven.1[0].w);
    assert!(seven.1[0].w >= twenty.1[0].w);

    for (n, (_, sizes, locs)) in [(1, one), (2, two), (7, seven), (20, twenty)] {
        assert_eq!(sizes.len(), n);
        for (size, loc) in sizes.iter().zip(&locs) {
            assert!(loc.x >= 0. && loc.y >= 0., "{n}: {loc:?}");
            assert!(loc.x + size.w <= output.w + 1., "{n}: {loc:?} {size:?}");
            assert!(loc.y + size.h <= output.h + 1., "{n}: {loc:?} {size:?}");
        }
    }
}

#[test]
fn grid_rows_and_block_are_centered() {
    let output = Size::new(1920., 1080.);
    let title_gap = 32.;
    let natural = Size::new(1280., 720.);
    let sizes = vec![natural; 7];
    let (_, display, locs) = compute_grid_layout(
        &sizes,
        output,
        1.,
        16.,
        16.,
        title_gap,
        0.6,
        720.,
        false,
        false,
        Size::new(64., 96.),
    );

    // Group thumbnails into rows by y, then check each row's horizontal center.
    let mut rows: Vec<(f64, Vec<(f64, f64)>)> = Vec::new();
    for (size, loc) in display.iter().zip(&locs) {
        match rows.iter_mut().find(|(y, _)| (*y - loc.y).abs() < 1.) {
            Some((_, items)) => items.push((loc.x, loc.x + size.w)),
            None => rows.push((loc.y, vec![(loc.x, loc.x + size.w)])),
        }
    }
    assert!(rows.len() > 1, "expected the grid to wrap");

    for (_, items) in &rows {
        let min = items.iter().map(|(l, _)| *l).fold(f64::INFINITY, f64::min);
        let max = items
            .iter()
            .map(|(_, r)| *r)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(((min + max) / 2. - output.w / 2.).abs() <= 1.);
    }

    // The block, including the title space below the bottom row, is centered.
    let top = locs.iter().map(|l| l.y).fold(f64::INFINITY, f64::min);
    let bottom = locs
        .iter()
        .zip(&display)
        .map(|(l, s)| l.y + s.h)
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(((top + bottom + title_gap) / 2. - output.h / 2.).abs() <= 2.);
}

#[test]
fn grid_neighbor_index_moves_between_rows() {
    // 7 thumbnails in 4 columns: rows are [0, 1, 2, 3] and [4, 5, 6].
    let (count, cols) = (7, 4);

    assert_eq!(grid_neighbor_index(1, count, cols, 1), Some(5));
    // Third column has no counterpart in the short last row, so clamp to it.
    assert_eq!(grid_neighbor_index(3, count, cols, 1), Some(6));
    assert_eq!(grid_neighbor_index(5, count, cols, 1), None);

    assert_eq!(grid_neighbor_index(5, count, cols, -1), Some(1));
    assert_eq!(grid_neighbor_index(6, count, cols, -1), Some(2));
    assert_eq!(grid_neighbor_index(1, count, cols, -1), None);

    assert_eq!(grid_neighbor_index(0, 2, 2, 1), None);
    assert_eq!(grid_neighbor_index(0, 0, 0, 1), None);
}

#[test]
fn scope_panel_is_a_segmented_control() {
    fn hex(rgb: u32) -> Color {
        let c = |shift: u32| ((rgb >> shift) & 0xff) as f32 / 255.;
        Color::new_unpremul(c(16), c(8), c(0), 1.)
    }
    // The light theme colours ALNIAC's theme sync writes (Flexoki Light).
    let config = niri_config::MruScopePanel {
        background_color: hex(0xf0ede2),
        background_opacity: 1.,
        text_color: hex(0x100f0f),
        active_color: hex(0xffffff),
        active_text_color: hex(0x100f0f),
        border_color: hex(0xd9d7cd),
        border_width: 1.,
        corner_radius: 7.,
        font: String::from("sans Medium 12px"),
        ..Default::default()
    };
    for scale in [1., 2.] {
        let (mut surface, pills) =
            draw_panel(scale, &config, MruScope::Workspace, Some(MruScope::Output)).unwrap();
        assert_eq!(f64::from(surface.height()), 26. * scale);
        // Equal segments, back to back, 2px in from either edge.
        let widths: Vec<f64> = pills.iter().map(|(_, a, b)| b - a).collect();
        assert!(widths.iter().all(|w| (w - widths[0]).abs() < 1e-9));
        assert_eq!(pills[0].1, 2.);
        assert_eq!(pills[1].1, pills[0].2);
        assert!((f64::from(surface.width()) / scale - (pills[2].2 + 2.)).abs() < 1.);
        // Raw premultiplied BGRA for a look: magick -size WxH -depth 8 bgra:FILE out.png
        if let Some(path) = std::env::var_os("MRU_PANEL_RAW") {
            let (w, h, stride) = (surface.width(), surface.height(), surface.stride());
            assert_eq!(stride, w * 4);
            let name = format!("{}-{w}x{h}.bgra", path.to_string_lossy());
            std::fs::write(name, &*surface.take_data().unwrap()).unwrap();
        }
    }
}

#[test]
fn icons_can_be_a_single_row() {
    let output = Size::from((1280., 800.));
    let icons = vec![Size::from((96., 96.)); 4];
    let (cols, display, locs) = compute_grid_layout(
        &icons,
        output,
        1.,
        18.,
        16.,
        8.,
        1.,
        96.,
        true,
        true,
        Size::new(64., 96.),
    );
    assert_eq!(cols, 4);
    assert!(display.iter().all(|size| size.w == 96. && size.h == 96.));
    assert!(locs.iter().all(|loc| loc.y == locs[0].y));

    // Too many for one row at full size: still one row, shrunk to fit.
    let icons = vec![Size::from((96., 96.)); 20];
    let (cols, display, locs) = compute_grid_layout(
        &icons,
        output,
        1.,
        18.,
        16.,
        8.,
        1.,
        96.,
        true,
        true,
        Size::new(64., 96.),
    );
    assert_eq!(cols, 20);
    assert!(display[0].w < 96.);
    assert!(locs.iter().all(|loc| loc.y == locs[0].y));
    let right = locs[19].x + display[19].w;
    assert!(locs[0].x >= 0. && right <= output.w);
}

#[test]
fn margins_bound_the_rows() {
    let output = Size::from((1280., 800.));
    let icons = vec![Size::from((96., 96.)); 9];
    let layout = |margin: Size<f64, Logical>| {
        compute_grid_layout(
            &icons, output, 1., 18., 0., 8., 1., 96., false, true, margin,
        )
    };
    // (1280 - 2 * 300) / 132 fits 5 per row; no side margin fits all 9.
    let (cols, display, locs) = layout(Size::new(300., 96.));
    assert_eq!(cols, 5);
    assert!(locs.iter().all(|loc| loc.x >= 300. - 18.));
    assert!(display.iter().all(|size| size.w == 96.));
    assert_eq!(layout(Size::new(0., 96.)).0, 9);
}

#[test]
fn wrapping_fills_rows_at_full_size() {
    let output = Size::from((1280., 800.));
    let layout = |n: usize| {
        let icons = vec![Size::from((96., 96.)); n];
        compute_grid_layout(
            &icons,
            output,
            1.,
            18.,
            0.,
            8.,
            1.,
            96.,
            false,
            true,
            Size::new(64., 96.),
        )
    };

    // A few stay side by side in one row (never a column), at full size.
    let (cols, display, locs) = layout(3);
    assert_eq!(cols, 3);
    assert!(display.iter().all(|size| size.w == 96.));
    assert!(locs.iter().all(|loc| loc.y == locs[0].y));

    // (1280 - 128) / (96 + 36) fits 8 per row: 9 is 8 + 1, still full size.
    let (cols, display, locs) = layout(9);
    assert_eq!(cols, 8);
    assert!(display.iter().all(|size| size.w == 96.));
    assert!(locs[..8].iter().all(|loc| loc.y == locs[0].y));
    assert!(locs[8].y > locs[0].y);
    // The short last row is centered on its own.
    assert!(((locs[8].x + 48.) - 640.).abs() <= 1.);

    // More rows than the screen holds: everything shrinks just enough.
    let (cols, display, locs) = layout(80);
    assert!(display[0].w < 96.);
    assert!(cols > 8);
    for (size, loc) in display.iter().zip(&locs) {
        assert!(loc.x >= 0. && loc.x + size.w <= output.w);
        assert!(loc.y >= 0. && loc.y + size.h <= output.h);
    }
}

#[test]
fn previews_can_be_a_single_row() {
    let output = Size::from((1280., 800.));
    let windows = vec![Size::from((1280., 800.)); 6];
    let (cols, display, locs) = compute_grid_layout(
        &windows,
        output,
        1.,
        16.,
        16.,
        32.,
        0.6,
        720.,
        true,
        false,
        Size::new(64., 96.),
    );
    assert_eq!(cols, 6);
    assert!(locs.iter().all(|loc| loc.y == locs[0].y));
    // Padding stays fixed between previews: gap plus both paddings.
    let between = locs[1].x - (locs[0].x + display[0].w);
    assert!((between - (16. + 2. * 16.)).abs() <= 1.);
    let right = locs[5].x + display[5].w;
    assert!(locs[0].x >= 0. && right <= output.w);
}

#[test]
fn label_text_by_kind() {
    let name = || Some(String::from("Firefox"));
    let title = || Some(String::from("GitHub"));
    use MruLabelText::*;
    assert_eq!(
        label_text(Title, true, name(), title()).as_deref(),
        Some("GitHub")
    );
    assert_eq!(
        label_text(AppName, true, name(), title()).as_deref(),
        Some("Firefox")
    );
    assert_eq!(
        label_text(AppAndTitle, true, name(), title()).as_deref(),
        Some("Firefox \u{2014} GitHub")
    );
    assert_eq!(label_text(None, true, name(), title()), Option::None);
    // A title that only repeats the app name, or no title, gives the name.
    let same = Some(String::from("firefox "));
    assert_eq!(
        label_text(AppAndTitle, true, name(), same).as_deref(),
        Some("Firefox")
    );
    assert_eq!(
        label_text(AppAndTitle, true, name(), Option::None).as_deref(),
        Some("Firefox")
    );
    // Name still being looked up: nothing yet, rather than a flash of the title.
    assert_eq!(
        label_text(AppName, true, Option::None, title()),
        Option::None
    );
    assert_eq!(
        label_text(AppAndTitle, true, Option::None, title()),
        Option::None
    );
    // No app id: the title stands in.
    assert_eq!(
        label_text(AppName, false, Option::None, title()).as_deref(),
        Some("GitHub")
    );
    assert_eq!(
        label_text(AppAndTitle, false, Option::None, title()).as_deref(),
        Some("GitHub")
    );
}

#[test]
fn lone_selected_label_sits_below_the_highlight() {
    let all = LabelLayout {
        shown: true,
        selected_only: false,
        gap: 8.,
        height: 17.,
        header: 0.,
    };
    // Icons: every label inside the highlight, under the icon.
    assert_eq!(all.top(18.), 8.);
    assert_eq!(all.extent(18.), 18. + 8. + 17. - 9.);

    let selected = LabelLayout {
        selected_only: true,
        ..all
    };
    // The selected one alone: below the highlight's padding, plus the gap.
    assert_eq!(selected.top(18.), 26.);
    assert_eq!(selected.extent(18.), 26. + 17.);

    // Previews: labels in the header above, nothing below but the padding.
    let previews = LabelLayout { header: 32., ..all };
    assert_eq!(previews.extent(18.), 18.);
    assert_eq!(previews.header_row(), 24.);

    let none = LabelLayout {
        shown: false,
        ..all
    };
    assert_eq!(none.extent(18.), 18.);
}

#[test]
fn label_layout_follows_config() {
    let mut recent = niri_config::RecentWindows::default();
    recent.previews.icons_only = true;
    let labels = LabelLayout::new(&recent, 1.);
    assert!(labels.shown && labels.selected_only);
    assert_eq!(labels.header, 0.);
    assert!(labels.height > 0.);

    recent.previews.icons_only = false;
    let labels = LabelLayout::new(&recent, 1.);
    assert!(labels.shown && !labels.selected_only);
    // A header row as tall as the larger of its icon and label, then the gap.
    assert_eq!(labels.header, f64::max(24., labels.height) + labels.gap);

    recent.label.text = Some(MruLabelText::None);
    assert!(!LabelLayout::new(&recent, 1.).shown);
}
