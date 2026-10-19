//! Actual cursor image regression, confined to repros/run-headless.sh.
//! No input is injected and no capture is taken outside that private
//! compositor.
mod common;
use common::private_input_test_guard;
use smithay_client_toolkit::seat::pointer::PointerEventKind;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::Anchor;
use smithay_client_toolkit::shell::wlr_layer::Layer;
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
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

struct PointerState;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for PointerState {
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
delegate_noop!(PointerState: ZwlrVirtualPointerManagerV1);
delegate_noop!(PointerState: ZwlrVirtualPointerV1);

struct VirtualPointer {
    pointer: ZwlrVirtualPointerV1,
    manager: ZwlrVirtualPointerManagerV1,
    _queue: EventQueue<PointerState>,
    connection: Connection,
}

impl VirtualPointer {
    fn new() -> Self {
        common::require_private_compositor();
        let connection = Connection::connect_to_env().unwrap();
        let (globals, queue) = registry_queue_init::<PointerState>(&connection).unwrap();
        let qh = queue.handle();
        let manager: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).unwrap();
        let pointer = manager.create_virtual_pointer(None, &qh, ());
        connection.roundtrip().unwrap();
        Self {
            pointer,
            manager,
            _queue: queue,
            connection,
        }
    }

    fn move_to_center(&self, time: u32) {
        self.move_to(time, 400, 300);
    }

    fn move_to(&self, time: u32, x: u32, y: u32) {
        // A small excursion guarantees motion even if already at the center.
        self.pointer
            .motion_absolute(time, x.saturating_sub(1), y, 800, 600);
        self.pointer.frame();
        self.pointer.motion_absolute(time + 1, x, y, 800, 600);
        self.pointer.frame();
        self.connection.roundtrip().unwrap();
    }

    fn click(&self, time: u32) {
        use wayland_client::protocol::wl_pointer::ButtonState;
        self.pointer.button(time, 0x110, ButtonState::Pressed);
        self.pointer.frame();
        self.pointer.button(time + 1, 0x110, ButtonState::Released);
        self.pointer.frame();
        self.connection.roundtrip().unwrap();
    }
}

impl Drop for VirtualPointer {
    fn drop(&mut self) {
        self.pointer.destroy();
        self.manager.destroy();
        let _ = self.connection.flush();
    }
}

#[test]
#[ignore = "requires private headless runner, virtual pointer and WGPU"]
fn recreated_view_retains_stationary_pointer_hover_and_click_position() {
    check_stationary_pointer_view_change(StationaryViewChange::Replace);
}

#[test]
#[ignore = "requires private headless runner, virtual pointer and WGPU"]
fn settled_zoom_retains_stationary_pointer_hover_and_click_position() {
    check_stationary_pointer_view_change(StationaryViewChange::Zoom);
}

#[test]
#[ignore = "requires private headless runner, virtual pointer and WGPU"]
fn recreated_view_does_not_replay_a_held_button_or_click_on_its_release() {
    check_stationary_pointer_view_change(StationaryViewChange::ReplaceWhilePressed);
}

#[derive(Clone, Copy)]
enum StationaryViewChange {
    Replace,
    Zoom,
    ReplaceWhilePressed,
}

