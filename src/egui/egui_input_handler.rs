//! EGUI view manager implementation
//!
//! This module provides a ViewManager-based approach to handling EGUI surfaces
//! following the pattern from single_color.rs

use egui::Event;
use egui::ImeEvent;
use egui::Key;
use egui::Modifiers as EguiModifiers;
use egui::PointerButton;
use egui::Pos2;
use egui::RawInput;
use log::trace;
use smithay_client_toolkit::seat::keyboard::KeyEvent;
use smithay_client_toolkit::seat::keyboard::Keysym;
use smithay_client_toolkit::seat::keyboard::Modifiers as WaylandModifiers;
use smithay_client_toolkit::seat::pointer::AxisScroll;
use smithay_client_toolkit::seat::pointer::PointerEvent;
use smithay_client_toolkit::seat::pointer::PointerEventKind;
use smithay_clipboard::Clipboard;
use std::collections::HashMap;
use std::time::Instant;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;

/// Handles input events from Wayland and converts them to EGUI RawInput
pub struct WaylandToEguiInput {
    modifiers: EguiModifiers,
    pointer_pos: Pos2,
    has_pointer: bool,
    events: Vec<Event>,
    screen_width: u32,
    screen_height: u32,
    start_time: Instant,
    clipboard: Clipboard,
    last_key_utf8: Option<(u32, Keysym, String)>,
    pressed_keys: HashMap<u32, Option<Key>>,
    has_keyboard_focus: bool,
}

impl WaylandToEguiInput {
    pub fn new(clipboard: Clipboard) -> Self {
        Self {
            modifiers: EguiModifiers::default(),
            pointer_pos: Pos2::ZERO,
            has_pointer: false,
            events: Vec::new(),
            screen_width: 256,
            screen_height: 256,
            start_time: Instant::now(),
            clipboard,
            last_key_utf8: None,
            pressed_keys: HashMap::new(),
            has_keyboard_focus: false,
        }
    }

    pub fn set_screen_size(&mut self, width: u32, height: u32) {
        self.screen_width = width;
        self.screen_height = height;
    }

