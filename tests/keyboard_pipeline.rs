//! Virtual keyboard input is confined to the private headless test compositor.
mod common;
#[cfg(feature = "clipboard")]
#[path = "common/slow_clipboard.rs"]
mod slow_clipboard;
use common::private_input_test_guard;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::KeyboardInteractivity;
use smithay_client_toolkit::shell::wlr_layer::Layer;
use smithay_client_toolkit::shell::wlr_layer::LayerSurface;
use std::cell::Cell;
use std::cell::RefCell;
use std::io::Write;
use std::os::fd::AsFd;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::channel;
use std::time::Duration;
use std::time::Instant;
use wayapp::Application;
use wayapp::DispatchToken;
use wayapp::EguiSurfaceState;
use wayapp::WaylandEvent;
use wayland_client::Connection;
use wayland_client::Dispatch;
use wayland_client::EventQueue;
use wayland_client::QueueHandle;
use wayland_client::delegate_noop;
use wayland_client::globals::GlobalListContents;
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_registry;
use wayland_client::protocol::wl_seat;
use smithay_client_toolkit::reexports::protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use smithay_client_toolkit::reexports::protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

struct KeyboardState;
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for KeyboardState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
delegate_noop!(KeyboardState: ignore wl_seat::WlSeat);
delegate_noop!(KeyboardState: ZwpVirtualKeyboardManagerV1);
delegate_noop!(KeyboardState: ZwpVirtualKeyboardV1);

struct VirtualKeyboard {
    keyboard: ZwpVirtualKeyboardV1,
    _manager: ZwpVirtualKeyboardManagerV1,
    seat: wl_seat::WlSeat,
    _queue: EventQueue<KeyboardState>,
    connection: Connection,
}

impl VirtualKeyboard {
    fn new() -> Self {
        common::require_private_compositor();
        let connection = Connection::connect_to_env().unwrap();
        let (globals, queue) = registry_queue_init::<KeyboardState>(&connection).unwrap();
        let qh = queue.handle();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=7, ()).unwrap();
        let manager: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).unwrap();
        let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
        let result = Self {
            keyboard,
            _manager: manager,
            seat,
            _queue: queue,
            connection,
        };
        result.set_keymap("us", "", "");
        result
    }

    fn set_keymap(&self, layout: &str, variant: &str, options: &str) {
        let compiled = std::process::Command::new("xkbcli")
            .args([
                "compile-keymap",
                "--layout",
                layout,
                "--variant",
                variant,
                "--model",
                "pc105",
                "--options",
                options,
            ])
            .output()
            .expect("xkbcli is required in the headless test shell");
        assert!(
            compiled.status.success(),
            "keymap compilation failed: {}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let mut keymap = compiled.stdout;
        keymap.push(0);
        static NEXT_KEYMAP: AtomicUsize = AtomicUsize::new(0);
        let path =
            std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap()).join(format!(
                "virtual-keyboard-{}-{}.keymap",
                std::process::id(),
                NEXT_KEYMAP.fetch_add(1, Ordering::Relaxed)
            ));
        let mut file = std::fs::File::create_new(path).unwrap();
        file.write_all(&keymap).unwrap();
        self.keyboard
            .keymap(1, file.as_fd(), keymap.len().try_into().unwrap());
        self.connection.roundtrip().unwrap();
    }

    fn modifiers(&self, depressed: u32, locked: u32) {
        self.keyboard.modifiers(depressed, 0, locked, 0);
        self.connection.roundtrip().unwrap();
    }

    fn tap(&self, time: u32, code: u32) {
        self.keyboard.key(time, code, 1);
        self.keyboard.key(time + 1, code, 0);
        self.connection.roundtrip().unwrap();
    }

    fn key(&self, time: u32, code: u32, pressed: bool) {
        self.keyboard.key(time, code, u32::from(pressed));
        self.connection.roundtrip().unwrap();
    }
}

impl Drop for VirtualKeyboard {
    fn drop(&mut self) {
        self.keyboard.modifiers(0, 0, 0, 0);
        self.keyboard.destroy();
        self.seat.release();
        let _ = self.connection.flush();
    }
}

struct KeyboardHarness<'guard> {
    surface: EguiSurfaceState<LayerSurface>,
    layer: LayerSurface,
    app: Application,
    keyboard: VirtualKeyboard,
    rx: Receiver<DispatchToken>,
    _guard: &'guard std::sync::MutexGuard<'static, ()>,
}