fn check_stationary_pointer_view_change(change: StationaryViewChange) {
    use std::cell::Cell;
    use wayland_client::protocol::wl_pointer::ButtonState;
    let replace_view = !matches!(change, StationaryViewChange::Zoom);
    let button_held = matches!(change, StationaryViewChange::ReplaceWhilePressed);
    let _guard = private_input_test_guard();
    let pointer = VirtualPointer::new();
    let (tx, rx) = channel();
    let mut app = Application::new(move |token| {
        let _ = tx.send(token);
    });
    let layer = app.layer_shell.create_layer_surface(
        &app.qh,
        app.compositor_state.create_surface(&app.qh),
        Layer::Top,
        Some("wayapp-pointer-replacement-test"),
        None,
    );
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_size(0, 0);
    layer.commit();
    let mut surface = EguiSurfaceState::new(&app, &layer, 800, 600);
    let position = Cell::new(None);
    let clicked = Cell::new(false);
    let down = Cell::new(false);
    let frames = Cell::new(0);
    let requested_zoom = Cell::new(1.0);
    let mut ui = |ui: &mut egui::Ui| {
        if ui.ctx().zoom_factor() != requested_zoom.get() {
            ui.ctx().set_zoom_factor(requested_zoom.get());
        }
        frames.set(frames.get() + 1);
        position.set(ui.input(|input| input.pointer.hover_pos()));
        down.set(ui.input(|input| input.pointer.primary_down()));
        let rect = egui::Rect::from_center_size(
            egui::pos2(400.0, 300.0) / ui.ctx().zoom_factor(),
            egui::vec2(120.0, 80.0),
        );
        if ui
            .put(rect, egui::Button::new("replacement probe"))
            .clicked()
        {
            clicked.set(true);
        }
    };
    app.run_dispatcher();
    dispatch_until(
        &mut app,
        &rx,
        |app, events| surface.handle_events(app, events, &mut ui),
        |events| {
            events
                .iter()
                .any(|event| matches!(event, WaylandEvent::LayerShellConfigure(..)))
        },
    );
    pointer.move_to_center(1);
    dispatch_until(
        &mut app,
        &rx,
        |app, events| surface.handle_events(app, events, &mut ui),
        pointer_reached_center,
    );
    assert_eq!(position.get(), Some(egui::pos2(400.0, 300.0)));
    if button_held {
        pointer.pointer.button(10, 0x110, ButtonState::Pressed);
        pointer.pointer.frame();
        pointer.connection.roundtrip().unwrap();
        dispatch_until(
            &mut app,
            &rx,
            |app, events| surface.handle_events(app, events, &mut ui),
            |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        WaylandEvent::PointerEvent((
                            _,
                            _,
                            PointerEventKind::Press { button: 0x110, .. }
                        ))
                    )
                })
            },
        );
        assert!(down.get(), "fixture did not hold the old view's button");
        assert!(!clicked.get());
    }
    if replace_view {
        drop(surface);
        surface = EguiSurfaceState::new(&app, &layer, 800, 600);
    } else {
        requested_zoom.set(2.0);
    }
    position.set(None);
    let previous_frames = frames.get();
    let unexpected_motion = Cell::new(false);
    surface.request_frame();
    dispatch_until(
        &mut app,
        &rx,
        |app, events| {
            unexpected_motion.set(
                unexpected_motion.get()
                    || events.iter().any(|event| {
                        matches!(
                            event,
                            WaylandEvent::PointerEvent((
                                _,
                                _,
                                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. }
                            ))
                        )
                    }),
            );
            surface.handle_events(app, events, &mut ui);
        },
        |_| frames.get() >= previous_frames + if replace_view { 1 } else { 3 },
    );
    assert!(
        !unexpected_motion.get(),
        "view change must not rely on fresh native motion"
    );
    assert_eq!(
        position.get(),
        Some(egui::pos2(400.0, 300.0) / requested_zoom.get()),
        "stationary hover was lost"
    );
    if button_held {
        assert!(
            !down.get(),
            "replacement replayed an existing physical button press"
        );
        pointer.pointer.button(20, 0x110, ButtonState::Released);
        pointer.pointer.frame();
        pointer.connection.roundtrip().unwrap();
        dispatch_until(
            &mut app,
            &rx,
            |app, events| surface.handle_events(app, events, &mut ui),
            |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        WaylandEvent::PointerEvent((
                            _,
                            _,
                            PointerEventKind::Release { button: 0x110, .. }
                        ))
                    )
                })
            },
        );
        assert!(!clicked.get(), "old view's release clicked a new widget");
        assert!(!down.get());
    }
    pointer.click(30);
    dispatch_until(
        &mut app,
        &rx,
        |app, events| surface.handle_events(app, events, &mut ui),
        |_| clicked.get(),
    );
    assert!(
        clicked.get(),
        "stationary click missed the replacement widget"
    );
}