    pub fn handle_pointer_event(&mut self, event: &PointerEvent) {
        match &event.kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                self.update_pointer_position(event.position);
            }
            PointerEventKind::Leave { .. } => {
                self.has_pointer = false;
                self.events.push(Event::PointerGone);
            }
            PointerEventKind::Press { button, .. } => {
                if let Some(egui_button) = wayland_button_to_egui(*button) {
                    self.events.push(Event::PointerButton {
                        pos: self.pointer_pos,
                        button: egui_button,
                        pressed: true,
                        modifiers: self.modifiers,
                    });
                }
            }
            PointerEventKind::Release { button, .. } => {
                if let Some(egui_button) = wayland_button_to_egui(*button) {
                    self.events.push(Event::PointerButton {
                        pos: self.pointer_pos,
                        button: egui_button,
                        pressed: false,
                        modifiers: self.modifiers,
                    });
                }
            }
            PointerEventKind::Axis {
                horizontal,
                vertical,
                ..
            } => {
                // Each axis can use a different source/unit. Preserve smooth
                // touchpad movement and fractional high-resolution wheel steps.
                for (axis, horizontal) in [(horizontal, true), (vertical, false)] {
                    let (unit, delta) = axis_scroll_delta(axis);
                    if delta != 0.0 {
                        self.events.push(Event::MouseWheel {
                            phase: egui::TouchPhase::Move,
                            unit,
                            delta: if horizontal {
                                egui::vec2(delta, 0.0)
                            } else {
                                egui::vec2(0.0, delta)
                            },
                            modifiers: self.modifiers,
                        });
                    }
                }
            }
        }
    }

    pub(crate) fn update_pointer_position(&mut self, position: (f64, f64)) {
        self.has_pointer = true;
        self.pointer_pos = Pos2::new(position.0 as f32, position.1 as f32);
        self.events.push(Event::PointerMoved(self.pointer_pos));
    }

    /// Re-express the native location after a coordinate-system change, without
    /// resurrecting a pointer that already left or replacing queued
    /// transitions.
    pub(crate) fn refresh_pointer_position(&mut self) {
        if self.has_pointer
            && !self.events.iter().any(|event| {
                matches!(
                    event,
                    Event::PointerMoved(_) | Event::PointerGone | Event::PointerButton { .. }
                )
            })
        {
            self.events.push(Event::PointerMoved(self.pointer_pos));
        }
    }

    pub fn handle_keyboard_enter(&mut self) {
        self.has_keyboard_focus = true;
        self.events.push(Event::WindowFocused(true));
    }

    /// Cancel a removed device, not an ordinary pointer leave. A synthetic
    /// release can click a widget, while PointerGone alone leaves buttons held.
    pub fn handle_pointer_removed(&mut self, context: &egui::Context) {
        self.events.retain(|event| {
            !matches!(
                event,
                Event::PointerMoved(_)
                    | Event::PointerButton { .. }
                    | Event::PointerGone
                    | Event::MouseWheel { .. }
            )
        });
        self.pointer_pos = Pos2::ZERO;
        self.has_pointer = false;
        context.input_mut(|input| input.pointer = Default::default());
        context.stop_dragging();
        self.events.push(Event::PointerGone);
        self.events.push(Event::MouseWheel {
            phase: egui::TouchPhase::Cancel,
            unit: egui::MouseWheelUnit::Point,
            delta: egui::Vec2::ZERO,
            modifiers: self.modifiers,
        });
    }

    pub fn handle_keyboard_leave(&mut self) {
        self.handle_ime_leave();
        self.has_keyboard_focus = false;
        self.last_key_utf8 = None;
        self.pressed_keys.clear();
        self.modifiers = EguiModifiers::default();
        self.events.push(Event::ModifiersChanged(self.modifiers));
        self.events.push(Event::WindowFocused(false));
    }

    pub fn handle_keyboard_event(&mut self, event: &KeyEvent, pressed: bool, is_repeat: bool) {
        let mapped_key = keysym_to_egui_key(event.keysym);
        if !pressed && !self.pressed_keys.contains_key(&event.raw_code) {
            return;
        }
        if is_repeat
            && self
                .last_key_utf8
                .as_ref()
                .is_some_and(|(code, keysym, _)| *code == event.raw_code && *keysym != event.keysym)
        {
            // Normalizing e/E to one egui key must not preserve old composed
            // text across Caps Lock/layout changes, even if the symbol returns.
            self.last_key_utf8 = None;
        }
        let old_key = if pressed {
            self.pressed_keys
                .insert(event.raw_code, mapped_key)
                .flatten()
        } else {
            self.pressed_keys.remove(&event.raw_code).flatten()
        };
        // Keysyms can change while the physical key stays held (Shift/layout).
        // Release the old logical identity only once no physical alias holds it.
        if pressed
            && old_key != mapped_key
            && let Some(old) = old_key
            && !self.pressed_keys.values().any(|key| *key == Some(old))
        {
            self.push_key(old, false, false);
        }
        let key = if pressed {
            mapped_key
        } else {
            old_key.filter(|old| !self.pressed_keys.values().any(|key| *key == Some(*old)))
        };
        if pressed && (!is_repeat || event.utf8.is_some()) {
            self.last_key_utf8 = event
                .utf8
                .clone()
                .map(|text| (event.raw_code, event.keysym, text));
        } else if !pressed
            && self
                .last_key_utf8
                .as_ref()
                .is_some_and(|(code, _, _)| *code == event.raw_code)
        {
            self.last_key_utf8 = None;
        }
        // Extra Alt belongs to a distinct shortcut (or a layout's AltGr
        // combination), not Copy/Cut/Paste. Preserve the ordinary key event.
        if pressed && !is_repeat && self.modifiers.ctrl && !self.modifiers.alt {
            match key {
                Some(Key::C) => self.events.push(Event::Copy),
                Some(Key::X) => self.events.push(Event::Cut),
                Some(Key::V) => self
                    .events
                    .push(Event::Paste(self.clipboard.load().unwrap_or_default())),
                _ => (),
            }
        }

        if let Some(key) = key {
            self.push_key(key, pressed, is_repeat);
        } else if pressed && mapped_key.is_none() {
            trace!(
                "[INPUT] No EGUI key mapping for keysym: {:?}",
                event.keysym.raw()
            );
        }

        // XKB can return printable Unicode even for Control shortcuts (e.g.
        // Ctrl+ö). Such shortcuts must not insert text. Keep Ctrl+Alt available
        // to layouts/platforms that represent AltGr using that combination.
        if (pressed || is_repeat) && (!self.modifiers.ctrl || self.modifiers.alt) {
            let mut text = event.utf8.clone();
            if is_repeat && text.is_none() {
                text = self
                    .last_key_utf8
                    .as_ref()
                    .filter(|(code, keysym, _)| *code == event.raw_code && *keysym == event.keysym)
                    .map(|(_, _, text)| text.clone());
            }
            if let Some(text) = text {
                if !text.chars().any(|c| c.is_control()) {
                    self.events.push(Event::Text(text.clone()));
                }
            }
        }
    }

    fn push_key(&mut self, key: Key, pressed: bool, repeat: bool) {
        self.events.push(Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat,
            modifiers: self.modifiers,
        });
    }

    pub fn update_modifiers(&mut self, wayland_mods: &WaylandModifiers) {
        let modifiers = EguiModifiers {
            alt: wayland_mods.alt,
            ctrl: wayland_mods.ctrl,
            shift: wayland_mods.shift,
            mac_cmd: false,
            command: wayland_mods.ctrl,
        };
        if modifiers != self.modifiers {
            self.modifiers = modifiers;
            self.last_key_utf8 = None;
            self.events.push(Event::ModifiersChanged(modifiers));
        }
    }

    pub fn take_raw_input(&mut self) -> RawInput {
        let events = std::mem::take(&mut self.events);
        RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(self.screen_width as f32, self.screen_height as f32),
            )),
            time: Some(self.start_time.elapsed().as_secs_f64()),
            predicted_dt: 1.0 / 60.0,
            events,
            hovered_files: Vec::new(),
            dropped_files: Vec::new(),
            focused: self.has_keyboard_focus,
            ..Default::default()
        }
    }

    pub fn handle_output_command(&mut self, output: &egui::OutputCommand) {
        match output {
            egui::OutputCommand::CopyText(text) => {
                self.clipboard.store(text.clone());
            }
            egui::OutputCommand::CopyImage(_image) => {
                // Handle image copy if needed
                trace!("[INPUT] CopyImage command received (not implemented)");
                // TODO: Implement image copying to clipboard if required
            }
            egui::OutputCommand::OpenUrl(url) => {
                trace!("[INPUT] OpenUrl command received: {}", url.url);
            }
        }
    }

    pub fn handle_ime_enter(&mut self) {
        trace!("[INPUT] IME enabled");
        // ImeEvent::Enabled is deprecated and no longer used by egui
    }

    pub fn handle_ime_leave(&mut self) {
        trace!("[INPUT] IME disabled");
        // Disabled is ignored by egui. Empty preedit cancels only an active
        // composition, retaining committed text and ordinary selections.
        self.events.push(Event::Ime(ImeEvent::Preedit {
            text: String::new(),
            active_range_chars: None,
        }));
    }

    pub fn handle_ime_commit(&mut self, text: &str) {
        trace!("[INPUT] Committing IME text: {}", text);
        self.events
            .push(Event::Ime(ImeEvent::Commit(text.to_string())));
    }

    pub fn handle_ime_preedit_string(&mut self, text: &str, _cursor_begin: i32, _cursor_end: i32) {
        trace!("[INPUT] IME preedit: {:?}", text);
        self.events.push(Event::Ime(ImeEvent::Preedit {
            text: text.to_string(),
            active_range_chars: None,
        }));
    }

    pub fn handle_ime_delete_surrounding_text(&mut self, before_length: u32, after_length: u32) {
        trace!(
            "[INPUT] IME delete surrounding text: before={} bytes, after={} bytes",
            before_length, after_length
        );
        // egui has no native DeleteSurroundingText event. Approximate with key
        // presses: divide byte counts by 3 (UTF-8 CJK chars are 3
        // bytes) to get character counts. (SLOP needs testing)

        // for _ in 0..before_length.div_ceil(3) {
        //     self.events.push(Event::Key {
        //         key: Key::Backspace,
        //         physical_key: None,
        //         pressed: true,
        //         repeat: false,
        //         modifiers: self.modifiers,
        //     });
        // }
        // for _ in 0..after_length.div_ceil(3) {
        //     self.events.push(Event::Key {
        //         key: Key::Delete,
        //         physical_key: None,
        //         pressed: true,
        //         repeat: false,
        //         modifiers: self.modifiers,
        //     });
        // }
    }
}