impl<'guard> KeyboardHarness<'guard> {
    fn new(guard: &'guard std::sync::MutexGuard<'static, ()>) -> Self {
        let keyboard = VirtualKeyboard::new();
        let (tx, rx) = channel();
        let mut app = Application::new(move |token| {
            let _ = tx.send(token);
        });
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            Layer::Top,
            Some("wayapp-keyboard-pipeline-test"),
            None,
        );
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.set_size(400, 100);
        layer.commit();
        let surface = EguiSurfaceState::new(&app, layer.clone(), 400, 100);
        app.run_dispatcher();
        Self {
            surface,
            layer,
            app,
            keyboard,
            rx,
            _guard: guard,
        }
    }

    fn pump_until(
        &mut self,
        ui: &mut impl FnMut(&mut egui::Ui),
        mut ready: impl FnMut(&[WaylandEvent]) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let token = self
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            let events = self.app.dispatch_pending(token);
            self.surface.handle_events(&mut self.app, &events, ui);
            if ready(&events) {
                return;
            }
        }
    }

    fn wait_for_focus(&mut self, ui: &mut impl FnMut(&mut egui::Ui)) {
        let target = self.layer.wl_surface().clone();
        self.pump_until(ui, |events| events.iter().any(|event| matches!(event, WaylandEvent::KeyboardEnter(surface, ..) if surface == &target)));
    }

    fn wait_for_key_release(&mut self, code: u32, ui: &mut impl FnMut(&mut egui::Ui)) {
        self.pump_until(ui, |events| {
            events
                .iter()
                .any(|event| matches!(event, WaylandEvent::KeyRelease(key) if key.raw_code == code))
        });
    }

    fn wait_for_press(&mut self, code: u32, ui: &mut impl FnMut(&mut egui::Ui)) {
        self.pump_until(ui, |events| {
            events
                .iter()
                .any(|event| matches!(event, WaylandEvent::KeyPress(key) if key.raw_code == code))
        });
    }

    #[cfg(feature = "clipboard")]
    fn wait_for_clipboard_selection(&mut self, expected: &str) {
        // Store is asynchronous; synchronize on an actual selection transfer.
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if self
                .app
                .clipboard
                .load()
                .is_ok_and(|value| value == expected)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "clipboard did not publish the expected selection"
            );
            self.app.conn.roundtrip().unwrap();
            std::thread::yield_now();
        }
    }

    fn pump_for(
        &mut self,
        duration: Duration,
        ui: &mut impl FnMut(&mut egui::Ui),
    ) -> Vec<WaylandEvent> {
        let deadline = Instant::now() + duration;
        let mut observed = Vec::new();
        loop {
            match self
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(token) => {
                    let events = self.app.dispatch_pending(token);
                    self.surface.handle_events(&mut self.app, &events, ui);
                    observed.extend(events);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return observed,
                Err(error) => panic!("keyboard dispatcher stopped unexpectedly: {error}"),
            }
            assert!(
                Instant::now() < deadline + Duration::from_secs(1),
                "dispatcher failed to settle"
            );
            if Instant::now() >= deadline {
                return observed;
            }
        }
    }
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn new_unfocused_view_inherits_current_modifiers_without_claiming_keyboard_focus() {
    let guard = private_input_test_guard();
    let mut harness = KeyboardHarness::new(&guard);
    let mut owner_ui = |ui: &mut egui::Ui| {
        ui.label("keyboard focus owner");
    };
    harness.wait_for_focus(&mut owner_ui);
    harness.keyboard.modifiers(1 << 2, 0);
    harness.pump_until(&mut owner_ui, |events| {
        events.iter().any(
            |event| matches!(event, WaylandEvent::ModifiersChanged(modifiers) if modifiers.ctrl),
        )
    });
    let layer = harness.app.layer_shell.create_layer_surface(
        &harness.app.qh,
        harness.app.compositor_state.create_surface(&harness.app.qh),
        Layer::Bottom,
        Some("wayapp-new-unfocused-modifiers"),
        None,
    );
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.set_size(200, 100);
    layer.commit();
    let mut view = EguiSurfaceState::new(&harness.app, &layer, 200, 100);
    let frames = Cell::new(0);
    let state = Cell::new((false, false));
    let mut ui = |ui: &mut egui::Ui| {
        frames.set(frames.get() + 1);
        state.set(ui.input(|input| (input.modifiers.ctrl, input.raw.focused)));
        ui.label("unfocused modifier probe");
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while frames.get() == 0 {
        let token = harness
            .rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        let events = harness.app.dispatch_pending(token);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, WaylandEvent::ModifiersChanged(_))),
            "new view must initialize without another native modifier update"
        );
        harness
            .surface
            .handle_events(&mut harness.app, &events, &mut owner_ui);
        view.handle_events(&mut harness.app, &events, &mut ui);
    }
    assert_eq!(
        state.get(),
        (true, false),
        "new unfocused view lost held Control or claimed focus"
    );
    harness.keyboard.modifiers(0, 0);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let token = harness
            .rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        let events = harness.app.dispatch_pending(token);
        harness
            .surface
            .handle_events(&mut harness.app, &events, &mut owner_ui);
        view.handle_events(&mut harness.app, &events, &mut ui);
        if events.iter().any(
            |event| matches!(event, WaylandEvent::ModifiersChanged(modifiers) if !modifiers.ctrl),
        ) {
            break;
        }
    }
    assert_eq!(state.get(), (false, false));
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn caps_lock_control_a_selects_all_in_real_egui_text_edit() {
    let guard = private_input_test_guard();
    let mut harness = KeyboardHarness::new(&guard);
    let mut text = String::from("original");
    let mut ui = |ui: &mut egui::Ui| {
        ui.text_edit_singleline(&mut text).request_focus();
    };
    harness.wait_for_focus(&mut ui);
    // US XKB modifier indices: Control = bit 2; Lock = bit 1.
    // With Caps Lock, the compositor sends an uppercase A keysym for Ctrl+A.
    harness.keyboard.modifiers(1 << 2, 1 << 1);
    harness.keyboard.tap(1, 30); // Linux KEY_A
    harness.keyboard.modifiers(0, 0);
    harness.keyboard.tap(10, 48); // Linux KEY_B
    harness.wait_for_key_release(48, &mut ui);
    assert_eq!(
        text, "b",
        "Caps Lock Ctrl+A failed to select and replace existing text"
    );
    harness
        .app
        .conn
        .roundtrip()
        .expect("virtual keyboard produced a protocol error");
}

