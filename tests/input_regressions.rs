use egui::Event;
use egui::Pos2;
use smithay_client_toolkit::seat::keyboard::KeyEvent;
use smithay_client_toolkit::seat::keyboard::Keysym;
use smithay_client_toolkit::seat::keyboard::Modifiers;
use smithay_client_toolkit::seat::pointer::AxisScroll;
use smithay_client_toolkit::seat::pointer::PointerEvent;
use smithay_client_toolkit::seat::pointer::PointerEventKind;

#[test]
#[ignore = "requires a Wayland compositor"]
fn control_only_suppresses_printable_text_but_not_key_events_or_altgr_text() {
    with_input(|_, input| {
        for ctrl in [false, true] {
            for alt in [false, true] {
                input.update_modifiers(&Modifiers {
                    ctrl,
                    alt,
                    ..Default::default()
                });
                input.take_raw_input();
                let mut key = KeyEvent {
                    time: 1,
                    raw_code: 18,
                    keysym: Keysym::e,
                    utf8: Some("é".into()),
                };
                input.handle_keyboard_event(&key, true, false);
                key.utf8 = None;
                input.handle_keyboard_event(&key, true, true);
                input.handle_keyboard_event(&key, false, false);
                let events = input.take_raw_input().events;
                let text: Vec<_> = events
                    .iter()
                    .filter_map(|event| match event {
                        Event::Text(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(
                    text,
                    if ctrl && !alt {
                        vec![]
                    } else {
                        vec!["é", "é"]
                    },
                    "text filtering for Ctrl={ctrl}, Alt={alt}"
                );
                assert!(events.iter().any(|event| matches!(
                    event,
                    Event::Key {
                        key: egui::Key::E,
                        pressed: true,
                        repeat: false,
                        ..
                    }
                )));
                assert!(events.iter().any(|event| matches!(
                    event,
                    Event::Key {
                        key: egui::Key::E,
                        pressed: true,
                        repeat: true,
                        ..
                    }
                )));
                assert!(events.iter().any(|event| matches!(
                    event,
                    Event::Key {
                        key: egui::Key::E,
                        pressed: false,
                        ..
                    }
                )));
            }
        }
    });
}

#[test]
#[ignore = "requires a Wayland compositor"]
fn clipboard_shortcuts_do_not_consume_control_alt_key_combinations() {
    with_input(|_, input| {
        input.handle_keyboard_enter();
        for shift in [false, true] {
            for alt in [false, true] {
                input.update_modifiers(&Modifiers {
                    ctrl: true,
                    alt,
                    shift,
                    ..Default::default()
                });
                input.take_raw_input();
                // C/X only: inspect events, never read or write a clipboard.
                for (raw_code, keysym) in [(46, Keysym::c), (45, Keysym::x)] {
                    let key = KeyEvent {
                        time: 1,
                        raw_code,
                        keysym,
                        utf8: None,
                    };
                    input.handle_keyboard_event(&key, true, false);
                    input.handle_keyboard_event(&key, false, false);
                    let events = input.take_raw_input().events;
                    assert_eq!(
                        events
                            .iter()
                            .any(|event| matches!(event, Event::Copy | Event::Cut)),
                        !alt,
                        "clipboard action for Alt={alt}, Shift={shift}: {events:?}"
                    );
                    assert!(
                        events.iter().any(
                            |event| matches!(event, Event::Key { pressed: true, modifiers, .. }
                        if modifiers.ctrl && modifiers.alt == alt && modifiers.shift == shift)
                        ),
                        "shortcut filtering swallowed the ordinary key event"
                    );
                }
            }
        }
    });
}

#[test]
#[ignore = "requires a Wayland compositor"]
fn mixed_scroll_axes_preserve_units_and_direction() {
    with_input(|app, input| {
        input.handle_pointer_event(&PointerEvent {
            surface: app.compositor_state.create_surface(&app.qh),
            position: (0.0, 0.0),
            kind: PointerEventKind::Axis {
                time: 1,
                horizontal: AxisScroll {
                    value120: 60,
                    ..Default::default()
                },
                vertical: AxisScroll {
                    absolute: 3.0,
                    ..Default::default()
                },
                source: None,
            },
        });
        let events = input.take_raw_input().events;
        assert!(events.iter().any(|event| matches!(event, Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta, .. } if *delta == egui::vec2(-0.5, 0.0))));
        assert!(events.iter().any(|event| matches!(event, Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta, .. } if *delta == egui::vec2(0.0, -3.0))));
    });
}

#[test]
#[ignore = "requires a Wayland compositor"]
fn wayland_scroll_events_actually_scroll_egui() {
    with_input(|app, input| {
        input.handle_keyboard_enter();
        input.handle_pointer_event(&PointerEvent {
            surface: app.compositor_state.create_surface(&app.qh),
            position: (0.0, 0.0),
            kind: PointerEventKind::Axis {
                time: 1,
                horizontal: AxisScroll::default(),
                vertical: AxisScroll {
                    absolute: 3.0,
                    ..Default::default()
                },
                source: None,
            },
        });
        let context = egui::Context::default();
        let mut scroll = 0.0;
        let output = context.run_ui(input.take_raw_input(), |ui| {
            scroll = ui.input(|input| input.smooth_scroll_delta.y);
        });
        output.drop_without_applying_deltas();
        assert!(
            scroll < 0.0,
            "Wayland axis movement did not reach egui scrolling"
        );
    });
}

#[test]
#[ignore = "requires a Wayland compositor"]
fn pointer_removal_stops_scrolling_without_discarding_pending_keyboard_input() {
    with_input(|app, input| {
        let context = egui::Context::default();
        let mut text = String::new();
        input.handle_keyboard_enter();
        context
            .run_ui(input.take_raw_input(), |ui| {
                ui.text_edit_singleline(&mut text).request_focus();
            })
            .drop_without_applying_deltas();
        let surface = app.compositor_state.create_surface(&app.qh);
        input.handle_pointer_event(&PointerEvent {
            surface: surface.clone(),
            position: (24.0, 24.0),
            kind: PointerEventKind::Enter { serial: 1 },
        });
        input.handle_pointer_event(&PointerEvent {
            surface: surface.clone(),
            position: (24.0, 24.0),
            kind: PointerEventKind::Axis {
                time: 1,
                horizontal: AxisScroll::default(),
                vertical: AxisScroll {
                    absolute: 20.0,
                    ..Default::default()
                },
                source: None,
            },
        });
        context
            .run_ui(input.take_raw_input(), |ui| {
                ui.text_edit_singleline(&mut text).request_focus();
            })
            .drop_without_applying_deltas();
        assert!(context.input(|input| input.is_scrolling()));
        input.handle_keyboard_event(
            &KeyEvent {
                time: 2,
                raw_code: 30,
                keysym: Keysym::a,
                utf8: Some("a".into()),
            },
            true,
            false,
        );
        input.handle_pointer_event(&PointerEvent {
            surface,
            position: (24.0, 24.0),
            kind: PointerEventKind::Press {
                serial: 2,
                time: 2,
                button: 0x110,
            },
        });
        input.handle_pointer_removed(&context);
        context
            .run_ui(input.take_raw_input(), |ui| {
                ui.text_edit_singleline(&mut text).request_focus();
            })
            .drop_without_applying_deltas();
        assert_eq!(
            text, "a",
            "pointer cancellation discarded pending keyboard text"
        );
        assert!(context.input(|input| input.key_down(egui::Key::A)));
        assert!(!context.input(|input| input.pointer.any_down()));
        assert!(!context.input(|input| input.is_scrolling()));
        assert_eq!(
            context.input(|input| input.smooth_scroll_delta),
            egui::Vec2::ZERO
        );
    });
}

#[test]
#[ignore = "requires a Wayland compositor for the adapter clipboard only"]
fn device_loss_clears_all_supported_buttons_without_synthetic_releases() {
    with_input(|app, input| {
        let surface = app.compositor_state.create_surface(&app.qh);
        let context = egui::Context::default();
        input.handle_pointer_event(&PointerEvent {
            surface: surface.clone(),
            position: (20.0, 20.0),
            kind: PointerEventKind::Enter { serial: 1 },
        });
        for button in [0x110, 0x111, 0x112] {
            input.handle_pointer_event(&PointerEvent {
                surface: surface.clone(),
                position: (20.0, 20.0),
                kind: PointerEventKind::Press {
                    serial: 1,
                    time: 1,
                    button,
                },
            });
        }
        context
            .run_ui(input.take_raw_input(), |_| {})
            .drop_without_applying_deltas();
        for button in [
            egui::PointerButton::Primary,
            egui::PointerButton::Secondary,
            egui::PointerButton::Middle,
        ] {
            assert!(context.input(|input| input.pointer.button_down(button)));
        }
        input.handle_pointer_removed(&context);
        let raw = input.take_raw_input();
        assert!(
            !raw.events
                .iter()
                .any(|event| matches!(event, Event::PointerButton { pressed: false, .. })),
            "device loss fabricated a button release"
        );
        context.run_ui(raw, |_| {}).drop_without_applying_deltas();
        assert!(!context.input(|input| input.pointer.any_down()));
    });
}
#[cfg(feature = "clipboard")]
use smithay_clipboard::Clipboard;
use wayapp::Application;
use wayapp::WaylandToEguiInput;
#[cfg(feature = "clipboard")]
use wayland_client::Proxy;

fn with_input(test: impl FnOnce(&Application, &mut WaylandToEguiInput)) {
    let app = Application::new(|_| {});
    // SAFETY: app (and its native connection) outlives this input/clipboard.
    #[cfg(feature = "clipboard")]
    let clipboard = unsafe { Clipboard::new(app.conn.display().id().as_ptr() as *mut _) };
    #[cfg(feature = "clipboard")]
    let mut input = WaylandToEguiInput::new(clipboard);
    #[cfg(not(feature = "clipboard"))]
    let mut input = WaylandToEguiInput::new();
    test(&app, &mut input);
}

#[test]
#[ignore = "requires a Wayland compositor for the adapter clipboard only"]
fn first_repeat_after_enter_is_not_required_to_have_a_local_press() {
    with_input(|_, input| {
        input.handle_keyboard_enter();
        assert!(
            !input
                .take_raw_input()
                .events
                .iter()
                .any(|event| matches!(event, Event::Key { .. }))
        );
        let key = KeyEvent {
            time: 1,
            raw_code: 18,
            keysym: Keysym::e,
            utf8: Some("e".into()),
        };
        input.handle_keyboard_event(&key, true, true);
        let raw = input.take_raw_input();
        assert!(
            raw.events
                .iter()
                .any(|event| matches!(event, Event::Text(text) if text == "e"))
        );
        assert!(raw.events.iter().any(|event| matches!(
            event,
            Event::Key {
                key: egui::Key::E,
                pressed: true,
                repeat: true,
                ..
            }
        )));
        input.handle_keyboard_event(&key, false, false);
        assert!(input.take_raw_input().events.iter().any(|event| matches!(
            event,
            Event::Key {
                key: egui::Key::E,
                pressed: false,
                ..
            }
        )));
    });
}

#[test]
#[ignore = "requires a Wayland compositor"]
fn modifier_only_changes_reach_egui() {
    with_input(|_, input| {
        let context = egui::Context::default();
        input.handle_keyboard_enter();
        input.update_modifiers(&Modifiers {
            shift: true,
            ..Default::default()
        });
        context
            .run_ui(input.take_raw_input(), |_| {})
            .drop_without_applying_deltas();
        assert!(context.input(|i| i.modifiers.shift));
        input.update_modifiers(&Modifiers::default());
        context
            .run_ui(input.take_raw_input(), |_| {})
            .drop_without_applying_deltas();
        assert_eq!(context.input(|i| i.modifiers), egui::Modifiers::default());
    });
}

#[test]
#[ignore = "requires a Wayland compositor"]
fn clicking_after_enter_uses_enter_coordinates() {
    with_input(|app, input| {
        let surface = app.compositor_state.create_surface(&app.qh);
        let position = (31.0, 47.0);
        input.handle_pointer_event(&PointerEvent {
            surface: surface.clone(),
            position,
            kind: PointerEventKind::Enter { serial: 1 },
        });
        input.handle_pointer_event(&PointerEvent {
            surface,
            position,
            kind: PointerEventKind::Press {
                time: 1,
                button: 0x110,
                serial: 2,
            },
        });
        let events = input.take_raw_input().events;
        assert!(events.iter().any(
            |event| matches!(event, Event::PointerMoved(pos) if *pos == Pos2::new(31.0, 47.0))
        ));
        assert!(events.iter().any(
            |event| matches!(event, Event::PointerButton {pos, ..} if *pos == Pos2::new(31.0, 47.0))
        ));
    });
}

#[test]
#[ignore = "requires a Wayland compositor"]
fn repeating_nontext_key_does_not_repeat_previous_text() {
    with_input(|_, input| {
        input.handle_keyboard_event(
            &KeyEvent {
                time: 1,
                raw_code: 30,
                keysym: Keysym::a,
                utf8: Some("a".into()),
            },
            true,
            false,
        );
        input.take_raw_input();
        let arrow = KeyEvent {
            time: 2,
            raw_code: 105,
            keysym: Keysym::Left,
            utf8: None,
        };
        input.handle_keyboard_event(&arrow, true, false);
        input.handle_keyboard_event(&arrow, true, true);
        assert!(
            !input
                .take_raw_input()
                .events
                .iter()
                .any(|event| matches!(event, Event::Text(_)))
        );
    });
}

#[test]
#[ignore = "requires a Wayland compositor"]
fn batched_focus_and_alias_sequences_keep_exact_logical_key_state() {
    use std::collections::HashMap;
    use std::collections::HashSet;
    with_input(|_, input| {
        let context = egui::Context::default();
        let mut physical = HashMap::<u32, Option<egui::Key>>::new();
        let symbols = [
            (Keysym::a, Some(egui::Key::A)),
            (Keysym::A, Some(egui::Key::A)),
            (Keysym::b, Some(egui::Key::B)),
            (Keysym::B, Some(egui::Key::B)),
            (Keysym::Return, Some(egui::Key::Enter)),
            (Keysym::KP_Enter, Some(egui::Key::Enter)),
            (Keysym::Left, Some(egui::Key::ArrowLeft)),
            (Keysym::Right, Some(egui::Key::ArrowRight)),
            (Keysym::eacute, None),
        ];
        let codes = [28, 96, 30, 48];
        let mut seed = 0x4d59_5df4_d0f3_3173_u64;
        let mut saw_enter_alias = false;
        let mut saw_repeat_change = false;
        let mut saw_changed_release = false;
        let mut saw_unmapped = false;
        let mut saw_focus_batch = false;
        input.handle_keyboard_enter();
        for step in 0..512 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let action = (seed >> 32) as usize % 5;
            let raw_code = codes[(seed >> 16) as usize % codes.len()];
            let (keysym, logical) = symbols[(seed >> 48) as usize % symbols.len()];
            let key = KeyEvent {
                time: step,
                raw_code,
                keysym,
                utf8: None,
            };
            match action {
                0 => {
                    input.handle_keyboard_event(&key, true, false);
                    physical.insert(raw_code, logical);
                }
                1 => {
                    // Release-time symbols deliberately differ from presses.
                    saw_changed_release |=
                        physical.get(&raw_code).is_some_and(|old| *old != logical);
                    input.handle_keyboard_event(&key, false, false);
                    physical.remove(&raw_code);
                }
                2 => {
                    if let Some(held) = physical.get_mut(&raw_code) {
                        // This model tracks local presses; enter-held repeats
                        // are covered separately rather than treated as stale.
                        input.handle_keyboard_event(&key, true, true);
                        saw_repeat_change |= *held != logical;
                        *held = logical;
                    }
                }
                3 => {
                    saw_focus_batch = true;
                    // Keep all transitions in one RawInput batch. A pending
                    // old-focus press must not survive the leave/re-enter.
                    input.handle_keyboard_event(
                        &KeyEvent {
                            time: step,
                            raw_code: 31,
                            keysym: Keysym::a,
                            utf8: None,
                        },
                        true,
                        false,
                    );
                    input.handle_keyboard_leave();
                    input.handle_keyboard_enter();
                    input.handle_keyboard_event(&key, true, false);
                    physical.clear();
                    physical.insert(raw_code, logical);
                }
                _ => input.update_modifiers(&Modifiers {
                    shift: seed & 1 != 0,
                    ctrl: seed & 2 != 0,
                    ..Default::default()
                }),
            }
            context
                .run_ui(input.take_raw_input(), |_| {})
                .drop_without_applying_deltas();
            let expected: HashSet<_> = physical.values().filter_map(|key| *key).collect();
            saw_enter_alias |= physical
                .values()
                .filter(|key| **key == Some(egui::Key::Enter))
                .count()
                > 1;
            saw_unmapped |= physical.values().any(Option::is_none);
            let actual: HashSet<_> =
                context.input(|input| input.keys_down.iter().copied().collect());
            assert_eq!(
                actual, expected,
                "logical keys diverged at step {step}, action {action}, raw {raw_code}, symbol \
                 {keysym:?}"
            );
            assert!(context.input(|input| input.raw.focused));
        }
        assert!(
            saw_enter_alias
                && saw_repeat_change
                && saw_changed_release
                && saw_unmapped
                && saw_focus_batch,
            "fixed sequence lost required alias/repeat/release/unmapped/focus coverage"
        );
    });
}

