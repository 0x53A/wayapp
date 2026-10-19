//! Run the child in a separate process so a native-library teardown crash
//! becomes an ordinary failed test rather than killing the whole test runner.
#[test]
#[ignore = "requires a Wayland compositor"]
fn application_teardown_keeps_display_alive() {
    in_subprocess("application_teardown_keeps_display_alive", || {
        drop(wayapp::Application::new(|_| {}));
    });
}

#[test]
#[ignore = "requires Wayland, wlr-layer-shell and a WGPU adapter"]
fn surface_can_outlive_application() {
    in_subprocess("surface_can_outlive_application", || {
        let app = wayapp::Application::new(|_| {});
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            smithay_client_toolkit::shell::wlr_layer::Layer::Top,
            Some("wayapp-drop-order-probe"),
            None,
        );
        let surface = wayapp::EguiSurfaceState::new(&app, &layer, 64, 64);
        drop(app);
        drop(surface);
    });
}

fn in_subprocess(name: &str, test: impl FnOnce()) {
    const CHILD: &str = "WAYAPP_TEARDOWN_TEST_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        test();
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--ignored", "--nocapture"])
        .env(CHILD, name)
        .spawn()
        .expect("launch teardown regression subprocess");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                assert!(status.success(), "Application teardown crashed: {status}");
                return;
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            state => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("Application teardown timed out: {state:?}");
            }
        }
    }
}