fn axis_scroll_delta(axis: &AxisScroll) -> (egui::MouseWheelUnit, f32) {
    // Wayland reports positive axes for right/down. Egui describes movement
    // of the content, whose direction is the opposite on both axes.
    if axis.value120 != 0 {
        (egui::MouseWheelUnit::Line, -(axis.value120 as f32) / 120.0)
    } else if axis.discrete != 0 {
        (egui::MouseWheelUnit::Line, -(axis.discrete as f32))
    } else {
        (egui::MouseWheelUnit::Point, -(axis.absolute as f32))
    }
}

fn wayland_button_to_egui(button: u32) -> Option<PointerButton> {
    // Linux button codes (from linux/input-event-codes.h)
    match button {
        0x110 => Some(PointerButton::Primary),
        0x111 => Some(PointerButton::Secondary),
        0x112 => Some(PointerButton::Middle),
        _ => None,
    }
}

fn keysym_to_egui_key(keysym: Keysym) -> Option<Key> {
    // ASCII letter keysyms encode case. Egui carries Shift separately, so its
    // logical key identity must stay the same for shifted letters/shortcuts.
    let keysym = if (u32::from(b'A')..=u32::from(b'Z')).contains(&keysym.raw()) {
        Keysym::new(keysym.raw() + u32::from(b'a' - b'A'))
    } else {
        keysym
    };
    // XKB already resolves Num Lock/layout into keypad navigation or numeric
    // symbols. Normalize those aliases rather than guessing modifier bits.
    Some(match keysym {
        // Commands:
        Keysym::downarrow | Keysym::Down | Keysym::KP_Down => Key::ArrowDown,
        Keysym::leftarrow | Keysym::Left | Keysym::KP_Left => Key::ArrowLeft,
        Keysym::rightarrow | Keysym::Right | Keysym::KP_Right => Key::ArrowRight,
        Keysym::uparrow | Keysym::Up | Keysym::KP_Up => Key::ArrowUp,
        Keysym::Escape => Key::Escape,
        Keysym::Tab | Keysym::ISO_Left_Tab | Keysym::KP_Tab => Key::Tab,
        Keysym::BackSpace => Key::Backspace,
        Keysym::Return | Keysym::KP_Enter => Key::Enter,
        Keysym::Insert | Keysym::KP_Insert => Key::Insert,
        Keysym::Delete | Keysym::KP_Delete => Key::Delete,
        Keysym::Home | Keysym::KP_Home => Key::Home,
        Keysym::End | Keysym::KP_End => Key::End,
        Keysym::Prior | Keysym::KP_Prior => Key::PageUp,
        Keysym::Next | Keysym::KP_Next => Key::PageDown,
        // Punctuation:
        Keysym::space | Keysym::KP_Space => Key::Space,
        Keysym::colon => Key::Colon,
        Keysym::comma | Keysym::KP_Separator => Key::Comma,
        Keysym::minus | Keysym::KP_Subtract => Key::Minus,
        Keysym::period | Keysym::KP_Decimal => Key::Period,
        Keysym::plus | Keysym::KP_Add => Key::Plus,
        Keysym::equal | Keysym::KP_Equal => Key::Equals,
        Keysym::semicolon => Key::Semicolon,
        Keysym::bracketleft => Key::OpenBracket,
        Keysym::bracketright => Key::CloseBracket,
        Keysym::braceleft => Key::OpenCurlyBracket,
        Keysym::braceright => Key::CloseCurlyBracket,
        Keysym::grave => Key::Backtick,
        Keysym::backslash => Key::Backslash,
        Keysym::slash | Keysym::KP_Divide => Key::Slash,
        Keysym::bar => Key::Pipe,
        Keysym::question => Key::Questionmark,
        Keysym::exclam => Key::Exclamationmark,
        Keysym::apostrophe => Key::Quote,
        // Digits:
        Keysym::_0 | Keysym::KP_0 => Key::Num0,
        Keysym::_1 | Keysym::KP_1 => Key::Num1,
        Keysym::_2 | Keysym::KP_2 => Key::Num2,
        Keysym::_3 | Keysym::KP_3 => Key::Num3,
        Keysym::_4 | Keysym::KP_4 => Key::Num4,
        Keysym::_5 | Keysym::KP_5 => Key::Num5,
        Keysym::_6 | Keysym::KP_6 => Key::Num6,
        Keysym::_7 | Keysym::KP_7 => Key::Num7,
        Keysym::_8 | Keysym::KP_8 => Key::Num8,
        Keysym::_9 | Keysym::KP_9 => Key::Num9,
        // Letters:
        Keysym::a => Key::A,
        Keysym::b => Key::B,
        Keysym::c => Key::C,
        Keysym::d => Key::D,
        Keysym::e => Key::E,
        Keysym::f => Key::F,
        Keysym::g => Key::G,
        Keysym::h => Key::H,
        Keysym::i => Key::I,
        Keysym::j => Key::J,
        Keysym::k => Key::K,
        Keysym::l => Key::L,
        Keysym::m => Key::M,
        Keysym::n => Key::N,
        Keysym::o => Key::O,
        Keysym::p => Key::P,
        Keysym::q => Key::Q,
        Keysym::r => Key::R,
        Keysym::s => Key::S,
        Keysym::t => Key::T,
        Keysym::u => Key::U,
        Keysym::v => Key::V,
        Keysym::w => Key::W,
        Keysym::x => Key::X,
        Keysym::y => Key::Y,
        Keysym::z => Key::Z,
        // Function keys:
        Keysym::F1 => Key::F1,
        Keysym::F2 => Key::F2,
        Keysym::F3 => Key::F3,
        Keysym::F4 => Key::F4,
        Keysym::F5 => Key::F5,
        Keysym::F6 => Key::F6,
        Keysym::F7 => Key::F7,
        Keysym::F8 => Key::F8,
        Keysym::F9 => Key::F9,
        Keysym::F10 => Key::F10,
        Keysym::F11 => Key::F11,
        Keysym::F12 => Key::F12,
        Keysym::F13 => Key::F13,
        Keysym::F14 => Key::F14,
        Keysym::F15 => Key::F15,
        Keysym::F16 => Key::F16,
        Keysym::F17 => Key::F17,
        Keysym::F18 => Key::F18,
        Keysym::F19 => Key::F19,
        Keysym::F20 => Key::F20,
        Keysym::F21 => Key::F21,
        Keysym::F22 => Key::F22,
        Keysym::F23 => Key::F23,
        Keysym::F24 => Key::F24,
        Keysym::F25 => Key::F25,
        Keysym::F26 => Key::F26,
        Keysym::F27 => Key::F27,
        Keysym::F28 => Key::F28,
        Keysym::F29 => Key::F29,
        Keysym::F30 => Key::F30,
        Keysym::F31 => Key::F31,
        Keysym::F32 => Key::F32,
        Keysym::F33 => Key::F33,
        Keysym::F34 => Key::F34,
        Keysym::F35 => Key::F35,
        // Navigation keys:
        // Keysym::BrowserBack => Key::BrowserBack,
        _ => return None,
    })
}