fn check_recreated_keyboard_view(held_control: bool) {
    let guard = private_input_test_guard();
    let mut harness = KeyboardHarness::new(&guard);
    let text = RefCell::new(String::from("original"));
    let frames = Cell::new(0usize);
    let focused = Cell::new(false);
    let mut ui = |ui: &mut egui::Ui| {
        frames.set(frames.get() + 1);
        focused.set(ui.input(|input| input.raw.focused));
        ui.text_edit_singleline(&mut *text.borrow_mut())
            .request_focus();
    };
    harness.wait_for_focus(&mut ui);
    if held_control {
        harness.keyboard.modifiers(1 << 2, 0);
        harness.pump_until(&mut ui, |events| events.iter().any(|event| matches!(event, WaylandEvent::ModifiersChanged(modifiers) if modifiers.ctrl)));
    }
    // Preserve the native role/focus, but destroy the previous GPU/input state
    // before constructing a replacement on that same surface.
    drop(harness.surface);
    harness.surface = EguiSurfaceState::new(&harness.app, harness.layer.clone(), 400, 100);
    let before = frames.get();
    let focus_changed = Cell::new(false);
    harness.surface.request_frame();
    harness.pump_until(&mut ui, |events| {
        if events.iter().any(|event| {
            matches!(
                event,
                WaylandEvent::KeyboardEnter(..) | WaylandEvent::KeyboardLeave(_)
            )
        }) {
            focus_changed.set(true);
        }
        frames.get() > before
    });
    let events = harness.pump_for(Duration::from_millis(30), &mut ui);
    assert!(
        !focus_changed.get()
            && !events.iter().any(|event| matches!(
                event,
                WaylandEvent::KeyboardEnter(..) | WaylandEvent::KeyboardLeave(_)
            )),
        "replacement changed native focus instead of inheriting it"
    );
    if held_control {
        harness.keyboard.tap(10, 30); // Ctrl+A must use the modifier held before replacement.
        harness.wait_for_key_release(30, &mut ui);
        harness.keyboard.modifiers(0, 0);
    }
    harness.keyboard.tap(20, 48);
    harness.wait_for_key_release(48, &mut ui);
    assert_eq!(
        *text.borrow(),
        if held_control { "b" } else { "originalb" },
        "replacement view lost native focus or held modifiers"
    );
    assert!(focused.get(), "replacement egui context stayed unfocused");
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn recreated_focused_view_still_accepts_native_keyboard_input() {
    check_recreated_keyboard_view(false);
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn recreated_focused_view_keeps_held_modifiers_for_shortcuts() {
    check_recreated_keyboard_view(true);
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn keypad_navigation_edits_unicode_and_num_lock_reports_logical_digits() {
    let guard = private_input_test_guard();
    let mut harness = KeyboardHarness::new(&guard);
    let text = RefCell::new(String::from("aé中"));
    let digits = RefCell::new(Vec::new());
    let any_key_down = Cell::new(false);
    let mut ui = |ui: &mut egui::Ui| {
        ui.text_edit_singleline(&mut *text.borrow_mut())
            .request_focus();
        ui.input(|input| {
            any_key_down.set(!input.keys_down.is_empty());
            for event in &input.events {
                if let egui::Event::Key {
                    key, pressed: true, ..
                } = event
                {
                    digits.borrow_mut().push(*key);
                }
            }
        });
    };
    harness.wait_for_focus(&mut ui);
    harness.keyboard.modifiers(0, 0);
    harness.keyboard.tap(1, 71); // KP7 -> KP_Home with Num Lock off.
    harness.wait_for_key_release(71, &mut ui);
    harness.keyboard.tap(10, 83); // KP_Decimal -> KP_Delete.
    harness.wait_for_key_release(83, &mut ui);
    assert_eq!(
        *text.borrow(),
        "é中",
        "keypad Home/Delete did not edit text"
    );
    harness.keyboard.tap(20, 79); // KP1 -> KP_End.
    harness.wait_for_key_release(79, &mut ui);
    harness.keyboard.tap(30, 75); // KP4 -> KP_Left.
    harness.wait_for_key_release(75, &mut ui);
    harness.keyboard.tap(40, 83);
    harness.wait_for_key_release(83, &mut ui);
    assert_eq!(
        *text.borrow(),
        "é",
        "keypad End/Left split or retained a Unicode character"
    );
    harness.keyboard.modifiers(0, 1 << 4); // US Num Lock.
    for (time, code) in [(50, 79), (60, 80), (70, 83)] {
        harness.keyboard.tap(time, code);
        harness.wait_for_key_release(code, &mut ui);
    }
    assert_eq!(*text.borrow(), "é12.");
    for key in [egui::Key::Num1, egui::Key::Num2, egui::Key::Period] {
        assert!(
            digits.borrow().contains(&key),
            "numeric keypad did not report {key:?}"
        );
    }
    harness.keyboard.modifiers(0, 0);
    harness.keyboard.key(80, 79, true); // Hold KP_End, then change its meaning.
    harness.wait_for_press(79, &mut ui);
    harness.keyboard.modifiers(0, 1 << 4);
    harness.keyboard.key(81, 79, false); // Release-time keysym is now KP_1.
    harness.wait_for_key_release(79, &mut ui);
    assert!(
        !any_key_down.get(),
        "Num Lock transition left a keypad key stuck"
    );
    harness.keyboard.modifiers(0, 0);
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn shift_tab_and_tab_traverse_real_text_fields() {
    let guard = private_input_test_guard();
    let mut harness = KeyboardHarness::new(&guard);
    let mut left = String::from("left");
    let mut right = String::from("right");
    let left_id = egui::Id::new("left-field");
    let right_id = egui::Id::new("right-field");
    let initial_focus = Cell::new(true);
    let focused = Cell::new(None);
    let mut ui = |ui: &mut egui::Ui| {
        ui.add(egui::TextEdit::singleline(&mut left).id(left_id));
        let response = ui.add(egui::TextEdit::singleline(&mut right).id(right_id));
        if initial_focus.get() {
            response.request_focus();
        }
        focused.set(ui.ctx().memory(|memory| memory.focused()));
    };
    harness.wait_for_focus(&mut ui);
    initial_focus.set(false);
    assert_eq!(focused.get(), Some(right_id));
    harness.keyboard.modifiers(1, 0); // Shift
    harness.keyboard.tap(20, 15); // Linux KEY_TAB -> ISO_Left_Tab with Shift
    harness.wait_for_key_release(15, &mut ui);
    assert_eq!(
        focused.get(),
        Some(left_id),
        "Shift+Tab did not focus the previous field"
    );
    harness.keyboard.modifiers(0, 0);
    harness.keyboard.tap(30, 15);
    harness.wait_for_key_release(15, &mut ui);
    assert_eq!(
        focused.get(),
        Some(right_id),
        "Tab did not focus the next field"
    );
}

fn in_private_subprocess(name: &str, test: impl FnOnce()) {
    const CHILD: &str = "WAYAPP_KEYBOARD_TEST_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        test();
        return;
    }
    // Hold the parent's input lock while its child owns compositor focus.
    let _guard = private_input_test_guard();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            name,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, name)
        .spawn()
        .expect("launch private clipboard test process");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                assert!(status.success(), "{name} failed: {status}");
                return;
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{name} exceeded its deadline or could not be observed: {result:?}");
            }
        }
    }
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
#[cfg(feature = "clipboard")]
fn clipboard_copy_cut_paste_preserves_unicode_with_caps_lock_shortcuts() {
    in_private_subprocess(
        "clipboard_copy_cut_paste_preserves_unicode_with_caps_lock_shortcuts",
        || {
            let guard = private_input_test_guard();
            let mut harness = KeyboardHarness::new(&guard);
            let payload = format!("Wayapp clipboard λä {}", std::process::id());
            let text = RefCell::new(payload.clone());
            let mut ui = |ui: &mut egui::Ui| {
                ui.text_edit_singleline(&mut *text.borrow_mut())
                    .request_focus();
            };
            harness.wait_for_focus(&mut ui);
            harness.keyboard.modifiers(1 << 2, 1 << 1); // Control + Caps Lock
            harness.keyboard.tap(1, 30); // Select all
            harness.wait_for_key_release(30, &mut ui);
            harness.keyboard.tap(10, 46); // Copy
            harness.wait_for_key_release(46, &mut ui);
            harness.wait_for_clipboard_selection(&payload);
            harness.keyboard.modifiers(0, 0);
            harness.keyboard.tap(20, 14); // Backspace removes the selected text.
            harness.wait_for_key_release(14, &mut ui);
            assert!(
                text.borrow().is_empty(),
                "text was not cleared before testing paste"
            );
            harness.keyboard.modifiers(1 << 2, 1 << 1);
            harness.keyboard.tap(30, 47); // Paste, with uppercase V keysym.
            harness.wait_for_key_release(47, &mut ui);
            assert_eq!(
                *text.borrow(),
                payload,
                "clipboard paste changed the Unicode payload"
            );
            harness.keyboard.modifiers(0, 0);
            harness.keyboard.tap(40, 46); // Append a character before cutting.
            harness.wait_for_key_release(46, &mut ui);
            let cut_payload = format!("{payload}c");
            assert_eq!(*text.borrow(), cut_payload);
            harness.keyboard.modifiers(1 << 2, 1 << 1);
            harness.keyboard.tap(50, 30); // Select all
            harness.wait_for_key_release(30, &mut ui);
            harness.keyboard.tap(60, 45); // Cut, with uppercase X keysym.
            harness.wait_for_key_release(45, &mut ui);
            assert!(
                text.borrow().is_empty(),
                "cut did not remove the selected text"
            );
            harness.wait_for_clipboard_selection(&cut_payload);
            harness.keyboard.tap(70, 47);
            harness.wait_for_key_release(47, &mut ui);
            assert_eq!(
                *text.borrow(),
                cut_payload,
                "cut/paste changed the Unicode payload"
            );
        },
    );
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
#[cfg(feature = "clipboard")]
fn clipboard_transfers_between_clients_after_keyboard_focus_changes() {
    in_private_subprocess(
        "clipboard_transfers_between_clients_after_keyboard_focus_changes",
        || {
            let guard = private_input_test_guard();
            let mut first = KeyboardHarness::new(&guard);
            let payload = format!("cross-client λä {}", std::process::id());
            let first_text = RefCell::new(payload.clone());
            let mut first_ui = |ui: &mut egui::Ui| {
                ui.text_edit_singleline(&mut *first_text.borrow_mut())
                    .request_focus();
            };
            first.wait_for_focus(&mut first_ui);
            first.keyboard.modifiers(1 << 2, 0);
            first.keyboard.tap(1, 30); // Select all
            first.wait_for_key_release(30, &mut first_ui);
            first.keyboard.tap(10, 46); // Copy
            first.wait_for_key_release(46, &mut first_ui);
            first.wait_for_clipboard_selection(&payload);
            first.keyboard.modifiers(0, 0);

            let mut second = KeyboardHarness::new(&guard);
            let second_text = RefCell::new(String::new());
            let mut second_ui = |ui: &mut egui::Ui| {
                ui.text_edit_singleline(&mut *second_text.borrow_mut())
                    .request_focus();
            };
            second.wait_for_focus(&mut second_ui);
            second.keyboard.modifiers(1 << 2, 0);
            second.keyboard.tap(20, 47); // Paste from the other application's offer.
            second.wait_for_key_release(47, &mut second_ui);
            assert_eq!(*second_text.borrow(), payload);

            // Reverse ownership, then restore keyboard focus to the first client.
            let return_payload = format!("{payload} — second client");
            *second_text.borrow_mut() = return_payload.clone();
            second.keyboard.tap(30, 30);
            second.wait_for_key_release(30, &mut second_ui);
            second.keyboard.tap(40, 46);
            second.wait_for_key_release(46, &mut second_ui);
            second.wait_for_clipboard_selection(&return_payload);
            second.keyboard.modifiers(0, 0);
            second
                .layer
                .set_keyboard_interactivity(KeyboardInteractivity::None);
            second.layer.commit();
            second.app.conn.flush().unwrap();
            first.wait_for_focus(&mut first_ui);
            first.keyboard.modifiers(1 << 2, 0);
            first.keyboard.tap(50, 30);
            first.wait_for_key_release(30, &mut first_ui);
            first.keyboard.tap(60, 47);
            first.wait_for_key_release(47, &mut first_ui);
            assert_eq!(*first_text.borrow(), return_payload);
        },
    );
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn held_native_key_repeats_in_real_text_edit() {
    let guard = private_input_test_guard();
    let mut harness = KeyboardHarness::new(&guard);
    let text = RefCell::new(String::new());
    let mut ui = |ui: &mut egui::Ui| {
        ui.text_edit_singleline(&mut *text.borrow_mut())
            .request_focus();
    };
    harness.wait_for_focus(&mut ui);
    harness.keyboard.key(1, 30, true); // Hold A, without synthetic repeated presses.
    harness.pump_until(&mut ui, |_| text.borrow().len() >= 3);
    harness.keyboard.key(1000, 30, false);
    harness.wait_for_key_release(30, &mut ui);
    assert!(text.borrow().chars().all(|character| character == 'a'));
    let after_release = text.borrow().clone();
    let events = harness.pump_for(Duration::from_millis(180), &mut ui);
    assert_eq!(
        *text.borrow(),
        after_release,
        "text continued repeating after release"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, WaylandEvent::KeyRepeat(_)))
    );
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn external_wakeup_dispatches_already_queued_release_before_due_repetition() {
    in_private_subprocess(
        "external_wakeup_dispatches_already_queued_release_before_due_repetition",
        || {
            let guard = private_input_test_guard();
            let mut harness = KeyboardHarness::new(&guard);
            let text = RefCell::new(String::new());
            let mut ui = |ui: &mut egui::Ui| {
                ui.text_edit_singleline(&mut *text.borrow_mut())
                    .request_focus();
            };
            harness.wait_for_focus(&mut ui);
            harness.keyboard.key(1, 30, true);
            harness.pump_until(&mut ui, |_| text.borrow().len() >= 2);
            harness.keyboard.key(1000, 30, false);
            // Read the compositor's release into Application's event queue,
            // without dispatching its callbacks or consuming its native token.
            harness.app.conn.roundtrip().unwrap();
            std::thread::sleep(Duration::from_millis(250));
            let before = text.borrow().clone();
            // Timer/egui wakeups use the external token. Its ordering relative
            // to the already-signaled native token is controlled by the caller.
            let events = harness.app.dispatch_pending(DispatchToken::default());
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, WaylandEvent::KeyRepeat(_))),
                "a due timer overtook an already-queued native release: {events:?}"
            );
            assert!(
                events.iter().any(
                    |event| matches!(event, WaylandEvent::KeyRelease(key) if key.raw_code == 30)
                )
            );
            harness
                .surface
                .handle_events(&mut harness.app, &events, &mut ui);
            assert_eq!(*text.borrow(), before);
            // A later native token must still acknowledge its reader handoff,
            // even if its callbacks were consumed by the external wakeup.
            harness.keyboard.tap(1100, 48);
            harness.wait_for_key_release(48, &mut ui);
            assert_eq!(*text.borrow(), format!("{before}b"));
        },
    );
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn external_wakeup_dispatches_already_queued_focus_loss_before_due_repetition() {
    in_private_subprocess(
        "external_wakeup_dispatches_already_queued_focus_loss_before_due_repetition",
        || {
            let guard = private_input_test_guard();
            let mut harness = KeyboardHarness::new(&guard);
            let text = RefCell::new(String::new());
            let mut ui = |ui: &mut egui::Ui| {
                ui.text_edit_singleline(&mut *text.borrow_mut())
                    .request_focus();
            };
            harness.wait_for_focus(&mut ui);
            harness.keyboard.key(1, 30, true);
            harness.pump_until(&mut ui, |_| text.borrow().len() >= 2);
            // Mapping a second exclusive client guarantees a real focus
            // transfer; changing this layer's interactivity alone need not.
            let mut other = KeyboardHarness::new(&guard);
            let mut other_ui = |ui: &mut egui::Ui| {
                ui.label("temporary focus owner");
            };
            other.wait_for_focus(&mut other_ui);
            harness.app.conn.roundtrip().unwrap();
            std::thread::sleep(Duration::from_millis(250));
            let before = text.borrow().clone();
            let events = harness.app.dispatch_pending(DispatchToken::default());
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, WaylandEvent::KeyboardLeave(_))),
                "native focus loss was not queued: {events:?}"
            );
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, WaylandEvent::KeyRepeat(_))),
                "due timer overtook queued native focus loss: {events:?}"
            );
            harness
                .surface
                .handle_events(&mut harness.app, &events, &mut ui);
            assert_eq!(*text.borrow(), before);
            harness.keyboard.key(1000, 30, false);
            other
                .layer
                .set_keyboard_interactivity(KeyboardInteractivity::None);
            other.layer.commit();
            other.app.conn.flush().unwrap();
            harness.wait_for_focus(&mut ui);
            harness.keyboard.tap(1100, 48);
            harness.wait_for_key_release(48, &mut ui);
            assert_eq!(*text.borrow(), format!("{before}b"));
        },
    );
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn replacing_held_key_survives_the_previous_repeat_deadline() {
    let guard = private_input_test_guard();
    let mut harness = KeyboardHarness::new(&guard);
    let text = RefCell::new(String::new());
    let mut ui = |ui: &mut egui::Ui| {
        ui.text_edit_singleline(&mut *text.borrow_mut())
            .request_focus();
    };
    harness.wait_for_focus(&mut ui);
    harness.keyboard.key(1, 30, true);
    harness.wait_for_press(30, &mut ui);
    harness.pump_for(Duration::from_millis(100), &mut ui);
    harness.keyboard.key(101, 48, true);
    harness.pump_until(&mut ui, |_| text.borrow().matches('b').count() >= 3);
    harness.keyboard.key(1000, 48, false);
    harness.wait_for_key_release(48, &mut ui);
    harness.keyboard.key(1001, 30, false);
    harness.wait_for_key_release(30, &mut ui);
    assert_eq!(
        text.borrow().chars().filter(|&c| c == 'a').count(),
        1,
        "the previous key kept repeating after replacement"
    );
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn held_key_repeat_stops_when_keyboard_focus_moves_to_another_client() {
    let guard = private_input_test_guard();
    let mut first = KeyboardHarness::new(&guard);
    let first_text = RefCell::new(String::new());
    let mut first_ui = |ui: &mut egui::Ui| {
        ui.text_edit_singleline(&mut *first_text.borrow_mut())
            .request_focus();
    };
    first.wait_for_focus(&mut first_ui);
    first.keyboard.key(1, 30, true);
    first.pump_until(&mut first_ui, |_| first_text.borrow().len() >= 3);
    let mut second = KeyboardHarness::new(&guard);
    let second_text = RefCell::new(String::new());
    let mut second_ui = |ui: &mut egui::Ui| {
        ui.text_edit_singleline(&mut *second_text.borrow_mut())
            .request_focus();
    };
    second.wait_for_focus(&mut second_ui);
    first.pump_until(&mut first_ui, |events| {
        events
            .iter()
            .any(|event| matches!(event, WaylandEvent::KeyboardLeave(_)))
    });
    let after_leave = first_text.borrow().clone();
    let old_events = first.pump_for(Duration::from_millis(180), &mut first_ui);
    let new_events = second.pump_for(Duration::from_millis(180), &mut second_ui);
    assert_eq!(*first_text.borrow(), after_leave);
    assert!(
        second_text.borrow().is_empty(),
        "held text leaked into the newly focused client"
    );
    assert!(
        !old_events
            .iter()
            .chain(&new_events)
            .any(|event| matches!(event, WaylandEvent::KeyRepeat(_)))
    );
    first.keyboard.key(1000, 30, false);
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn held_key_repeat_stops_when_keyboard_capability_is_removed() {
    use smithay_client_toolkit::seat::Capability;
    use smithay_client_toolkit::seat::SeatHandler;
    let guard = private_input_test_guard();
    let mut harness = KeyboardHarness::new(&guard);
    let text = RefCell::new(String::new());
    let mut ui = |ui: &mut egui::Ui| {
        ui.text_edit_singleline(&mut *text.borrow_mut())
            .request_focus();
    };
    harness.wait_for_focus(&mut ui);
    harness.keyboard.key(1, 30, true);
    harness.pump_until(&mut ui, |_| text.borrow().len() >= 3);
    let seat = harness.app.seat_state.seats().next().unwrap();
    let conn = harness.app.conn.clone();
    let qh = harness.app.qh.clone();
    // Exercise the same application callback as a real capability removal.
    harness
        .app
        .remove_capability(&conn, &qh, seat, Capability::Keyboard);
    let after_removal = text.borrow().clone();
    let events = harness.pump_for(Duration::from_millis(180), &mut ui);
    assert_eq!(*text.borrow(), after_removal);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, WaylandEvent::KeyRepeat(_)))
    );
    harness.keyboard.key(1000, 30, false);
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn dropping_application_with_a_held_key_releases_timer_poll_fds() {
    in_private_subprocess(
        "dropping_application_with_a_held_key_releases_timer_poll_fds",
        || {
            fn poll_fds() -> usize {
                std::fs::read_dir("/proc/self/fd")
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        std::fs::read_link(entry.path()).is_ok_and(|path| {
                            path == std::path::Path::new("anon_inode:[eventpoll]")
                        })
                    })
                    .count()
            }
            let guard = private_input_test_guard();
            let exercise = || {
                let mut harness = KeyboardHarness::new(&guard);
                let text = RefCell::new(String::new());
                let mut ui = |ui: &mut egui::Ui| {
                    ui.visuals_mut().text_cursor.blink = false;
                    ui.text_edit_singleline(&mut *text.borrow_mut())
                        .request_focus();
                };
                harness.wait_for_focus(&mut ui);
                harness.keyboard.key(1, 30, true);
                harness.pump_until(&mut ui, |_| text.borrow().len() >= 2);
                // Deliberately drop without a release or a timer-draining dispatch.
                drop(harness);
            };
            exercise(); // Warm up graphics/runtime resources first.
            let baseline = poll_fds();
            for _ in 0..3 {
                exercise();
            }
            assert!(
                poll_fds() <= baseline,
                "dropping a repeating application retained poll fds: baseline {baseline}, now {}",
                poll_fds()
            );
        },
    );
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn held_key_repetition_tracks_modifier_changes_without_repeating_modifier_keys() {
    let guard = private_input_test_guard();
    let mut harness = KeyboardHarness::new(&guard);
    let text = RefCell::new(String::new());
    let mut ui = |ui: &mut egui::Ui| {
        ui.visuals_mut().text_cursor.blink = false;
        ui.text_edit_singleline(&mut *text.borrow_mut())
            .request_focus();
    };
    harness.wait_for_focus(&mut ui);
    harness.keyboard.key(1, 42, true); // Hold left Shift alone.
    harness.keyboard.modifiers(1, 0);
    harness.wait_for_press(42, &mut ui);
    let modifier_events = harness.pump_for(Duration::from_millis(800), &mut ui);
    assert!(text.borrow().is_empty());
    assert!(
        !modifier_events
            .iter()
            .any(|event| matches!(event, WaylandEvent::KeyRepeat(_))),
        "a non-repeating modifier key generated repeats"
    );
    harness.keyboard.key(900, 30, true);
    harness.pump_until(&mut ui, |_| text.borrow().matches('A').count() >= 3);
    harness.keyboard.key(1600, 42, false);
    harness.keyboard.modifiers(0, 0);
    harness.pump_until(&mut ui, |_| text.borrow().matches('a').count() >= 3);
    // Pressing another non-repeatable key must not cancel the held letter.
    harness.keyboard.key(1800, 42, true);
    harness.keyboard.modifiers(1, 0);
    let previous_uppercase = text.borrow().matches('A').count();
    harness.pump_until(&mut ui, |_| {
        text.borrow().matches('A').count() >= previous_uppercase + 2
    });
    harness.keyboard.key(1900, 30, false);
    harness.wait_for_key_release(30, &mut ui);
    harness.keyboard.key(1901, 42, false);
    harness.keyboard.modifiers(0, 0);
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn held_key_repeat_stops_when_its_focused_surface_is_destroyed() {
    use wayland_client::Proxy;
    let guard = private_input_test_guard();
    let mut harness = KeyboardHarness::new(&guard);
    let text = RefCell::new(String::new());
    let mut ui = |ui: &mut egui::Ui| {
        ui.visuals_mut().text_cursor.blink = false;
        ui.text_edit_singleline(&mut *text.borrow_mut())
            .request_focus();
    };
    harness.wait_for_focus(&mut ui);
    harness.keyboard.key(1, 30, true);
    harness.pump_until(&mut ui, |_| text.borrow().len() >= 3);
    let target = harness.layer.wl_surface().clone();
    drop(harness.surface);
    drop(harness.layer);
    assert!(
        !target.is_alive(),
        "test did not actually destroy the focused surface"
    );
    // Deliberately dispatch only an external token first: the compositor need
    // not send a leave for an object the client itself destroyed.
    let deadline = Instant::now() + Duration::from_millis(180);
    let mut repeats = 0;
    loop {
        let events = harness.app.dispatch_pending(DispatchToken::default());
        repeats += events
            .iter()
            .filter(|event| matches!(event, WaylandEvent::KeyRepeat(_)))
            .count();
        match harness
            .rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            // Keep native notifications queued, intentionally exercising
            // local object liveness without relying on a compositor leave.
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
            Err(error) => panic!("keyboard dispatcher stopped unexpectedly: {error}"),
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    assert_eq!(repeats, 0, "a destroyed surface's held key kept repeating");
    harness.keyboard.key(1000, 30, false);
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn international_dead_keys_compose_unicode_in_real_text_edit() {
    let guard = private_input_test_guard();
    let mut harness = KeyboardHarness::new(&guard);
    harness.keyboard.set_keymap("us", "intl", "");
    let text = RefCell::new(String::new());
    let mut ui = |ui: &mut egui::Ui| {
        ui.text_edit_singleline(&mut *text.borrow_mut())
            .request_focus();
    };
    harness.wait_for_focus(&mut ui);
    harness.keyboard.tap(1, 40); // Apostrophe -> dead acute in US International.
    harness.wait_for_key_release(40, &mut ui);
    assert!(
        text.borrow().is_empty(),
        "dead accent produced premature text"
    );
    harness.keyboard.tap(10, 18); // E
    harness.wait_for_key_release(18, &mut ui);
    assert_eq!(*text.borrow(), "é", "dead acute + E did not compose");
    harness.keyboard.tap(20, 40);
    harness.wait_for_key_release(40, &mut ui);
    harness.keyboard.tap(30, 57); // Space after a dead accent yields a literal apostrophe.
    harness.wait_for_key_release(57, &mut ui);
    assert_eq!(
        *text.borrow(),
        "é'",
        "dead accent + Space did not produce a literal accent"
    );
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn unfinished_dead_key_is_cleared_when_keyboard_focus_leaves_and_returns() {
    let guard = private_input_test_guard();
    let mut first = KeyboardHarness::new(&guard);
    first.keyboard.set_keymap("us", "intl", "");
    let first_text = RefCell::new(String::new());
    let mut first_ui = |ui: &mut egui::Ui| {
        ui.text_edit_singleline(&mut *first_text.borrow_mut())
            .request_focus();
    };
    first.wait_for_focus(&mut first_ui);
    first.keyboard.tap(1, 40); // Start, but do not finish, a dead-acute sequence.
    first.wait_for_key_release(40, &mut first_ui);
    let mut second = KeyboardHarness::new(&guard);
    let mut second_ui = |ui: &mut egui::Ui| {
        ui.label("temporary keyboard focus");
    };
    second.wait_for_focus(&mut second_ui);
    first.pump_until(&mut first_ui, |events| {
        events
            .iter()
            .any(|event| matches!(event, WaylandEvent::KeyboardLeave(_)))
    });
    second
        .layer
        .set_keyboard_interactivity(KeyboardInteractivity::None);
    second.layer.commit();
    second.app.conn.flush().unwrap();
    first.wait_for_focus(&mut first_ui);
    first.keyboard.tap(10, 18);
    first.wait_for_key_release(18, &mut first_ui);
    assert_eq!(
        *first_text.borrow(),
        "e",
        "a stale dead key survived keyboard focus loss"
    );
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn compose_sequences_and_escape_cancellation_use_the_configured_compose_key() {
    let guard = private_input_test_guard();
    let mut harness = KeyboardHarness::new(&guard);
    harness.keyboard.set_keymap("us", "", "compose:ralt");
    let text = RefCell::new(String::new());
    let mut ui = |ui: &mut egui::Ui| {
        ui.text_edit_singleline(&mut *text.borrow_mut())
            .request_focus();
    };
    harness.wait_for_focus(&mut ui);
    for (time, key) in [(1, 100), (10, 40), (20, 18)] {
        // Compose, apostrophe, E.
        harness.keyboard.tap(time, key);
        harness.wait_for_key_release(key, &mut ui);
    }
    assert_eq!(*text.borrow(), "é");
    for (time, key) in [(30, 100), (40, 40), (50, 1), (60, 18)] {
        // Escape cancels pending composition.
        harness.keyboard.tap(time, key);
        harness.wait_for_key_release(key, &mut ui);
    }
    assert_eq!(
        *text.borrow(),
        "ée",
        "Escape did not cancel the unfinished sequence"
    );
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn keymap_replacement_discards_pending_composition_and_types_german_altgr_text() {
    let guard = private_input_test_guard();
    let mut harness = KeyboardHarness::new(&guard);
    harness.keyboard.set_keymap("us", "intl", "");
    let text = RefCell::new(String::new());
    let mut ui = |ui: &mut egui::Ui| {
        ui.text_edit_singleline(&mut *text.borrow_mut())
            .request_focus();
    };
    harness.wait_for_focus(&mut ui);
    harness.keyboard.tap(1, 40); // Pending acute in the old keymap.
    harness.wait_for_key_release(40, &mut ui);
    harness.keyboard.set_keymap("de", "", "");
    for (time, modifiers) in [(5, 1 << 2), (7, (1 << 2) | 1)] {
        harness.keyboard.modifiers(modifiers, 0);
        harness.keyboard.tap(time, 39); // Ctrl+ö / Ctrl+Shift+Ö must not type.
        harness.wait_for_key_release(39, &mut ui);
    }
    assert!(
        text.borrow().is_empty(),
        "international Control shortcut inserted text"
    );
    harness.keyboard.modifiers(0, 0);
    for (time, key) in [(10, 18), (20, 39), (30, 40)] {
        // E, Ö, Ä keys.
        harness.keyboard.tap(time, key);
        harness.wait_for_key_release(key, &mut ui);
    }
    harness.keyboard.modifiers(1, 0); // Shift
    harness.keyboard.tap(40, 40);
    harness.wait_for_key_release(40, &mut ui);
    harness.keyboard.modifiers(1 << 7, 0); // Mod5 / AltGr
    for (time, key) in [(50, 16), (60, 18)] {
        // AltGr+Q -> @; AltGr+E -> euro.
        harness.keyboard.tap(time, key);
        harness.wait_for_key_release(key, &mut ui);
    }
    harness.keyboard.modifiers(0, 0);
    assert_eq!(
        *text.borrow(),
        "eöäÄ@€",
        "layout replacement or AltGr changed the typed text"
    );
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn holding_the_last_key_of_a_composed_character_repeats_that_character() {
    let guard = private_input_test_guard();
    let mut harness = KeyboardHarness::new(&guard);
    harness.keyboard.set_keymap("us", "intl", "");
    let text = RefCell::new(String::new());
    let mut ui = |ui: &mut egui::Ui| {
        ui.visuals_mut().text_cursor.blink = false;
        ui.text_edit_singleline(&mut *text.borrow_mut())
            .request_focus();
    };
    harness.wait_for_focus(&mut ui);
    harness.keyboard.tap(1, 40);
    harness.wait_for_key_release(40, &mut ui);
    harness.keyboard.key(10, 18, true);
    harness.pump_until(&mut ui, |_| text.borrow().chars().count() >= 3);
    harness.keyboard.key(1000, 18, false);
    harness.wait_for_key_release(18, &mut ui);
    assert!(
        text.borrow().chars().all(|character| character == 'é'),
        "repeat lost the composed character: {:?}",
        text.borrow()
    );
    harness.keyboard.tap(1010, 18);
    harness.wait_for_key_release(18, &mut ui);
    assert!(
        text.borrow().ends_with('e'),
        "completed compose state leaked into the next press"
    );
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn initialized_application_can_move_threads_and_continue_keyboard_repetition() {
    let guard = private_input_test_guard();
    let mut harness = KeyboardHarness::new(&guard);
    let mut text = String::new();
    harness.wait_for_focus(&mut |ui| {
        ui.text_edit_singleline(&mut text).request_focus();
    });
    let KeyboardHarness {
        surface,
        layer,
        mut app,
        keyboard,
        rx,
        ..
    } = harness;
    // The initialized keymap/Compose state moves with Application. Rendering
    // stays idle on the original thread; only the worker dispatches keyboard data.
    let worker = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut repeated = Vec::new();
        loop {
            let token = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            for event in app.dispatch_pending(token) {
                if let WaylandEvent::KeyRepeat(event) = event {
                    repeated.push(event.utf8);
                }
            }
            if repeated.len() >= 3 {
                return (app, repeated);
            }
        }
    });
    keyboard.key(1, 30, true);
    let (app, repeated) = worker.join().expect("transferred application failed");
    assert!(repeated.iter().all(|text| text.as_deref() == Some("a")));
    keyboard.key(1000, 30, false);
    // Move back and drop in the usual native-resource order.
    drop(surface);
    drop(layer);
    drop(app);
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn unrelated_lock_changes_preserve_a_composed_character_during_repeat() {
    let guard = private_input_test_guard();
    let mut harness = KeyboardHarness::new(&guard);
    harness.keyboard.set_keymap("us", "intl", "");
    let text = RefCell::new(String::new());
    let mut ui = |ui: &mut egui::Ui| {
        ui.visuals_mut().text_cursor.blink = false;
        ui.text_edit_singleline(&mut *text.borrow_mut())
            .request_focus();
    };
    harness.wait_for_focus(&mut ui);
    harness.keyboard.tap(1, 40);
    harness.wait_for_key_release(40, &mut ui);
    harness.keyboard.key(10, 18, true);
    harness.pump_until(&mut ui, |_| text.borrow().chars().count() >= 2);
    let before = text.borrow().chars().count();
    harness.keyboard.modifiers(0, 1 << 4); // Num Lock does not change the E key.
    harness.pump_until(&mut ui, |_| text.borrow().chars().count() >= before + 2);
    harness.keyboard.key(1000, 18, false);
    harness.wait_for_key_release(18, &mut ui);
    assert!(
        text.borrow().chars().all(|character| character == 'é'),
        "an unrelated lock change stripped composed repeat text: {:?}",
        text.borrow()
    );
}

/// Diagnostic, not a responsiveness guarantee: the controlled owner always
/// recovers. Passing records that paste currently waits synchronously for it.
#[test]
#[ignore = "diagnostic; requires private headless runner, virtual keyboard and data-control"]
#[cfg(feature = "clipboard")]
fn report_paste_latency_from_a_delayed_clipboard_owner() {
    in_private_subprocess(
        "report_paste_latency_from_a_delayed_clipboard_owner",
        || {
            let guard = private_input_test_guard();
            let mut harness = KeyboardHarness::new(&guard);
            let text = RefCell::new(String::new());
            let mut ui = |ui: &mut egui::Ui| {
                ui.text_edit_singleline(&mut *text.borrow_mut())
                    .request_focus();
            };
            harness.wait_for_focus(&mut ui);
            harness.keyboard.modifiers(0, 0);
            let payload = format!("controlled clipboard delay λ {}", std::process::id());
            let delay = Duration::from_millis(250);
            let owner = slow_clipboard::SlowClipboardOwner::new(payload.clone(), delay);
            harness.wait_for_clipboard_selection(&payload);
            let before = owner.requests();
            owner.arm();
            harness.keyboard.modifiers(1 << 2, 0);
            let started = Instant::now();
            harness.keyboard.tap(10, 47);
            harness.wait_for_key_release(47, &mut ui);
            let elapsed = started.elapsed();
            assert_eq!(*text.borrow(), payload);
            assert!(
                owner.requests() > before,
                "paste did not request new source data"
            );
            assert!(
                elapsed >= Duration::from_millis(200),
                "synchronous clipboard behavior changed: {elapsed:?}"
            );
            eprintln!(
                "synchronous paste waited {elapsed:?} for a controlled {delay:?} clipboard owner"
            );
        },
    );
}

#[test]
#[ignore = "requires private headless runner, virtual keyboard, xkbcli and WGPU"]
fn changing_shift_while_a_symbol_key_is_held_does_not_leave_the_original_key_down() {
    let guard = private_input_test_guard();
    let mut harness = KeyboardHarness::new(&guard);
    let mut text = String::new();
    let number_down = Cell::new(false);
    let mut ui = |ui: &mut egui::Ui| {
        ui.text_edit_singleline(&mut text).request_focus();
        number_down.set(ui.input(|input| input.key_down(egui::Key::Num1)));
    };
    harness.wait_for_focus(&mut ui);
    harness.keyboard.key(1, 2, true); // KEY_1 without Shift.
    harness.wait_for_press(2, &mut ui);
    assert!(number_down.get());
    harness.keyboard.modifiers(1, 0);
    harness.keyboard.key(10, 2, false); // Release is now reported as exclam.
    harness.wait_for_key_release(2, &mut ui);
    assert!(
        !number_down.get(),
        "Shift changed the release keysym and left Num1 held in egui"
    );
}
