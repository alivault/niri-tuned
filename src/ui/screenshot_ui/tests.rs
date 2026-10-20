use smithay::output::{PhysicalProperties, Subpixel};

use super::*;

fn dragging(settings: niri_config::ScreenshotUi) -> ScreenshotUi {
    let output = Output::new(
        "test".into(),
        PhysicalProperties {
            size: (100, 100).into(),
            subpixel: Subpixel::Unknown,
            make: String::new(),
            model: String::new(),
            serial_number: String::new(),
        },
    );
    let clock = Clock::default();
    let config = Rc::new(RefCell::new(Config::default()));
    ScreenshotUi::Open {
        selection: (output.clone(), (10, 20).into(), (110, 120).into()),
        selection_visible: true,
        settings,
        // Release logic and selection lifetime do not need GPU-backed outputs.
        output_data: HashMap::new(),
        button: Button::Down {
            touch_slot: None,
            on_capture_button: false,
            last_pos: (output, (110, 120).into()),
            move_state: None,
        },
        show_pointer: false,
        open_anim: Animation::new(
            clock.clone(),
            0.,
            1.,
            0.,
            niri_config::Animations::default().screenshot_ui_open.0,
        ),
        clock,
        config,
        path: None,
    }
}

#[test]
fn release_captures_once_and_manual_confirmation_still_works() {
    let mut ui = dragging(Default::default());
    assert_eq!(ui.pointer_up(None), Some(true));
    assert_eq!(ui.pointer_up(None), None);
    assert_eq!(
        ui.action(Keysym::Return, Default::default()),
        Some(Action::ConfirmScreenshot {
            write_to_disk: true
        })
    );

    let mut ui = dragging(niri_config::ScreenshotUi {
        capture_on_release: false,
        ..Default::default()
    });
    assert_eq!(ui.pointer_up(None), Some(false));
    assert_eq!(
        ui.action(Keysym::space, Default::default()),
        Some(Action::ConfirmScreenshot {
            write_to_disk: true
        })
    );
    let mods = ModifiersState {
        ctrl: true,
        ..Default::default()
    };
    assert_eq!(
        ui.action(Keysym::c, mods),
        Some(Action::ConfirmScreenshot {
            write_to_disk: false
        })
    );
}

#[test]
fn hidden_selection_cannot_be_captured_by_keyboard() {
    let mut ui = dragging(Default::default());
    if let ScreenshotUi::Open {
        selection_visible,
        button,
        ..
    } = &mut ui
    {
        *selection_visible = false;
        *button = Button::Up;
    }
    assert!(!ui.has_selection());
    assert_eq!(ui.action(Keysym::Return, Default::default()), None);
    assert_eq!(ui.action(Keysym::space, Default::default()), None);
    assert_eq!(
        ui.action(
            Keysym::c,
            ModifiersState {
                ctrl: true,
                ..Default::default()
            }
        ),
        None
    );
    assert_eq!(
        ui.action(Keysym::Escape, Default::default()),
        Some(Action::CancelScreenshot)
    );
}

#[test]
fn clicks_and_line_drags_do_not_capture() {
    for point in [(10, 20), (10, 100), (100, 20)] {
        let mut ui = dragging(Default::default());
        if let ScreenshotUi::Open { selection, .. } = &mut ui {
            selection.2 = point.into();
        }
        assert_eq!(ui.pointer_up(None), Some(false));
        assert!(!ui.has_selection());
    }
}

#[test]
fn only_the_selecting_touch_can_capture() {
    let mut ui = dragging(Default::default());
    if let ScreenshotUi::Open {
        button: Button::Down { touch_slot, .. },
        ..
    } = &mut ui
    {
        *touch_slot = Some(TouchSlot::from(Some(1)));
    }
    assert_eq!(ui.pointer_up(Some(TouchSlot::from(Some(2)))), None);
    assert_eq!(ui.pointer_up(None), None);
    assert_eq!(ui.pointer_up(Some(TouchSlot::from(Some(1)))), Some(true));
}

#[test]
fn secondary_move_touch_does_not_capture() {
    let mut ui = dragging(Default::default());
    if let ScreenshotUi::Open {
        button:
            Button::Down {
                touch_slot,
                move_state,
                ..
            },
        ..
    } = &mut ui
    {
        *touch_slot = Some(TouchSlot::from(Some(1)));
        *move_state = Some(MoveState {
            pointer_offset: (0, 0).into(),
            touch_slot: Some(TouchSlot::from(Some(2))),
        });
    }
    assert_eq!(ui.pointer_up(Some(TouchSlot::from(Some(2)))), None);
    assert_eq!(ui.pointer_up(Some(TouchSlot::from(Some(1)))), Some(true));
}

#[test]
fn remembering_selection_is_opt_in() {
    for remember in [false, true] {
        let mut ui = dragging(niri_config::ScreenshotUi {
            remember_selection: remember,
            ..Default::default()
        });
        assert!(ui.close());
        let ScreenshotUi::Closed { last_selection, .. } = ui else {
            panic!("not closed")
        };
        assert_eq!(last_selection.is_some(), remember);
    }
}