fn pump_until(
    app: &mut Application,
    surface: &mut EguiSurfaceState<&smithay_client_toolkit::shell::wlr_layer::LayerSurface>,
    rx: &Receiver<DispatchToken>,
    cursor: egui::CursorIcon,
    ready: impl FnMut(&[WaylandEvent]) -> bool,
) {
    dispatch_until(
        app,
        rx,
        |app, events| {
            surface.handle_events(app, events, &mut |ui| cursor_ui(ui, cursor));
        },
        ready,
    );
}

fn cursor_ui(ui: &mut egui::Ui, cursor: egui::CursorIcon) {
    ui.ctx().set_cursor_icon(cursor);
    ui.painter()
        .rect_filled(ui.max_rect(), 0.0, egui::Color32::BLACK);
}

fn dispatch_until(
    app: &mut Application,
    rx: &Receiver<DispatchToken>,
    mut handle: impl FnMut(&mut Application, &[WaylandEvent]),
    mut ready: impl FnMut(&[WaylandEvent]) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let token = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("timed out waiting for private compositor events");
        let events = app.dispatch_pending(token);
        handle(app, &events);
        if ready(&events) {
            app.conn
                .roundtrip()
                .expect("cursor request disconnected Wayland");
            return;
        }
    }
}

fn pointer_reached_center(events: &[WaylandEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            WaylandEvent::PointerEvent((
                _,
                (400.0, 300.0),
                PointerEventKind::Motion { .. } | PointerEventKind::Enter { .. }
            ))
        )
    })
}

fn cursor_crop() -> Vec<u8> {
    cursor_crop_at(400, 300)
}