pub fn egui_to_cursor_shape(cursor: egui::CursorIcon) -> Shape {
    use Shape as C;
    use egui::CursorIcon::*;

    match cursor {
        Default => C::Default,
        None => C::Default,
        ContextMenu => C::ContextMenu,
        Help => C::Help,
        PointingHand => C::Pointer,
        Progress => C::Progress,
        Wait => C::Wait,
        Cell => C::Cell,
        Crosshair => C::Crosshair,
        Text => C::Text,
        VerticalText => C::VerticalText,
        Alias => C::Alias,
        Copy => C::Copy,
        Move => C::Move,
        NoDrop => C::NoDrop,
        NotAllowed => C::NotAllowed,
        Grab => C::Grab,
        Grabbing => C::Grabbing,
        AllScroll => C::AllScroll,
        ResizeHorizontal => C::EwResize,
        ResizeNeSw => C::NeswResize,
        ResizeNwSe => C::NwseResize,
        ResizeVertical => C::NsResize,
        ResizeEast => C::EResize,
        ResizeSouthEast => C::SeResize,
        ResizeSouth => C::SResize,
        ResizeSouthWest => C::SwResize,
        ResizeWest => C::WResize,
        ResizeNorthWest => C::NwResize,
        ResizeNorth => C::NResize,
        ResizeNorthEast => C::NeResize,
        ResizeColumn => C::ColResize,
        ResizeRow => C::RowResize,
        ZoomIn => C::ZoomIn,
        ZoomOut => C::ZoomOut,
    }
}

