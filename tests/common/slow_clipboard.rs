//! Test-only clipboard publisher. Privileged data-control is confined to the
//! private compositor and is never used by production clipboard handling.
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use wayland_client::Connection;
use wayland_client::Dispatch;
use wayland_client::QueueHandle;
use wayland_client::delegate_noop;
use wayland_client::event_created_child;
use wayland_client::globals::GlobalListContents;
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_callback::WlCallback;
use wayland_client::protocol::wl_registry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_device_v1::ZwlrDataControlDeviceV1;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_device_v1::{self};
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_manager_v1::ZwlrDataControlManagerV1;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_offer_v1::ZwlrDataControlOfferV1;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_source_v1::ZwlrDataControlSourceV1;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_source_v1::{self};

struct OwnerState {
    payload: String,
    delay: Duration,
    armed: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for OwnerState {
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
delegate_noop!(OwnerState: ignore WlSeat);
delegate_noop!(OwnerState: ignore WlCallback);
delegate_noop!(OwnerState: ignore ZwlrDataControlManagerV1);
delegate_noop!(OwnerState: ignore ZwlrDataControlOfferV1);

impl Dispatch<ZwlrDataControlDeviceV1, ()> for OwnerState {
    event_created_child!(OwnerState, ZwlrDataControlDeviceV1, [0 => (ZwlrDataControlOfferV1, ())]);

    fn event(
        _: &mut Self,
        _: &ZwlrDataControlDeviceV1,
        _: zwlr_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrDataControlSourceV1, ()> for OwnerState {
    fn event(
        state: &mut Self,
        _: &ZwlrDataControlSourceV1,
        event: zwlr_data_control_source_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_source_v1::Event::Send { fd, .. } = event {
            state.requests.fetch_add(1, Ordering::SeqCst);
            if state.armed.load(Ordering::SeqCst) {
                std::thread::sleep(state.delay);
            }
            // Keep writes small and bounded; closing the descriptor is what
            // finally lets smithay-clipboard's synchronous load finish.
            let _ = std::fs::File::from(fd).write_all(state.payload.as_bytes());
        }
    }
}

pub struct SlowClipboardOwner {
    connection: Connection,
    qh: QueueHandle<OwnerState>,
    armed: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SlowClipboardOwner {
    pub fn new(payload: String, delay: Duration) -> Self {
        crate::common::require_private_compositor();
        assert!(payload.len() < 1024);
        assert!(
            delay <= Duration::from_secs(1),
            "test publisher must always recover promptly"
        );
        let armed = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let worker_armed = armed.clone();
        let worker_requests = requests.clone();
        let worker_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            let connection = Connection::connect_to_env().unwrap();
            let (globals, mut queue) = registry_queue_init::<OwnerState>(&connection).unwrap();
            let qh = queue.handle();
            let manager: ZwlrDataControlManagerV1 = globals.bind(&qh, 1..=2, ()).unwrap();
            let seat: WlSeat = globals.bind(&qh, 1..=7, ()).unwrap();
            let device = manager.get_data_device(&seat, &qh, ());
            let source = manager.create_data_source(&qh, ());
            source.offer("text/plain;charset=utf-8".into());
            device.set_selection(Some(&source));
            let mut state = OwnerState {
                payload,
                delay,
                armed: worker_armed,
                requests: worker_requests,
            };
            queue.roundtrip(&mut state).unwrap();
            ready_tx.send((connection.clone(), qh.clone())).unwrap();
            while !worker_stop.load(Ordering::SeqCst) {
                queue.blocking_dispatch(&mut state).unwrap();
            }
            device.set_selection(None);
            source.destroy();
            device.destroy();
            manager.destroy();
            seat.release();
            let _ = connection.flush();
        });
        let (connection, qh) = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        Self {
            connection,
            qh,
            armed,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    pub fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for SlowClipboardOwner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake blocking_dispatch on the owner's connection without reading
        // from another thread or leaving a detached test publisher behind.
        self.connection.display().sync(&self.qh, ());
        let _ = self.connection.flush();
        let result = self.thread.take().unwrap().join();
        if !std::thread::panicking() {
            result.expect("private clipboard publisher failed during cleanup");
        }
    }
}