fn cursor_crop_at(x: u32, y: u32) -> Vec<u8> {
    common::require_private_compositor();
    let geometry = format!("{},{} 40x40", x - 4, y - 4);
    let output = std::process::Command::new("grim")
        .args(["-c", "-t", "ppm", "-g", &geometry, "-"])
        .output()
        .expect("grim is required; use repros/run-headless.sh");
    assert!(
        output.status.success(),
        "private cursor capture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.starts_with(b"P6\n"), "expected a PPM capture");
    output.stdout
}

#[test]
#[ignore = "requires the private headless runner, virtual-pointer protocol and grim"]
fn egui_cursor_none_hides_and_default_restores_pointer_image() {
    let _guard = private_input_test_guard();

    let pointer = VirtualPointer::new();
    let (tx, rx) = channel();
    let mut app = Application::new(move |token| {
        let _ = tx.send(token);
    });
    let layer = app.layer_shell.create_layer_surface(
        &app.qh,
        app.compositor_state.create_surface(&app.qh),
        Layer::Top,
        Some("wayapp-cursor-visibility-test"),
        None,
    );
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_size(0, 0);
    layer.commit();
    let mut surface = EguiSurfaceState::new(&app, &layer, 800, 600);
    app.run_dispatcher();
    pump_until(
        &mut app,
        &mut surface,
        &rx,
        egui::CursorIcon::Default,
        |events| {
            events
                .iter()
                .any(|event| matches!(event, WaylandEvent::LayerShellConfigure(..)))
        },
    );

    pointer.move_to_center(1);
    pump_until(
        &mut app,
        &mut surface,
        &rx,
        egui::CursorIcon::Default,
        pointer_reached_center,
    );
    let visible = cursor_crop();
    // A different surface may process global events or repaint without owning
    // pointer focus. Its cursor output must not affect the focused surface.
    let background_layer = app.layer_shell.create_layer_surface(
        &app.qh,
        app.compositor_state.create_surface(&app.qh),
        Layer::Bottom,
        Some("wayapp-unfocused-cursor-test"),
        None,
    );
    let mut background = EguiSurfaceState::new(&app, &background_layer, 64, 64);
    background.handle_events(
        &mut app,
        &[WaylandEvent::ModifiersChanged(Default::default())],
        &mut |ui| {
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
        },
    );
    app.conn.roundtrip().unwrap();
    assert!(
        cursor_crop() == visible,
        "unfocused surface changed the active pointer image"
    );
    // Cursor changes can come from a timer/background operation while the
    // pointer stays still. They must apply on frame events, not only motion.
    surface.request_frame();
    pump_until(
        &mut app,
        &mut surface,
        &rx,
        egui::CursorIcon::None,
        |events| {
            events.iter().any(|event| matches!(event, WaylandEvent::Frame(surface, _) if surface == layer.wl_surface()))
        },
    );
    let hidden = cursor_crop();
    assert!(
        visible != hidden,
        "CursorIcon::None did not hide the visible pointer image"
    );
    surface.request_frame();
    pump_until(
        &mut app,
        &mut surface,
        &rx,
        egui::CursorIcon::Default,
        |events| {
            events.iter().any(|event| matches!(event, WaylandEvent::Frame(surface, _) if surface == layer.wl_surface()))
        },
    );
    let restored = cursor_crop();
    assert!(
        visible == restored,
        "default pointer image was not restored after hiding"
    );
    // Exercise real protocol-object cleanup/rebinding while the private
    // virtual device remains present (not a physical hardware unplug test).
    use smithay_client_toolkit::seat::Capability;
    use smithay_client_toolkit::seat::SeatHandler;
    let seat = app.seat_state.seats().next().unwrap();
    let conn = app.conn.clone();
    let qh = app.qh.clone();
    app.remove_capability(&conn, &qh, seat.clone(), Capability::Pointer);
    let cleanup = app.take_wayland_events();
    assert!(cleanup.iter().any(|event|
        matches!(event, WaylandEvent::PointerRemoved(target) if target == layer.wl_surface())));
    surface.handle_events(&mut app, &cleanup, &mut |ui| {
        cursor_ui(ui, egui::CursorIcon::Default)
    });
    app.conn
        .roundtrip()
        .expect("pointer/cursor object cleanup disconnected Wayland");
    app.new_capability(&conn, &qh, seat, Capability::Pointer);
    pointer.move_to_center(100);
    pump_until(
        &mut app,
        &mut surface,
        &rx,
        egui::CursorIcon::Default,
        pointer_reached_center,
    );
    assert!(
        cursor_crop() == visible,
        "cursor did not recover after pointer rebind"
    );
}

#[test]
#[ignore = "requires the private headless runner, virtual-pointer protocol and grim"]
fn cursor_follows_focus_between_mapped_surfaces_and_restores_on_reentry() {
    let _guard = private_input_test_guard();
    let pointer = VirtualPointer::new();
    let (tx, rx) = channel();
    let mut app = Application::new(move |token| {
        let _ = tx.send(token);
    });
    let left = app.layer_shell.create_layer_surface(
        &app.qh,
        app.compositor_state.create_surface(&app.qh),
        Layer::Top,
        Some("wayapp-left-cursor-test"),
        None,
    );
    left.set_anchor(Anchor::LEFT | Anchor::TOP | Anchor::BOTTOM);
    left.set_size(400, 0);
    left.commit();
    let right = app.layer_shell.create_layer_surface(
        &app.qh,
        app.compositor_state.create_surface(&app.qh),
        Layer::Top,
        Some("wayapp-right-cursor-test"),
        None,
    );
    right.set_anchor(Anchor::RIGHT | Anchor::TOP | Anchor::BOTTOM);
    right.set_size(400, 0);
    right.commit();
    let mut left_view = EguiSurfaceState::new(&app, &left, 400, 600);
    let mut right_view = EguiSurfaceState::new(&app, &right, 400, 600);
    app.run_dispatcher();
    let (mut configured_left, mut configured_right) = (false, false);
    dispatch_until(
        &mut app,
        &rx,
        |app, events| {
            left_view.handle_events(app, events, &mut |ui| {
                cursor_ui(ui, egui::CursorIcon::Default)
            });
            right_view.handle_events(app, events, &mut |ui| cursor_ui(ui, egui::CursorIcon::Text));
        },
        |events| {
            for event in events {
                if let WaylandEvent::LayerShellConfigure(surface, _) = event {
                    configured_left |= surface.wl_surface() == left.wl_surface();
                    configured_right |= surface.wl_surface() == right.wl_surface();
                }
            }
            configured_left && configured_right
        },
    );
    let reached =
        |events: &[WaylandEvent],
         target: &smithay_client_toolkit::shell::wlr_layer::LayerSurface| {
            events.iter().any(|event| matches!(event,
            WaylandEvent::PointerEvent((surface, (200.0, 300.0), PointerEventKind::Motion { .. } | PointerEventKind::Enter { .. })) if surface == target.wl_surface()))
        };
    pointer.move_to(1, 200, 300);
    dispatch_until(
        &mut app,
        &rx,
        |app, events| {
            left_view.handle_events(app, events, &mut |ui| {
                cursor_ui(ui, egui::CursorIcon::Default)
            });
            right_view.handle_events(app, events, &mut |ui| cursor_ui(ui, egui::CursorIcon::Text));
        },
        |events| reached(events, &left),
    );
    let arrow = cursor_crop_at(200, 300);

    pointer.move_to(10, 600, 300);
    dispatch_until(
        &mut app,
        &rx,
        |app, events| {
            // Process the unfocused surface last to catch accidental cursor overrides.
            right_view.handle_events(app, events, &mut |ui| cursor_ui(ui, egui::CursorIcon::Text));
            left_view.handle_events(app, events, &mut |ui| {
                cursor_ui(ui, egui::CursorIcon::Default)
            });
        },
        |events| reached(events, &right),
    );
    let text = cursor_crop_at(600, 300);
    assert!(
        text != arrow,
        "entering the text surface did not change cursor image"
    );

    left_view.request_frame();
    dispatch_until(
        &mut app,
        &rx,
        |app, events| {
            right_view.handle_events(app, events, &mut |ui| cursor_ui(ui, egui::CursorIcon::Text));
            left_view.handle_events(app, events, &mut |ui| cursor_ui(ui, egui::CursorIcon::None));
        },
        |events| {
            events.iter().any(|event| matches!(event, WaylandEvent::Frame(surface, _) if surface == left.wl_surface()))
        },
    );
    assert!(
        cursor_crop_at(600, 300) == text,
        "unfocused mapped surface hid another surface's cursor"
    );

    pointer.move_to(20, 200, 300);
    dispatch_until(
        &mut app,
        &rx,
        |app, events| {
            left_view.handle_events(app, events, &mut |ui| {
                cursor_ui(ui, egui::CursorIcon::Default)
            });
            right_view.handle_events(app, events, &mut |ui| cursor_ui(ui, egui::CursorIcon::Text));
        },
        |events| reached(events, &left),
    );
    assert!(
        cursor_crop_at(200, 300) == arrow,
        "cursor image was not restored on surface reentry"
    );
}

#[test]
#[ignore = "requires the private headless runner, virtual-pointer protocol and grim"]
fn zoomed_button_pixels_and_pointer_hit_testing_agree() {
    use std::cell::Cell;
    let _guard = private_input_test_guard();
    let pointer = VirtualPointer::new();
    let (tx, rx) = channel();
    let mut app = Application::new(move |token| {
        let _ = tx.send(token);
    });
    let layer = app.layer_shell.create_layer_surface(
        &app.qh,
        app.compositor_state.create_surface(&app.qh),
        Layer::Top,
        Some("wayapp-zoom-click-test"),
        None,
    );
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_size(0, 0);
    layer.commit();
    let mut surface = EguiSurfaceState::new(&app, &layer, 800, 600);
    let requested_zoom = Cell::new(false);
    let scale = Cell::new(1.0);
    let clicks = Cell::new(0);
    let mut ui = |ui: &mut egui::Ui| {
        if !requested_zoom.replace(true) {
            ui.ctx().set_zoom_factor(2.0);
        }
        scale.set(ui.ctx().pixels_per_point());
        ui.painter()
            .rect_filled(ui.max_rect(), 0.0, egui::Color32::BLACK);
        let rect = egui::Rect::from_min_max(egui::pos2(20.0, 20.0), egui::pos2(60.0, 40.0));
        if ui
            .put(
                rect,
                egui::Button::new("").fill(egui::Color32::from_rgb(0, 200, 0)),
            )
            .clicked()
        {
            clicks.set(clicks.get() + 1);
        }
    };
    app.run_dispatcher();
    dispatch_until(
        &mut app,
        &rx,
        |app, events| surface.handle_events(app, events, &mut ui),
        |events| {
            scale.get() == 2.0 && events.iter().any(|event| matches!(event, WaylandEvent::Frame(target, _) if target == layer.wl_surface()))
        },
    );
    pointer.move_to(10, 80, 60); // Center of the button at 2x zoom.
    dispatch_until(
        &mut app,
        &rx,
        |app, events| surface.handle_events(app, events, &mut ui),
        |events| {
            events.iter().any(|event| matches!(event, WaylandEvent::PointerEvent((target, (80.0, 60.0), PointerEventKind::Motion { .. } | PointerEventKind::Enter { .. })) if target == layer.wl_surface()))
        },
    );
    let image = std::process::Command::new("grim")
        .args(["-t", "ppm", "-g", "80,60 1x1", "-"])
        .output()
        .unwrap();
    assert!(image.status.success());
    let pixel = image
        .stdout
        .strip_prefix(b"P6\n1 1\n255\n")
        .expect("unexpected PPM pixel header");
    assert!(
        pixel != [0, 0, 0],
        "button pixels were not rendered at their zoomed position"
    );
    pointer.click(20);
    dispatch_until(
        &mut app,
        &rx,
        |app, events| surface.handle_events(app, events, &mut ui),
        |events| {
            events.iter().any(|event| matches!(event, WaylandEvent::PointerEvent((target, _, PointerEventKind::Release { .. })) if target == layer.wl_surface()))
        },
    );
    assert_eq!(
        clicks.get(),
        1,
        "click did not hit the visibly rendered zoomed button exactly once"
    );
}

#[test]
#[ignore = "requires the private headless runner and virtual-pointer protocol"]
fn native_wheel_and_touchpad_events_scroll_real_content_on_both_axes() {
    use std::cell::Cell;
    use wayland_client::protocol::wl_pointer::Axis;
    use wayland_client::protocol::wl_pointer::AxisSource;
    let _guard = private_input_test_guard();
    let pointer = VirtualPointer::new();
    let (tx, rx) = channel();
    let mut app = Application::new(move |token| {
        let _ = tx.send(token);
    });
    let layer = app.layer_shell.create_layer_surface(
        &app.qh,
        app.compositor_state.create_surface(&app.qh),
        Layer::Top,
        Some("wayapp-native-scroll-test"),
        None,
    );
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_size(0, 0);
    layer.commit();
    let mut surface = EguiSurfaceState::new(&app, &layer, 800, 600);
    let offset = Cell::new(egui::Vec2::ZERO);
    let mut ui = |ui: &mut egui::Ui| {
        ui.ctx()
            .options_mut(|options| options.input_options.line_scroll_speed = 40.0);
        let result = egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            ui.allocate_space(egui::vec2(2000.0, 2000.0));
        });
        offset.set(result.state.offset);
    };
    app.run_dispatcher();
    dispatch_until(
        &mut app,
        &rx,
        |app, events| surface.handle_events(app, events, &mut ui),
        |events| {
            events
                .iter()
                .any(|event| matches!(event, WaylandEvent::LayerShellConfigure(..)))
        },
    );
    pointer.move_to_center(1);
    dispatch_until(
        &mut app,
        &rx,
        |app, events| surface.handle_events(app, events, &mut ui),
        pointer_reached_center,
    );
    assert_eq!(offset.get(), egui::Vec2::ZERO);
    pointer.pointer.axis_source(AxisSource::Wheel);
    pointer
        .pointer
        .axis_discrete(10, Axis::VerticalScroll, 15.0, 1);
    pointer
        .pointer
        .axis_discrete(10, Axis::HorizontalScroll, 15.0, 1);
    pointer.pointer.frame();
    pointer.connection.roundtrip().unwrap();
    dispatch_until(
        &mut app,
        &rx,
        |app, events| surface.handle_events(app, events, &mut ui),
        |_| offset.get().x >= 39.99 && offset.get().y >= 39.99,
    );
    let wheel_offset = offset.get();
    assert!(
        (wheel_offset - egui::vec2(40.0, 40.0)).length() < 0.1,
        "one wheel step did not map to one egui line: {wheel_offset:?}"
    );
    pointer.pointer.axis_source(AxisSource::Finger);
    pointer.pointer.axis(20, Axis::VerticalScroll, 25.0);
    pointer.pointer.axis(20, Axis::HorizontalScroll, 25.0);
    pointer.pointer.frame();
    pointer.pointer.axis_source(AxisSource::Finger);
    pointer.pointer.axis_stop(21, Axis::VerticalScroll);
    pointer.pointer.axis_stop(21, Axis::HorizontalScroll);
    pointer.pointer.frame();
    pointer.connection.roundtrip().unwrap();
    let stopped = Cell::new(false);
    dispatch_until(
        &mut app,
        &rx,
        |app, events| surface.handle_events(app, events, &mut ui),
        |events| {
            if events.iter().any(|event| matches!(event,
                WaylandEvent::PointerEvent((_, _, PointerEventKind::Axis { vertical, horizontal, .. }))
                if vertical.stop && horizontal.stop)) {
                stopped.set(true);
            }
            // egui smooths scrolling over subsequent repaint frames; seeing
            // the native stop event is not enough to observe the final offset.
            stopped.get()
                && offset.get().x >= wheel_offset.x + 24.9
                && offset.get().y >= wheel_offset.y + 24.9
        },
    );
    assert!(
        offset.get().x >= wheel_offset.x + 24.9 && offset.get().y >= wheel_offset.y + 24.9,
        "touchpad movement was lost or reversed: before {wheel_offset:?}, after {:?}",
        offset.get()
    );
    let forward_offset = offset.get();
    assert!(
        (forward_offset - egui::vec2(65.0, 65.0)).length() < 0.2,
        "touchpad pixels did not retain their distance: {forward_offset:?}"
    );
    pointer.pointer.axis_source(AxisSource::Finger);
    pointer.pointer.axis(30, Axis::VerticalScroll, -5.0);
    pointer.pointer.axis(30, Axis::HorizontalScroll, -5.0);
    pointer.pointer.frame();
    pointer.connection.roundtrip().unwrap();
    dispatch_until(
        &mut app,
        &rx,
        |app, events| surface.handle_events(app, events, &mut ui),
        |_| offset.get().x < forward_offset.x - 4.9 && offset.get().y < forward_offset.y - 4.9,
    );
    assert!(
        (offset.get() - egui::vec2(60.0, 60.0)).length() < 0.2,
        "reversed touchpad pixels did not move content back: {:?}",
        offset.get()
    );
}