#[test]
#[ignore = "requires a Wayland compositor"]
fn missing_repeat_text_does_not_reuse_or_resurrect_a_changed_keysym_cache() {
    for (original, changed) in [(Keysym::e, Keysym::E), (Keysym::eacute, Keysym::odiaeresis)] {
        with_input(|_, input| {
            let mut key = KeyEvent {
                time: 1,
                raw_code: 18,
                keysym: original,
                utf8: Some("é".into()),
            };
            input.handle_keyboard_event(&key, true, false);
            input.take_raw_input();
            key.utf8 = None;
            input.handle_keyboard_event(&key, true, true);
            assert!(
                input
                    .take_raw_input()
                    .events
                    .iter()
                    .any(|event| matches!(event, Event::Text(text) if text == "é")),
                "unchanged composition cache was lost"
            );
            input.update_modifiers(&Modifiers {
                num_lock: true,
                ..Default::default()
            });
            input.handle_keyboard_event(&key, true, true);
            assert!(
                input
                    .take_raw_input()
                    .events
                    .iter()
                    .any(|event| matches!(event, Event::Text(text) if text == "é")),
                "unrelated lock change discarded stable fallback text"
            );
            input.update_modifiers(&Modifiers {
                caps_lock: true,
                ..Default::default()
            });
            key.keysym = changed;
            input.handle_keyboard_event(&key, true, true);
            assert!(
                !input
                    .take_raw_input()
                    .events
                    .iter()
                    .any(|event| matches!(event, Event::Text(_))),
                "changed keysym reused old composed text"
            );
            input.update_modifiers(&Modifiers::default());
            key.keysym = original;
            input.handle_keyboard_event(&key, true, true);
            assert!(
                !input
                    .take_raw_input()
                    .events
                    .iter()
                    .any(|event| matches!(event, Event::Text(_))),
                "returning to the old keysym resurrected an invalidated composition"
            );
            input.handle_keyboard_event(&key, false, false);
            key.utf8 = Some("é".into());
            input.handle_keyboard_event(&key, true, false);
            input.take_raw_input();
            key.utf8 = None;
            input.handle_keyboard_event(&key, true, true);
            assert!(
                input
                    .take_raw_input()
                    .events
                    .iter()
                    .any(|event| matches!(event, Event::Text(text) if text == "é")),
                "new press did not restore fallback"
            );
        });
    }
}

