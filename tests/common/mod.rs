use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;
use std::sync::MutexGuard;

static INPUT_TEST: Mutex<()> = Mutex::new(());

/// Virtual devices share compositor focus, even when Rust runs tests in
/// parallel.
pub fn private_input_test_guard() -> MutexGuard<'static, ()> {
    require_private_compositor();
    INPUT_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Input injection/capture is authorized only in the runner's private
/// compositor.
pub fn require_private_compositor() {
    assert_eq!(
        std::env::var("WAYAPP_HEADLESS_TEST").as_deref(),
        Ok("1"),
        "use bash repros/run-headless.sh; never inject input into a personal compositor"
    );
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap();
    let suffix = runtime
        .strip_prefix("/tmp/wayapp-test-runtime.")
        .expect("use the private headless runtime directory");
    assert!(
        !suffix.is_empty() && !suffix.contains('/'),
        "private runtime must be a direct directory, without traversal"
    );
    let directory = std::fs::symlink_metadata(&runtime).unwrap();
    assert!(directory.is_dir(), "private runtime must not be a symlink");
    assert_eq!(
        directory.permissions().mode() & 0o077,
        0,
        "private runtime must not be accessible to other users"
    );
    assert!(
        std::env::var_os("WAYLAND_SOCKET").is_none(),
        "inherited display socket bypasses the private runtime"
    );
    let display = std::env::var("WAYLAND_DISPLAY").unwrap();
    assert!(
        !display.contains('/'),
        "absolute display paths bypass the private runtime"
    );
    let socket = std::path::Path::new(&runtime).join(display);
    assert!(
        std::fs::symlink_metadata(socket)
            .unwrap()
            .file_type()
            .is_socket()
    );
}
