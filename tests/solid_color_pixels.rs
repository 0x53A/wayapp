//! Pixel-level verification of solid-color resize and native view replacement.
mod common;
use common::private_input_test_guard;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::Anchor;
use smithay_client_toolkit::shell::wlr_layer::Layer;
use smithay_client_toolkit::shm::slot::SlotPool;
use std::sync::mpsc::channel;
use std::time::Duration;
use std::time::Instant;
use wayapp::Application;
use wayapp::SingleColorState;
use wayapp::WaylandEvent;
use wayland_client::protocol::wl_shm;

fn assert_uniform_region(width: u32, height: u32, color: [u8; 3]) {
    common::require_private_compositor();
    let geometry = format!("0,0 {width}x{height}");
    let capture = std::process::Command::new("grim")
        .args(["-t", "ppm", "-g", &geometry, "-"])
        .output()
        .expect("grim is required in the private headless shell");
    assert!(
        capture.status.success(),
        "private solid-color capture failed: {}",
        String::from_utf8_lossy(&capture.stderr)
    );
    let header = format!("P6\n{width} {height}\n255\n");
    let pixels = capture
        .stdout
        .strip_prefix(header.as_bytes())
        .expect("unexpected grim PPM header");
    assert_eq!(pixels.len(), width as usize * height as usize * 3);
    for (index, pixel) in pixels.chunks_exact(3).enumerate() {
        assert_eq!(
            pixel, color,
            "incorrect color at pixel {index} after resize to {width}x{height}"
        );
    }
}

#[test]
#[ignore = "requires the private headless runner and grim"]
fn solid_color_covers_each_resized_viewport_with_correct_rgb_channels() {
    let _guard = private_input_test_guard();
    let (tx, rx) = channel();
    let mut app = Application::new(move |token| {
        let _ = tx.send(token);
    });
    let layer = app.layer_shell.create_layer_surface(
        &app.qh,
        app.compositor_state.create_surface(&app.qh),
        Layer::Top,
        Some("wayapp-solid-color-pixel-test"),
        None,
    );
    layer.set_anchor(Anchor::TOP | Anchor::LEFT);
    let color = [17, 97, 233];
    let mut state = SingleColorState::new(&layer, (color[0], color[1], color[2]), 64, 64);
    app.run_dispatcher();
    for (width, height) in [(64, 64), (128, 96), (400, 300), (48, 32)] {
        layer.set_size(width, height);
        layer.commit();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let token = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            let events = app.dispatch_pending(token);
            state.handle_events(&app, &events);
            if events.iter().any(|event| matches!(event, WaylandEvent::LayerShellConfigure(target, configure) if target.wl_surface() == layer.wl_surface() && configure.new_size == (width, height))) {
                break;
            }
        }
        app.conn
            .roundtrip()
            .expect("solid-color resize produced a protocol error");
        assert_uniform_region(width, height, color);
    }
    // A view can be replaced while its native role/surface remains owned by
    // the caller. Its old viewport must be destroyed, not merely forgotten.
    // First establish a valid high-DPI buffer/scale pair, like a previous view
    // would leave. Upstream's debounce can retain any earlier source size;
    // cover the largest tested source (400x300) with scale-2 physical pixels.
    let mut previous_pool = SlotPool::new(800 * 600 * 4, &app.shm_state).unwrap();
    let (previous_buffer, canvas) = previous_pool
        .create_buffer(800, 600, 800 * 4, wl_shm::Format::Argb8888)
        .unwrap();
    for pixel in canvas.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[color[2], color[1], color[0], 255]);
    }
    layer.wl_surface().set_buffer_scale(2);
    previous_buffer.attach_to(layer.wl_surface()).unwrap();
    layer.wl_surface().damage_buffer(0, 0, 800, 600);
    layer.commit();
    app.conn
        .roundtrip()
        .expect("high-DPI fixture must be valid before replacement");
    drop(state);
    let replacement_color = [211, 43, 79];
    let mut replacement = SingleColorState::new(
        &layer,
        (
            replacement_color[0],
            replacement_color[1],
            replacement_color[2],
        ),
        72,
        48,
    );
    layer.set_size(72, 48);
    layer.commit();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let token = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        let events = app.dispatch_pending(token);
        replacement.handle_events(&app, &events);
        if events.iter().any(|event| matches!(event, WaylandEvent::LayerShellConfigure(target, configure) if target.wl_surface() == layer.wl_surface() && configure.new_size == (72, 48))) {
            break;
        }
    }
    app.conn
        .roundtrip()
        .expect("replacing the solid-color view disconnected Wayland");
    assert_uniform_region(72, 48, replacement_color);
}