#[test]
#[ignore = "requires a Wayland compositor"]
fn text_repeat_fallback_is_scoped_to_the_pressed_key_and_focus() {
    with_input(|_, input| {
        let mut key = KeyEvent {
            time: 1,
            raw_code: 30,
            keysym: Keysym::a,
            utf8: Some("a".into()),
        };
        input.handle_keyboard_event(&key, true, false);
        input.take_raw_input();
        key.utf8 = None;
        input.handle_keyboard_event(&key, true, true);
        assert!(
            input
                .take_raw_input()
                .events
                .iter()
                .any(|event| matches!(event, Event::Text(text) if text == "a"))
        );
        input.handle_keyboard_leave();
        input.handle_keyboard_enter();
        input.handle_keyboard_event(&key, true, true);
        assert!(
            !input
                .take_raw_input()
                .events
                .iter()
                .any(|event| matches!(event, Event::Text(_)))
        );
    });
}

#[test]
#[ignore = "requires a Wayland compositor for the adapter clipboard only"]
fn explicit_repeat_text_replaces_older_fallback_for_the_same_keysym() {
    with_input(|_, input| {
        input.handle_keyboard_enter();
        let mut key = KeyEvent {
            time: 1,
            raw_code: 18,
            keysym: Keysym::e,
            utf8: Some("é".into()),
        };
        input.handle_keyboard_event(&key, true, false);
        input.take_raw_input();
        key.utf8 = Some("e".into());
        input.handle_keyboard_event(&key, true, true);
        input.take_raw_input();
        key.utf8 = None;
        input.handle_keyboard_event(&key, true, true);
        let text: Vec<_> = input
            .take_raw_input()
            .events
            .into_iter()
            .filter_map(|event| match event {
                Event::Text(text) => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(
            text,
            ["e"],
            "repeat fallback resurrected an older composition"
        );
    });
}

#[test]
#[ignore = "requires a Wayland compositor for the adapter clipboard only"]
fn ime_leave_removes_only_the_uncommitted_composition() {
    for keyboard_leave in [false, true] {
        with_input(|_, input| {
            let context = egui::Context::default();
            let mut text = String::new();
            let frame = |input: &mut WaylandToEguiInput, text: &mut String| {
                context
                    .run_ui(input.take_raw_input(), |ui| {
                        ui.text_edit_singleline(text).request_focus();
                    })
                    .drop_without_applying_deltas();
            };
            input.handle_keyboard_enter();
            frame(input, &mut text);
            input.handle_ime_enter();
            input.handle_ime_commit("保存");
            frame(input, &mut text);
            assert_eq!(text, "保存");
            input.handle_ime_preedit_string("未確定", 0, 9);
            frame(input, &mut text);
            assert_eq!(text, "保存未確定");
            if keyboard_leave {
                input.handle_keyboard_leave();
            } else {
                input.handle_ime_leave();
            }
            frame(input, &mut text);
            assert_eq!(text, "保存", "IME leave kept uncommitted preedit text");
            input.handle_ime_leave();
            frame(input, &mut text);
            assert_eq!(text, "保存", "repeated leave deleted committed text");
        });
    }
}