#[cfg(test)]
mod scroll_tests {
    use super::*;

    #[test]
    fn upstream_mouse_buttons_preserve_their_identity() {
        for (code, expected) in [
            (0x110, PointerButton::Primary),
            (0x111, PointerButton::Secondary),
            (0x112, PointerButton::Middle),
        ] {
            assert_eq!(
                wayland_button_to_egui(code),
                Some(expected),
                "button {code:#x}"
            );
        }
        assert_eq!(wayland_button_to_egui(0), None);
    }

    #[test]
    fn shifted_letters_and_navigation_keep_key_identity() {
        for (keysym, expected) in [
            (Keysym::A, Key::A),
            (Keysym::Z, Key::Z),
            (Keysym::ISO_Left_Tab, Key::Tab),
            (Keysym::KP_Enter, Key::Enter),
        ] {
            assert_eq!(keysym_to_egui_key(keysym), Some(expected));
        }
    }

    #[test]
    fn keypad_keys_share_their_main_keyboard_logical_identity() {
        for (keysym, expected) in [
            (Keysym::KP_Down, Key::ArrowDown),
            (Keysym::KP_Left, Key::ArrowLeft),
            (Keysym::KP_Right, Key::ArrowRight),
            (Keysym::KP_Up, Key::ArrowUp),
            (Keysym::KP_Home, Key::Home),
            (Keysym::KP_End, Key::End),
            (Keysym::KP_Prior, Key::PageUp),
            (Keysym::KP_Next, Key::PageDown),
            (Keysym::KP_Insert, Key::Insert),
            (Keysym::KP_Delete, Key::Delete),
            (Keysym::KP_Tab, Key::Tab),
            (Keysym::KP_Space, Key::Space),
            (Keysym::KP_Equal, Key::Equals),
            (Keysym::KP_Add, Key::Plus),
            (Keysym::KP_Subtract, Key::Minus),
            (Keysym::KP_Divide, Key::Slash),
            (Keysym::KP_Decimal, Key::Period),
            (Keysym::KP_Separator, Key::Comma),
            (Keysym::KP_0, Key::Num0),
            (Keysym::KP_1, Key::Num1),
            (Keysym::KP_2, Key::Num2),
            (Keysym::KP_3, Key::Num3),
            (Keysym::KP_4, Key::Num4),
            (Keysym::KP_5, Key::Num5),
            (Keysym::KP_6, Key::Num6),
            (Keysym::KP_7, Key::Num7),
            (Keysym::KP_8, Key::Num8),
            (Keysym::KP_9, Key::Num9),
        ] {
            assert_eq!(keysym_to_egui_key(keysym), Some(expected), "{keysym:?}");
        }
    }

    #[test]
    fn touchpad_pixels_work_without_discrete_steps() {
        assert_eq!(
            axis_scroll_delta(&AxisScroll {
                absolute: 2.5,
                ..Default::default()
            }),
            (egui::MouseWheelUnit::Point, -2.5)
        );
    }

    #[test]
    fn high_resolution_steps_take_precedence_without_double_counting() {
        let axis = AxisScroll {
            value120: 60,
            discrete: 1,
            absolute: 15.0,
            ..Default::default()
        };
        assert_eq!(axis_scroll_delta(&axis), (egui::MouseWheelUnit::Line, -0.5));
    }

    #[test]
    fn legacy_steps_keep_line_units_and_direction() {
        assert_eq!(
            axis_scroll_delta(&AxisScroll {
                discrete: -1,
                ..Default::default()
            }),
            (egui::MouseWheelUnit::Line, 1.0)
        );
    }
}
