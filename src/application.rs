// use crate::LayerSurfaceContainer;
// use crate::PopupContainer;
// use crate::SubsurfaceContainer;
// use crate::WindowContainer;
use crate::keyboard_repeat::KeyboardRepeat;
use log::trace;
use smithay_client_toolkit::compositor::CompositorHandler;
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::delegate_registry;
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::output::OutputHandler;
use smithay_client_toolkit::output::OutputInfo;
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::registry::ProvidesRegistryState;
use smithay_client_toolkit::registry::RegistryState;
use smithay_client_toolkit::registry::SimpleGlobal;
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::seat::Capability;
use smithay_client_toolkit::seat::SeatHandler;
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::seat::keyboard::KeyEvent;
use smithay_client_toolkit::seat::keyboard::KeyboardHandler;
use smithay_client_toolkit::seat::keyboard::Keymap;
use smithay_client_toolkit::seat::keyboard::Keysym;
use smithay_client_toolkit::seat::keyboard::Modifiers as KeyboardModifiers;
use smithay_client_toolkit::seat::keyboard::RepeatInfo;
use smithay_client_toolkit::seat::pointer::PointerEvent;
use smithay_client_toolkit::seat::pointer::PointerEventKind;
use smithay_client_toolkit::seat::pointer::PointerHandler;
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::LayerShell;
use smithay_client_toolkit::shell::wlr_layer::LayerShellHandler;
use smithay_client_toolkit::shell::wlr_layer::LayerSurface;
use smithay_client_toolkit::shell::wlr_layer::LayerSurfaceConfigure;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::popup::Popup;
use smithay_client_toolkit::shell::xdg::popup::PopupConfigure;
use smithay_client_toolkit::shell::xdg::popup::PopupHandler;
use smithay_client_toolkit::shell::xdg::window::Window;
use smithay_client_toolkit::shell::xdg::window::WindowConfigure;
use smithay_client_toolkit::shell::xdg::window::WindowHandler;
use smithay_client_toolkit::shm::Shm;
use smithay_client_toolkit::shm::ShmHandler;
use smithay_client_toolkit::subcompositor::SubcompositorState;
#[cfg(feature = "clipboard")]
use smithay_clipboard::Clipboard;
use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use wayland_backend::client::ObjectId;
use wayland_backend::client::WaylandError;
use wayland_client::Connection;
use wayland_client::Dispatch;
use wayland_client::EventQueue;
use wayland_client::Proxy;
use wayland_client::QueueHandle;
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_keyboard::WlKeyboard;
use wayland_client::protocol::wl_output;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_seat;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::WpCursorShapeDeviceV1;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::ZwpTextInputV3;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewport::{self};
use wayland_protocols::wp::viewporter::client::wp_viewporter;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

/// Enum representing different Wayland events
///
/// This is not same as smithay_client_toolkit events, this is an
/// application-level event enum.
#[derive(Debug, Clone)]
pub enum WaylandEvent {
    /// WlSurface and the timestamp
    Frame(WlSurface, u32),
    ScaleFactorChanged(WlSurface, i32),
    TransformChanged(WlSurface),
    SurfaceEnteredOutput(WlSurface, WlOutput),
    SurfaceLeftOutput(WlSurface, WlOutput),
    LayerShellClosed(LayerSurface),
    LayerShellConfigure(LayerSurface, LayerSurfaceConfigure),
    PopupConfigure(Popup, PopupConfigure),
    PopupDone(Popup),
    WindowRequestClose(Window),
    WindowConfigure(Window, WindowConfigure),
    OutputCreated(WlOutput),
    OutputUpdated(WlOutput),
    OutputDestroyed(WlOutput),
    KeyboardEnter(WlSurface, Vec<u32>, Vec<Keysym>),
    KeyboardLeave(WlSurface),
    KeyPress(KeyEvent),
    KeyRelease(KeyEvent),
    KeyRepeat(KeyEvent),
    PointerEvent((WlSurface, (f64, f64), PointerEventKind)),
    /// A pointer device disappeared while affecting this surface. Cancel
    /// interactions without synthesizing a button release (and accidental
    /// click).
    PointerRemoved(WlSurface),
    ModifiersChanged(smithay_client_toolkit::seat::keyboard::Modifiers),
    ImeEnter(WlSurface),
    ImeLeave(WlSurface),
    /// The text to commit to the input field. `None` clears any pending
    /// preedit.
    ImeCommitString(Option<String>),
    /// Preedit (in-progress composition) text, cursor byte offset from the
    /// start of the preedit string to the beginning and end of the cursor
    /// selection (`cursor_begin`, `cursor_end`). Both are `-1` when there
    /// is no cursor.
    ImePreeditString(Option<String>, i32, i32),
    /// Number of bytes to delete before (`before_length`) and after
    /// (`after_length`) the current cursor position.
    ImeDeleteSurroundingText(u32, u32),
    /// Signals that a complete set of IME events has been sent for this serial.
    ImeDone(u32),
}

impl WaylandEvent {
    pub fn get_wl_surface(&self) -> Option<&WlSurface> {
        match self {
            WaylandEvent::Frame(s, _) => Some(s),
            WaylandEvent::ScaleFactorChanged(s, _) => Some(s),
            WaylandEvent::TransformChanged(s) => Some(s),
            WaylandEvent::SurfaceEnteredOutput(s, _) => Some(s),
            WaylandEvent::SurfaceLeftOutput(s, _) => Some(s),
            WaylandEvent::WindowConfigure(w, _) => Some(&w.wl_surface()),
            WaylandEvent::LayerShellConfigure(layer, _) => Some(&layer.wl_surface()),
            WaylandEvent::LayerShellClosed(layer) => Some(&layer.wl_surface()),
            WaylandEvent::PopupConfigure(popup, _) => Some(&popup.wl_surface()),
            WaylandEvent::PopupDone(popup) => Some(&popup.wl_surface()),
            WaylandEvent::KeyboardEnter(s, _, _) => Some(s),
            WaylandEvent::KeyboardLeave(s) => Some(s),
            WaylandEvent::PointerEvent((s, _, _)) => Some(s),
            WaylandEvent::PointerRemoved(s) => Some(s),
            WaylandEvent::ImeEnter(s) => Some(s),
            WaylandEvent::ImeLeave(s) => Some(s),
            _ => None,
        }
    }
}

/// External dispatcher for emitting Wayland events from outside the application
#[derive(Clone)]
pub struct WaylandEventEmitter {
    dispatch_fn: Arc<dyn Fn(DispatchToken) + Send + Sync + 'static>,
    events: Arc<Mutex<Vec<WaylandEvent>>>,
}

impl WaylandEventEmitter {
    /// Emit events to the application
    pub fn emit_events(&self, events: Vec<WaylandEvent>) {
        self.events.lock().unwrap().extend(events);
        (self.dispatch_fn)(DispatchToken::external());
    }
}

struct TrackedPointer {
    pointer: WlPointer,
    seat: ObjectId,
    focus: Option<WlSurface>,
    buttons: HashMap<u32, WlSurface>,
}

impl Drop for TrackedPointer {
    fn drop(&mut self) {
        if self.pointer.is_alive() {
            if self.pointer.version() >= 3 {
                self.pointer.release();
            } else if let Some(backend) = self.pointer.backend().upgrade() {
                // Before v3 there is no release request. Retire local state
                // without sending an unsupported protocol destructor.
                let _ = backend.destroy_object(&self.pointer.id());
            }
        }
    }
}

struct OwnedCursorShapeDevice(WpCursorShapeDeviceV1);

impl Drop for OwnedCursorShapeDevice {
    fn drop(&mut self) {
        if self.0.is_alive() {
            self.0.destroy();
        }
    }
}

pub struct Application {
    wayland_events: Arc<Mutex<Vec<WaylandEvent>>>,
    event_queue: Option<EventQueue<Self>>,
    pub qh: QueueHandle<Self>,
    pub registry_state: RegistryState,
    pub seat_state: SeatState,
    pub output_state: OutputState,
    pub shm_state: Shm,
    pub compositor_state: CompositorState,
    pub subcompositor_state: SubcompositorState,
    pub xdg_shell: XdgShell,
    pub layer_shell: LayerShell,
    #[cfg(feature = "clipboard")]
    pub clipboard: Clipboard,
    pub viewporter: SimpleGlobal<WpViewporter, 1>,
    pub text_input_manager: Option<SimpleGlobal<ZwpTextInputManagerV3, 1>>,
    pub text_input: Option<ZwpTextInputV3>,

    cursor_shape_manager: CursorShapeManager,
    last_pointer_enter_serial: Option<u32>,
    last_pointer: Option<WlPointer>,
    pointer_focused_surface: Option<ObjectId>,
    pointer_position: Option<(f64, f64)>,
    // Destroy cursor-device children before the owned pointers they refer to.
    pointer_shape_devices: HashMap<ObjectId, OwnedCursorShapeDevice>,
    pointers: HashMap<ObjectId, TrackedPointer>,
    keyboard_focused_surface: Option<ObjectId>,
    keyboard_modifiers: KeyboardModifiers,
    keyboard_repeat: KeyboardRepeat,
    dispatcher: Option<InternalDispatcherThread>,
    dispatch_fn: Arc<dyn Fn(DispatchToken) + Send + Sync + 'static>,
    // Keep the display alive until clipboard and native state have been dropped.
    pub conn: Connection,
}

impl Application {
    /// Create a new Application, initializing all Wayland globals and state.
    pub fn new<T: Fn(DispatchToken) + Send + Sync + 'static>(dispatch_fn: T) -> Self {
        let conn = Connection::connect_to_env().expect("Failed to connect to Wayland");
        let (globals, event_queue) =
            registry_queue_init::<Self>(&conn).expect("Failed to init registry");
        let qh: QueueHandle<Self> = event_queue.handle();

        // Bind required globals
        let compositor_state =
            CompositorState::bind(&globals, &qh).expect("wl_compositor not available");
        let subcompositor_state =
            SubcompositorState::bind(compositor_state.wl_compositor().clone(), &globals, &qh)
                .expect("wl_subcompositor not available");
        let xdg_shell = XdgShell::bind(&globals, &qh).expect("xdg shell not available");
        let shm_state = Shm::bind(&globals, &qh).expect("wl_shm not available");
        let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell not available");
        let cursor_shape_manager =
            CursorShapeManager::bind(&globals, &qh).expect("cursor shape manager not available");
        let viewporter = SimpleGlobal::<WpViewporter, 1>::bind(&globals, &qh)
            .expect("wp_viewporter not available");

        // TODO: Enable IME support in future
        // let text_input_manager = SimpleGlobal::<ZwpTextInputManagerV3,
        // 1>::bind(&globals, &qh) .expect("zwp_text_input_manager_v3 not
        // available");

        // SAFETY: conn is dropped after clipboard (field declaration order).
        #[cfg(feature = "clipboard")]
        let clipboard = unsafe { Clipboard::new(conn.display().id().as_ptr() as *mut _) };
        let dispatch_fn: Arc<dyn Fn(DispatchToken) + Send + Sync> = Arc::new(dispatch_fn);
        let repeat_dispatch_fn = dispatch_fn.clone();

        Self {
            wayland_events: Arc::new(Mutex::new(Vec::new())),
            event_queue: Some(event_queue),
            conn,
            qh: qh.clone(),
            subcompositor_state,
            registry_state: RegistryState::new(&globals),
            seat_state: SeatState::new(&globals, &qh),
            output_state: OutputState::new(&globals, &qh),
            shm_state,
            compositor_state,
            xdg_shell,
            layer_shell,
            #[cfg(feature = "clipboard")]
            clipboard,
            viewporter,
            text_input_manager: None,
            text_input: None,
            cursor_shape_manager,
            last_pointer_enter_serial: None,
            last_pointer: None,
            pointer_focused_surface: None,
            pointer_position: None,
            pointer_shape_devices: HashMap::new(),
            pointers: HashMap::new(),
            keyboard_focused_surface: None,
            keyboard_modifiers: KeyboardModifiers::default(),
            keyboard_repeat: KeyboardRepeat::new(move || {
                repeat_dispatch_fn(DispatchToken::external())
            }),
            dispatcher: None,
            dispatch_fn,
        }
    }

    pub fn take_wayland_events(&mut self) -> Vec<WaylandEvent> {
        let mut events = self.wayland_events.lock().unwrap();
        events.drain(..).collect()
    }

    pub fn get_event_emitter(&self) -> WaylandEventEmitter {
        WaylandEventEmitter {
            dispatch_fn: self.dispatch_fn.clone(),
            events: self.wayland_events.clone(),
        }
    }

    /// Get information about an output (monitor)
    pub fn output_info(&self, output: &WlOutput) -> Option<OutputInfo> {
        self.output_state.info(output)
    }

    /// Get an iterator over all known outputs
    pub fn outputs(&self) -> impl Iterator<Item = WlOutput> + '_ {
        self.output_state.outputs()
    }

    fn push_wayland_event(&self, event: WaylandEvent) {
        self.wayland_events.lock().unwrap().push(event);
    }

    pub fn set_cursor(&mut self, shape: Shape) {
        if !self.has_live_pointer_focus() {
            return;
        }
        if let Some(serial) = self.last_pointer_enter_serial
            && let Some(pointer) = &self.last_pointer
        {
            let pointer_id = pointer.id();
            let device = self
                .pointer_shape_devices
                .entry(pointer_id)
                .or_insert_with(|| {
                    trace!(
                        "[COMMON] Creating new cursor shape device for pointer id {}",
                        pointer.id()
                    );
                    OwnedCursorShapeDevice(
                        self.cursor_shape_manager
                            .get_shape_device(pointer, &self.qh),
                    )
                });
            device.0.set_shape(serial, shape);
        }
    }

    pub(crate) fn hide_cursor(&mut self) {
        if !self.has_live_pointer_focus() {
            return;
        }
        if let Some(serial) = self.last_pointer_enter_serial
            && let Some(pointer) = &self.last_pointer
        {
            pointer.set_cursor(serial, None, 0, 0);
        }
    }

    fn has_live_pointer_focus(&self) -> bool {
        self.last_pointer.as_ref().is_some_and(Proxy::is_alive)
            && self
                .pointer_focused_surface
                .as_ref()
                .is_some_and(|id| self.conn.backend().info(id.clone()).is_ok())
    }

    pub(crate) fn pointer_has_focus(&self, surface: &WlSurface) -> bool {
        surface.is_alive()
            && self.has_live_pointer_focus()
            && self.pointer_focused_surface.as_ref() == Some(&surface.id())
    }

    pub(crate) fn pointer_focus_position(&self, surface: &WlSurface) -> Option<(f64, f64)> {
        self.pointer_has_focus(surface)
            .then_some(self.pointer_position)
            .flatten()
    }

    pub(crate) fn keyboard_focus_modifiers(
        &self,
        surface: &WlSurface,
    ) -> Option<KeyboardModifiers> {
        (self.keyboard_focused_surface.as_ref() == Some(&surface.id()) && surface.is_alive())
            .then_some(self.keyboard_modifiers)
    }

    pub(crate) fn current_keyboard_modifiers(&self) -> KeyboardModifiers {
        if self
            .keyboard_focused_surface
            .as_ref()
            .is_some_and(|id| self.conn.backend().info(id.clone()).is_err())
        {
            KeyboardModifiers::default()
        } else {
            self.keyboard_modifiers
        }
    }

    fn track_pointer(&mut self, seat: &wl_seat::WlSeat, pointer: WlPointer) {
        self.pointers.insert(
            pointer.id(),
            TrackedPointer {
                pointer,
                seat: seat.id(),
                focus: None,
                buttons: HashMap::new(),
            },
        );
    }

    fn remove_pointers(&mut self, seat: &wl_seat::WlSeat) {
        let ids: Vec<_> = self
            .pointers
            .iter()
            .filter_map(|(id, pointer)| (pointer.seat == seat.id()).then_some(id.clone()))
            .collect();
        for event in self.retire_pointers(ids) {
            self.push_wayland_event(event);
        }
    }

    fn retire_pointers(&mut self, ids: impl IntoIterator<Item = ObjectId>) -> Vec<WaylandEvent> {
        let mut notified = Vec::new();
        let mut events = Vec::new();
        for id in ids {
            let Some(tracked) = self.pointers.remove(&id) else {
                continue;
            };
            // Normal leave may already have happened during a drag. Remember
            // button-owner surfaces too, since PointerGone does not release egui buttons.
            for surface in tracked.focus.iter().chain(tracked.buttons.values()) {
                let surface_id = surface.id();
                if surface.is_alive() && !notified.contains(&surface_id) {
                    notified.push(surface_id);
                    events.push(WaylandEvent::PointerRemoved(surface.clone()));
                }
            }
            if self
                .last_pointer
                .as_ref()
                .is_some_and(|last| last.id() == id)
            {
                self.last_pointer = None;
                self.last_pointer_enter_serial = None;
                self.pointer_focused_surface = None;
                self.pointer_position = None;
            }
            drop(self.pointer_shape_devices.remove(&id));
        }
        // Destroying a surface locally need not deliver a pointer leave. Keep
        // the device registration, but discard ownership of dead surfaces.
        for tracked in self.pointers.values_mut() {
            if tracked
                .focus
                .as_ref()
                .is_some_and(|surface| !surface.is_alive())
            {
                tracked.focus = None;
            }
            tracked.buttons.retain(|_, surface| surface.is_alive());
        }
        if self
            .pointer_focused_surface
            .as_ref()
            .is_some_and(|id| self.conn.backend().info(id.clone()).is_err())
        {
            self.last_pointer = None;
            self.last_pointer_enter_serial = None;
            self.pointer_focused_surface = None;
            self.pointer_position = None;
        }
        events
    }

    fn remove_keyboard(&mut self, seat: &wl_seat::WlSeat) {
        for surface in self.keyboard_repeat.remove_keyboard(seat) {
            if self.keyboard_focused_surface.as_ref() == Some(&surface.id()) {
                self.keyboard_focused_surface = None;
                self.keyboard_modifiers = KeyboardModifiers::default();
                self.push_wayland_event(WaylandEvent::ModifiersChanged(self.keyboard_modifiers));
            }
            if surface.is_alive() {
                self.push_wayland_event(WaylandEvent::KeyboardLeave(surface));
            }
        }
    }

    /// Asynchronous way to run the Wayland event loop
    ///
    /// Connection reading happens blockingly in separate thread, but
    /// dispatching is done by the user via `AsyncDispatcher::dispatch_pending`
    /// The callback should enqueue/wake the owner rather than block. Shutdown
    /// cancels idle waits asynchronously; it does not join an active callback.
    ///
    /// See egui_tokio_async.rs example for usage.
    ///
    /// This may panic if the event queue has already been taken.
    pub fn run_dispatcher(&mut self) {
        let event_queue = self.event_queue.take().expect("Event queue already used");
        let dispatch_fn = self.dispatch_fn.clone();
        let mut dispatcher =
            InternalDispatcherThread::new(self.conn.clone(), event_queue, dispatch_fn);
        dispatcher.start_thread();
        self.dispatcher = Some(dispatcher);
    }

    /// Dispatch pending events, and return collected Wayland events
    ///
    /// Used with `run_dispatcher` method, when dispatch_fn is signaled
    /// Either token also drains already-queued native callbacks before timers;
    /// a native token additionally acknowledges the connection reader handoff.
    /// Panics on a terminal connection/dispatch failure, or a native token
    /// received before starting the dispatcher.
    pub fn dispatch_pending(&mut self, token: DispatchToken) -> Vec<WaylandEvent> {
        self.check_connection_error();
        // External wakeups may be processed before an already-signaled native
        // token. Drain queued native callbacks on either path so releases/focus
        // loss are applied before due timers. This does not read the socket.
        let mut events = if let Some(mut dispatcher) = self.dispatcher.take() {
            let res = dispatcher.dispatch_pending(self, token.from_wayland());
            self.dispatcher = Some(dispatcher);
            res
        } else if !token.from_wayland() {
            self.take_wayland_events()
        } else {
            panic!("Dispatcher not running, call run_dispatcher first");
        };
        // A display error can be parsed by these callbacks, after the entry
        // check. Never generate repeats after discovering that terminal state.
        self.check_connection_error();
        // Managed device retirement is driven by seat/capability callbacks.
        // Still discard locally destroyed surface ownership on an idle wake.
        events.extend(self.retire_pointers([]));
        events.extend(self.take_wayland_events());
        if self
            .keyboard_focused_surface
            .as_ref()
            .is_some_and(|id| self.conn.backend().info(id.clone()).is_err())
        {
            // Local surface destruction need not produce a native leave. Other
            // views still need to discard this application's stale modifiers.
            self.keyboard_focused_surface = None;
            self.keyboard_modifiers = KeyboardModifiers::default();
            events.push(WaylandEvent::ModifiersChanged(self.keyboard_modifiers));
        }
        // Process native release/focus events before timers, so a due repeat
        // cannot escape after its key was released or its surface lost focus.
        events.extend(
            self.keyboard_repeat
                .dispatch()
                .into_iter()
                .map(WaylandEvent::KeyRepeat),
        );
        events
    }

    fn check_connection_error(&self) {
        // The backend records terminal errors even when no events are queued.
        if let Some(error) = self.conn.backend().last_error() {
            panic!("Wayland connection failed: {error:?}");
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct DispatchToken(bool);

impl DispatchToken {
    /// Create a token indicating dispatch from Wayland connection
    pub(crate) fn wayland() -> Self {
        DispatchToken(true)
    }

    /// Create a token indicating external dispatch
    pub(crate) fn external() -> Self {
        DispatchToken(false)
    }

    /// Check if the dispatch is from Wayland connection
    fn from_wayland(&self) -> bool {
        self.0
    }
}

enum InternalDispatcherError {
    RecvError(std::sync::mpsc::RecvError),
    WaylandError(WaylandError),
}
impl From<std::sync::mpsc::RecvError> for InternalDispatcherError {
    fn from(e: std::sync::mpsc::RecvError) -> Self {
        InternalDispatcherError::RecvError(e)
    }
}
impl From<WaylandError> for InternalDispatcherError {
    fn from(e: WaylandError) -> Self {
        InternalDispatcherError::WaylandError(e)
    }
}

struct InternalDispatcherThread {
    // Close both wakeup owners before native queue cleanup. Channel closure
    // wakes a handoff wait; socket HUP cancels an idle prepared-read poll.
    _stop_sender: UnixStream,
    count_sender: std::sync::mpsc::Sender<Option<usize>>,
    conn: Option<Connection>,
    event_queue: EventQueue<Application>,
    count_reader: Option<std::sync::mpsc::Receiver<Option<usize>>>,
    dispatch_fn: Option<Arc<dyn Fn(DispatchToken) + Sync + Send + 'static>>,
    #[allow(dead_code)]
    reader_thread: Option<JoinHandle<()>>,
    stop_reader: Option<UnixStream>,
}

impl InternalDispatcherThread {
    fn new(
        conn: Connection,
        event_queue: EventQueue<Application>,
        dispatch_fn: Arc<dyn Fn(DispatchToken) + Sync + Send + 'static>,
    ) -> Self {
        let (count_sender, count_reader) = std::sync::mpsc::channel::<Option<usize>>();
        let (stop_sender, stop_reader) = UnixStream::pair().expect("reader shutdown socket pair");
        InternalDispatcherThread {
            conn: Some(conn),
            event_queue,
            count_sender,
            count_reader: Some(count_reader),
            dispatch_fn: Some(dispatch_fn),
            reader_thread: None,
            _stop_sender: stop_sender,
            stop_reader: Some(stop_reader),
        }
    }

    /// Start the blocking reading thread
    pub(crate) fn start_thread(&mut self) {
        let count_reader = self.count_reader.take().expect("Count reader missing");
        let dispatch_fn = self.dispatch_fn.take().expect("Dispatch function missing");
        let conn = self.conn.take().expect("Connection missing");
        let stop_reader = self
            .stop_reader
            .take()
            .expect("Reader shutdown socket missing");

        self.reader_thread = Some(std::thread::spawn(move || {
            // Initial trigger dispatching
            (dispatch_fn)(DispatchToken::wayland());

            loop {
                match InternalDispatcherThread::read_blocking(
                    &conn,
                    &dispatch_fn,
                    &count_reader,
                    &stop_reader,
                ) {
                    Ok(cont) => {
                        if !cont {
                            break;
                        }
                    }
                    Err(InternalDispatcherError::RecvError(_)) => {
                        // Sender dropped, exit thread
                        break;
                    }
                    Err(InternalDispatcherError::WaylandError(e)) => {
                        // The application may be idle with no future external
                        // wakeup. Let dispatch_pending observe the terminal
                        // error instead of silently abandoning its event loop.
                        (dispatch_fn)(DispatchToken::wayland());
                        eprintln!("Error in Wayland reader thread: {:?}", e);
                        break;
                    }
                }
            }
        }));
    }

    /// Reads the wayland connection blockingly, and calls the user defined
    /// dispatch function
    #[inline]
    fn read_blocking(
        conn: &Connection,
        dispatch_fn: &Arc<dyn Fn(DispatchToken) + Sync + Send + 'static>,
        count_reader: &std::sync::mpsc::Receiver<Option<usize>>,
        stop_reader: &UnixStream,
    ) -> Result<bool, InternalDispatcherError> {
        // Implementation follows `EventQueue::blocking_dispatch` logic
        match count_reader.recv()? {
            Some(count) => {
                if count > 0 {
                    (dispatch_fn)(DispatchToken::wayland());
                    return Ok(true);
                }
            }
            None => return Ok(false),
        }

        // A full outgoing socket is transient, not a terminal backend error.
        let flush = || match conn.flush() {
            Ok(()) => Ok(false),
            Err(WaylandError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {
                Ok(true)
            }
            Err(error) => Err(error),
        };
        let mut write_blocked = flush()?;

        if let Some(guard) = conn.prepare_read() {
            // Do not enter libwayland's coordinated read before fd readiness.
            // Another prepared reader may otherwise keep us blocked inside C,
            // unable to observe local shutdown. Dropping the guard on stop
            // cancels our prepared read without waiting for that other reader.
            let display_fd = guard.connection_fd();
            let flags = rustix::event::PollFlags::IN
                | rustix::event::PollFlags::ERR
                | rustix::event::PollFlags::HUP;
            loop {
                let mut events = flags;
                if write_blocked {
                    events |= rustix::event::PollFlags::OUT;
                }
                let mut fds = [
                    rustix::event::PollFd::new(&display_fd, events),
                    rustix::event::PollFd::new(stop_reader, flags),
                ];
                match rustix::event::poll(&mut fds, None) {
                    Ok(_) => {}
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(error) => return Err(WaylandError::Io(error.into()).into()),
                }
                if !fds[1].revents().is_empty() {
                    return Ok(false);
                }
                if write_blocked && fds[0].revents().contains(rustix::event::PollFlags::OUT) {
                    write_blocked = flush()?;
                }
                if fds[0].revents().intersects(flags) {
                    break;
                }
            }
            match guard.read_without_dispatch() {
                Ok(()) => {}
                // A different reader may have consumed readiness. Acknowledge
                // on the main thread, then retry the normal readiness wait.
                Err(WaylandError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        } else {
            // Another reader may already have queued events. This is normal
            // readiness, not a socket failure; dispatch them on the main thread.
            trace!("Wayland events already pending; dispatching without a socket read");
        }

        (dispatch_fn)(DispatchToken::wayland());
        Ok(true) // Continue
    }

    /// Dispatch pending events, and return collected Wayland events
    pub(crate) fn dispatch_pending(
        &mut self,
        app: &mut Application,
        acknowledge_reader: bool,
    ) -> Vec<WaylandEvent> {
        let count = self
            .event_queue
            .dispatch_pending(app)
            .expect("Wayland dispatch failed");
        // Only a native token acknowledges a reader handoff. Sending a count
        // for an external wakeup would accumulate surplus read permits and
        // could block/retrigger the reader independently of native readiness.
        if acknowledge_reader {
            let _ = self.count_sender.send(Some(count));
        }
        app.take_wayland_events()
    }
}

impl CompositorHandler for Application {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &WlSurface,
        new_factor: i32,
    ) {
        self.push_wayland_event(WaylandEvent::ScaleFactorChanged(
            surface.clone(),
            new_factor,
        ));
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &WlSurface,
        _new_transform: wl_output::Transform,
    ) {
        self.push_wayland_event(WaylandEvent::TransformChanged(surface.clone()));
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &WlSurface,
        time: u32,
    ) {
        self.push_wayland_event(WaylandEvent::Frame(surface.clone(), time));
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &WlSurface,
        output: &WlOutput,
    ) {
        self.push_wayland_event(WaylandEvent::SurfaceEnteredOutput(
            surface.clone(),
            output.clone(),
        ));
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &WlSurface,
        output: &WlOutput,
    ) {
        self.push_wayland_event(WaylandEvent::SurfaceLeftOutput(
            surface.clone(),
            output.clone(),
        ));
    }
}

impl OutputHandler for Application {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, output: WlOutput) {
        self.push_wayland_event(WaylandEvent::OutputCreated(output));
    }

    fn update_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, output: WlOutput) {
        self.push_wayland_event(WaylandEvent::OutputUpdated(output));
    }

    fn output_destroyed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, output: WlOutput) {
        self.push_wayland_event(WaylandEvent::OutputDestroyed(output));
    }
}

impl LayerShellHandler for Application {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, target_layer: &LayerSurface) {
        self.push_wayland_event(WaylandEvent::LayerShellClosed(target_layer.clone()));
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        target_layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        self.push_wayland_event(WaylandEvent::LayerShellConfigure(
            target_layer.clone(),
            configure.clone(),
        ));
    }
}

impl PopupHandler for Application {
    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        target_popup: &Popup,
        config: PopupConfigure,
    ) {
        self.push_wayland_event(WaylandEvent::PopupConfigure(
            target_popup.clone(),
            config.clone(),
        ));

        trace!("[COMMON] XDG popup configure");
    }

    fn done(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, target_popup: &Popup) {
        self.push_wayland_event(WaylandEvent::PopupDone(target_popup.clone()));

        trace!("[COMMON] XDG popup done");
    }
}

impl WindowHandler for Application {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, target_window: &Window) {
        trace!("[COMMON] XDG window close requested");
        self.push_wayland_event(WaylandEvent::WindowRequestClose(target_window.clone()));
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        target_window: &Window,
        configure: WindowConfigure,
        _serial: u32,
    ) {
        self.push_wayland_event(WaylandEvent::WindowConfigure(
            target_window.clone(),
            configure.clone(),
        ));

        trace!("[COMMON] XDG window configure");
    }
}

impl PointerHandler for Application {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        if !pointer.is_alive() {
            return;
        }
        // Only pointers acquired by Application's capability handler are owned
        // and retired here. Other live SDK proxies retain upstream's callback
        // forwarding behavior without silently becoming managed devices.
        for event in events {
            // Callbacks can already be queued when their surface is destroyed.
            // They must not restore focus, button ownership or cursor serials.
            if !event.surface.is_alive() {
                continue;
            }
            if let Some(tracked) = self.pointers.get_mut(&pointer.id()) {
                match event.kind {
                    PointerEventKind::Enter { .. } => {
                        tracked.focus = Some(event.surface.clone());
                    }
                    PointerEventKind::Leave { .. }
                        if tracked.focus.as_ref() == Some(&event.surface) =>
                    {
                        tracked.focus = None;
                    }
                    PointerEventKind::Press { button, .. } => {
                        tracked.buttons.insert(button, event.surface.clone());
                    }
                    PointerEventKind::Release { button, .. } => {
                        tracked.buttons.remove(&button);
                    }
                    _ => {}
                }
            }
            match event.kind {
                // Changing cursor shape requires last enter serial number, we are storing it here
                PointerEventKind::Enter { serial } => {
                    self.last_pointer_enter_serial = Some(serial);
                    self.last_pointer = Some(pointer.clone());
                    self.pointer_focused_surface = Some(event.surface.id());
                    self.pointer_position = Some(event.position);
                }
                PointerEventKind::Leave { .. }
                    if self
                        .last_pointer
                        .as_ref()
                        .is_some_and(|last| last.id() == pointer.id())
                        && self.pointer_focused_surface.as_ref() == Some(&event.surface.id()) =>
                {
                    self.last_pointer_enter_serial = None;
                    self.last_pointer = None;
                    self.pointer_focused_surface = None;
                    self.pointer_position = None;
                }
                _ => {}
            }

            if self
                .last_pointer
                .as_ref()
                .is_some_and(|last| last.id() == pointer.id())
                && self.pointer_has_focus(&event.surface)
                && matches!(event.kind, PointerEventKind::Motion { .. })
            {
                self.pointer_position = Some(event.position);
            }

            self.push_wayland_event(WaylandEvent::PointerEvent((
                event.surface.clone(),
                event.position,
                event.kind.clone(),
            )));
        }
    }
}

impl KeyboardHandler for Application {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        keyboard: &WlKeyboard,
        surface: &WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
        if !keyboard.is_alive() || !surface.is_alive() {
            return;
        }
        trace!("[MAIN] Keyboard focus gained on surface {:?}", surface.id());
        self.keyboard_repeat.enter(keyboard, surface);
        self.push_wayland_event(WaylandEvent::KeyboardEnter(
            surface.clone(),
            _raw.to_vec(),
            _keysyms.to_vec(),
        ));

        self.keyboard_focused_surface = Some(surface.id());
        // The compositor follows enter with its current modifiers. Do not
        // reuse another focus owner's cached state before that update arrives.
        self.keyboard_modifiers = KeyboardModifiers::default();
    }

    fn leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        keyboard: &WlKeyboard,
        surface: &WlSurface,
        _serial: u32,
    ) {
        if !keyboard.is_alive()
            || (self.keyboard_repeat.contains_keyboard(keyboard)
                && !self.keyboard_repeat.focus_matches(keyboard, surface))
        {
            return;
        }
        trace!("[MAIN] Keyboard focus lost");
        self.keyboard_repeat.leave(keyboard);
        if self.keyboard_focused_surface.as_ref() == Some(&surface.id()) {
            self.keyboard_focused_surface = None;
            self.keyboard_modifiers = KeyboardModifiers::default();
            self.push_wayland_event(WaylandEvent::ModifiersChanged(self.keyboard_modifiers));
        }
        self.push_wayland_event(WaylandEvent::KeyboardLeave(surface.clone()));
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        keyboard: &WlKeyboard,
        _serial: u32,
        mut event: KeyEvent,
    ) {
        if !self.keyboard_repeat.accepts_key_event(keyboard) {
            return;
        }
        trace!("[MAIN] Key pressed: keycode={}", event.raw_code);
        self.keyboard_repeat.press(keyboard, &mut event);
        self.push_wayland_event(WaylandEvent::KeyPress(event));
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        keyboard: &WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        if !self.keyboard_repeat.accepts_key_event(keyboard) {
            return;
        }
        trace!("[MAIN] Key released: keycode={}", event.raw_code);
        self.keyboard_repeat.release(keyboard, event.raw_code);
        self.push_wayland_event(WaylandEvent::KeyRelease(event.clone()));
    }

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        keyboard: &WlKeyboard,
        _serial: u32,
        modifiers: smithay_client_toolkit::seat::keyboard::Modifiers,
        raw_modifiers: smithay_client_toolkit::seat::keyboard::RawModifiers,
        layout: u32,
    ) {
        // Wayland permits modifiers without keyboard focus (e.g. pointer focus).
        // Only a retired proxy, not absence of focus/ownership, suppresses them.
        if !keyboard.is_alive() {
            return;
        }
        self.keyboard_repeat
            .update_modifiers(keyboard, raw_modifiers, layout);
        self.keyboard_modifiers = modifiers;
        self.push_wayland_event(WaylandEvent::ModifiersChanged(modifiers));
    }

    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        keyboard: &WlKeyboard,
        _serial: u32,
        mut event: KeyEvent,
    ) {
        trace!("[MAIN] Key repeated: keycode={}", event.raw_code);
        if self.keyboard_repeat.server_repeat(keyboard, &mut event) {
            self.push_wayland_event(WaylandEvent::KeyRepeat(event));
        }
    }

    fn update_repeat_info(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &WlKeyboard,
        info: RepeatInfo,
    ) {
        self.keyboard_repeat.update_info(keyboard, info);
    }

    fn update_keymap(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        keyboard: &WlKeyboard,
        keymap: Keymap<'_>,
    ) {
        self.keyboard_repeat.update_keymap(keyboard, keymap);
    }
}

impl SeatHandler for Application {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        trace!("[MAIN] New seat capability: {:?}", capability);
        if capability == Capability::Keyboard {
            trace!("[MAIN] Creating wl_keyboard");
            match self.seat_state.get_keyboard(qh, &seat, None) {
                Ok(wl_keyboard) => {
                    self.keyboard_repeat.add_keyboard(&seat, wl_keyboard);
                    trace!("[MAIN] wl_keyboard created successfully");
                }
                Err(e) => {
                    trace!("[MAIN] Failed to create wl_keyboard: {:?}", e);
                }
            }
            if self.text_input.is_none() {
                if let Some(manager) = self.text_input_manager.as_ref() {
                    if let Ok(manager) = manager.get() {
                        self.text_input = Some(manager.get_text_input(&seat, qh, ()));
                        trace!("[MAIN] Created zwp_text_input_v3");
                    }
                }
            }
        }
        if capability == Capability::Pointer {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(pointer) => self.track_pointer(&seat, pointer),
                Err(error) => trace!("[MAIN] Failed to create pointer: {error}"),
            }
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            self.remove_keyboard(&seat);
        }
        if capability == Capability::Pointer {
            self.remove_pointers(&seat);
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.remove_keyboard(&seat);
        self.remove_pointers(&seat);
    }
}

impl ShmHandler for Application {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm_state
    }
}

impl ProvidesRegistryState for Application {
    registry_handlers![OutputState, SeatState];

    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
}

impl AsMut<SimpleGlobal<WpViewporter, 1>> for Application {
    fn as_mut(&mut self) -> &mut SimpleGlobal<WpViewporter, 1> {
        &mut self.viewporter
    }
}

// impl AsMut<SimpleGlobal<ZwpTextInputManagerV3, 1>> for Application {
//     fn as_mut(&mut self) -> &mut SimpleGlobal<ZwpTextInputManagerV3, 1> {
//         self.text_input_manager.as_mut().unwrap()
//     }
// }

impl Dispatch<ZwpTextInputV3, ()> for Application {
    fn event(
        state: &mut Application,
        _ti: &ZwpTextInputV3,
        evt: zwp_text_input_v3::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match evt {
            zwp_text_input_v3::Event::Enter { surface } => {
                trace!(
                    "[COMMON] Text input enter event for surface {:?}",
                    surface.id()
                );
                state.push_wayland_event(WaylandEvent::ImeEnter(surface));
            }
            zwp_text_input_v3::Event::Leave { surface } => {
                trace!(
                    "[COMMON] Text input leave event for surface {:?}",
                    surface.id()
                );
                state.push_wayland_event(WaylandEvent::ImeLeave(surface));
            }
            zwp_text_input_v3::Event::CommitString { text } => {
                trace!("[COMMON] Received committed text input: {:?}", text);
                state.push_wayland_event(WaylandEvent::ImeCommitString(text));
            }
            zwp_text_input_v3::Event::PreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                trace!(
                    "[COMMON] Received preedit text input: text={:?}, cursor_begin={}, \
                     cursor_end={}",
                    text, cursor_begin, cursor_end
                );
                state.push_wayland_event(WaylandEvent::ImePreeditString(
                    text,
                    cursor_begin,
                    cursor_end,
                ));
            }
            zwp_text_input_v3::Event::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                trace!(
                    "[COMMON] Received delete surrounding text input: before_length={}, \
                     after_length={}",
                    before_length, after_length
                );
                state.push_wayland_event(WaylandEvent::ImeDeleteSurroundingText(
                    before_length,
                    after_length,
                ));
            }
            zwp_text_input_v3::Event::Done { serial } => {
                trace!(
                    "[COMMON] Received text input done event with serial {}",
                    serial
                );
                state.push_wayland_event(WaylandEvent::ImeDone(serial));
            }
            _ => {
                trace!("[COMMON] Received unhandled text input event: {:?}", evt);
            }
        }
    }
}

impl Dispatch<WpViewport, ()> for Application {
    fn event(
        _: &mut Application,
        _: &WpViewport,
        _: wp_viewport::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // No events expected from wp_viewport
    }
}

impl Dispatch<WlRegion, ()> for Application {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegion,
        _event: <WlRegion as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch2<WpViewporter, Application> for () {
    fn event(
        &self,
        _: &mut Application,
        _: &WpViewporter,
        _: wp_viewporter::Event,
        _: &Connection,
        _: &QueueHandle<Application>,
    ) {
    }
}

delegate_registry!(Application);
smithay_client_toolkit::delegate_dispatch2!(Application);

// ----------------------------------------------------------------
// Request frame helper
// ----------------------------------------------------------------
// pub trait RequestFrame {
//     fn request_frame(&self, qh: &QueueHandle<Application>);
// }

// impl RequestFrame for LayerSurface {
//     fn request_frame(&self, qh: &QueueHandle<Application>) {
//         let wl_surface = self.wl_surface();
//         wl_surface.frame(qh, wl_surface.clone());
//         wl_surface.commit();
//     }
// }

// impl RequestFrame for Window {
//     fn request_frame(&self, qh: &QueueHandle<Application>) {
//         let wl_surface = self.wl_surface();
//         wl_surface.frame(qh, wl_surface.clone());
//         wl_surface.commit();
//     }
// }

// impl RequestFrame for Popup {
//     fn request_frame(&self, qh: &QueueHandle<Application>) {
//         let wl_surface = self.wl_surface();
//         wl_surface.frame(qh, wl_surface.clone());
//         wl_surface.commit();
//     }
// }

// impl RequestFrame for WlSurface {
//     fn request_frame(&self, qh: &QueueHandle<Application>) {
//         self.frame(qh, self.clone());
//         self.commit();
//     }
// }

#[cfg(test)]
mod dispatcher_tests {
    use super::*;
    use std::sync::mpsc::RecvTimeoutError;
    use std::sync::mpsc::channel;
    use std::time::Duration;
    use std::time::Instant;

    #[test]
    fn reader_flush_backpressure_waits_for_writable_and_remains_cancellable() {
        use std::io::Read;
        use std::io::Write;
        use wayland_client::protocol::wl_callback::WlCallback;
        struct Sink(usize);
        impl Dispatch<WlCallback, ()> for Sink {
            fn event(
                sink: &mut Self,
                _: &WlCallback,
                event: wayland_client::protocol::wl_callback::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                assert!(matches!(
                    event,
                    wayland_client::protocol::wl_callback::Event::Done { callback_data: 7 }
                ));
                sink.0 += 1;
            }
        }

        for (cancel, incoming) in [(false, false), (true, false), (false, true)] {
            let (mut socket, mut server) = UnixStream::pair().unwrap();
            socket.set_nonblocking(true).unwrap();
            server
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let conn = Connection::from_socket(socket.try_clone().unwrap()).unwrap();
            let mut queue = conn.new_event_queue::<Sink>();
            // In the duplex case the server already knows an earlier sync;
            // its response must remain readable while later writes are blocked.
            let prior = incoming.then(|| {
                let callback = conn.display().sync(&queue.handle(), ());
                conn.flush().unwrap();
                let mut request = [0; 12];
                server.read_exact(&mut request).unwrap();
                callback
            });
            // Fill only this fixture's outgoing kernel buffer. The peer discards
            // these known bytes before handling the subsequent real sync request.
            let mut filled = 0;
            loop {
                match socket.write(&[0; 8192]) {
                    Ok(n) => filled += n,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    result => panic!("could not establish write pressure: {result:?}"),
                }
                assert!(
                    filled < 4 * 1024 * 1024,
                    "socket never reached backpressure"
                );
            }
            let requested = conn.display().sync(&queue.handle(), ());
            let callback = prior.unwrap_or_else(|| requested.clone());
            assert!(matches!(conn.flush(), Err(WaylandError::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock));
            assert!(conn.backend().last_error().is_none());

            let (counts, count_reader) = channel();
            counts.send(Some(0)).unwrap();
            let (notifier, notified) = channel();
            let dispatch: Arc<dyn Fn(DispatchToken) + Send + Sync> = Arc::new(move |token| {
                notifier.send(token).unwrap();
            });
            let (stop, stop_reader) = UnixStream::pair().unwrap();
            let reader_conn = conn.clone();
            let (finished, result) = channel();
            let reader = std::thread::spawn(move || {
                let read = InternalDispatcherThread::read_blocking(
                    &reader_conn,
                    &dispatch,
                    &count_reader,
                    &stop_reader,
                );
                finished.send(read).unwrap();
            });
            let early = result.recv_timeout(Duration::from_millis(50));
            let waited = matches!(early, Err(RecvTimeoutError::Timeout));
            // Always release the worker before assertions, including on failure.
            if cancel || early.is_ok() {
                drop(stop);
            } else {
                if !incoming {
                    let mut discarded = vec![0; filled];
                    server.read_exact(&mut discarded).unwrap();
                    let mut request = [0; 12];
                    server.read_exact(&mut request).unwrap();
                    assert_eq!(u32::from_ne_bytes(request[0..4].try_into().unwrap()), 1);
                    assert_eq!(
                        u32::from_ne_bytes(request[4..8].try_into().unwrap()),
                        12 << 16
                    );
                    assert_eq!(
                        u32::from_ne_bytes(request[8..12].try_into().unwrap()),
                        requested.id().protocol_id()
                    );
                }
                let mut response = Vec::new();
                response.extend(callback.id().protocol_id().to_ne_bytes());
                response.extend((12_u32 << 16).to_ne_bytes());
                response.extend(7_u32.to_ne_bytes());
                server.write_all(&response).unwrap();
            }
            let read = match early {
                Ok(read) => read,
                Err(RecvTimeoutError::Timeout) => {
                    result.recv_timeout(Duration::from_secs(2)).unwrap()
                }
                Err(e) => panic!("reader disconnected: {e}"),
            };
            reader.join().unwrap();
            assert!(waited, "reader exited on transient flush pressure");
            assert!(
                matches!(read, Ok(value) if value != cancel),
                "reader failed"
            );
            if !cancel {
                assert!(
                    notified
                        .recv_timeout(Duration::from_secs(1))
                        .unwrap()
                        .from_wayland()
                );
                let mut sink = Sink(0);
                queue.dispatch_pending(&mut sink).unwrap();
                assert_eq!(sink.0, 1);
            }
            if incoming {
                assert!(matches!(conn.flush(), Err(WaylandError::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock));
            }
            assert!(conn.backend().last_error().is_none());
        }
    }

    #[test]
    #[ignore = "requires private headless runner; isolated connection with no clipboard"]
    fn dropping_idle_reader_releases_callback_without_native_traffic() {
        crate::test_support::require_private_compositor();
        const CHILD: &str = "WAYAPP_READER_DROP_PROBE";
        if std::env::var_os(CHILD).is_none() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "application::dispatcher_tests::dropping_idle_reader_releases_callback_without_native_traffic", "--ignored", "--nocapture"])
                .env(CHILD, "1").spawn().unwrap();
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        assert!(status.success(), "reader drop probe failed: {status}");
                        return;
                    }
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    state => {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("reader drop probe timed out: {state:?}");
                    }
                }
            }
        }
        let conn = Connection::connect_to_env().unwrap();
        let queue = conn.new_event_queue::<Application>();
        let (released_tx, released_rx) = channel();
        let retained = NotifyOnDrop(released_tx);
        let (ready_tx, ready_rx) = channel();
        let mut reader = InternalDispatcherThread::new(
            conn.clone(),
            queue,
            Arc::new(move |token| {
                let _ = &retained;
                let _ = ready_tx.send(token);
            }),
        );
        reader.start_thread();
        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        // Model another event loop preparing a read while it waits for fd
        // readiness (as clipboard integrations do). Keep that guard alive
        // until after measuring reader shutdown, without sending native data.
        let competing_connection = conn.clone();
        let competing_read = competing_connection.prepare_read().unwrap();
        reader.count_sender.send(Some(0)).unwrap();
        assert!(
            !reader.reader_thread.as_ref().unwrap().is_finished(),
            "reader stopped before the shutdown probe"
        );
        // Rescue the old implementation after measuring release: no orphan
        // reader is left behind even when this regression fails.
        let (rescue_tx, rescue_rx) = channel();
        let rescue = std::thread::spawn(move || {
            let _ = rescue_rx.recv_timeout(Duration::from_secs(2));
            conn.roundtrip().unwrap();
        });
        drop(reader);
        let released = released_rx.recv_timeout(Duration::from_secs(1));
        println!("idle callback released before rescue: {}", released.is_ok());
        drop(competing_read);
        let _ = rescue_tx.send(());
        rescue.join().unwrap();
        assert!(
            released.is_ok(),
            "reader shutdown needed another reader's cancellation/native traffic"
        );

        // Stop must not join a callback that is itself waiting on its owner.
        // The callback may finish after Drop, retaining its connection safely.
        let conn = Connection::connect_to_env().unwrap();
        let queue = conn.new_event_queue::<Application>();
        let (entered, entry) = channel();
        let (resume, gate) = channel();
        let gate = Mutex::new(gate);
        let (finished, finish) = channel();
        let retained = NotifyOnDrop(finished);
        let mut reader = InternalDispatcherThread::new(
            conn,
            queue,
            Arc::new(move |_| {
                let _ = &retained;
                let _ = entered.send(());
                let _ = gate.lock().unwrap().recv_timeout(Duration::from_secs(2));
            }),
        );
        reader.start_thread();
        entry.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(reader);
        let finished_before_release = finish.try_recv().is_ok();
        let resumed = resume.send(()).is_ok();
        assert!(
            !finished_before_release && resumed,
            "Drop waited for a callback that needed its owner to resume it"
        );
        finish
            .recv_timeout(Duration::from_secs(1))
            .expect("resumed callback retained reader state");
    }

    #[test]
    #[ignore = "requires private headless runner; uses a separate bare client connection"]
    fn idle_read_without_a_clipboard_reader_waits_for_socket_readiness() {
        crate::test_support::require_private_compositor();
        const CHILD: &str = "WAYAPP_IDLE_READ_PROBE";
        if std::env::var_os(CHILD).is_none() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "application::dispatcher_tests::idle_read_without_a_clipboard_reader_waits_for_socket_readiness", "--ignored", "--nocapture"])
                .env(CHILD, "1").spawn().unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        assert!(status.success(), "idle read probe failed: {status}");
                        return;
                    }
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    state => {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("idle read probe timed out: {state:?}");
                    }
                }
            }
        }
        // No Application/Clipboard is constructed: the reader must not depend
        // on another libwayland read guard to wait for actual socket activity.
        for nonblocking in [false, true] {
            let conn = if nonblocking {
                let path = std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap())
                    .join(std::env::var("WAYLAND_DISPLAY").unwrap());
                let socket = UnixStream::connect(path).unwrap();
                socket.set_nonblocking(true).unwrap(); // This fixture owns its transport.
                Connection::from_socket(socket).unwrap()
            } else {
                Connection::connect_to_env().unwrap()
            };
            let (counts, count_reader) = channel();
            counts.send(Some(0)).unwrap();
            let (notifications, notified) = channel();
            let dispatch: Arc<dyn Fn(DispatchToken) + Send + Sync> = Arc::new(move |token| {
                let _ = notifications.send(token);
            });
            let peer = conn.clone();
            let response = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                peer.roundtrip().unwrap();
            });
            let (_stop_sender, stop_reader) = UnixStream::pair().unwrap();
            let read = InternalDispatcherThread::read_blocking(
                &conn,
                &dispatch,
                &count_reader,
                &stop_reader,
            );
            response.join().unwrap();
            if let Err(InternalDispatcherError::WaylandError(error)) = &read {
                println!(
                    "idle read failed before readiness (nonblocking={nonblocking}): {error:?}"
                );
            }
            assert!(
                matches!(read, Ok(true)),
                "idle native read stopped before its delayed response"
            );
            assert!(notified.try_recv().unwrap().from_wayland());
            assert!(conn.backend().last_error().is_none());
        }
    }

    #[test]
    #[ignore = "requires private headless runner; induces an error on its own client only"]
    fn reader_reports_fatal_connection_error_to_an_idle_application() {
        crate::test_support::require_private_compositor();
        const CHILD: &str = "WAYAPP_READER_ERROR_PROBE";
        if std::env::var_os(CHILD).is_none() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "application::dispatcher_tests::reader_reports_fatal_connection_error_to_an_idle_application", "--ignored", "--nocapture"])
                .env(CHILD, "1")
                // Native libraries can repeat diagnostics after a
                // fatal display error. Never accumulate unbounded stderr here.
                .stderr(std::process::Stdio::null())
                .spawn().unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        assert!(status.success(), "reader error probe failed: {status}");
                        return;
                    }
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    state => {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("reader error probe timed out: {state:?}");
                    }
                }
            }
        }
        for protocol_error in [true, false] {
            let (tx, rx) = channel();
            let mut app = Application::new(move |token| {
                let _ = tx.send(token);
            });
            let surface = app.compositor_state.create_surface(&app.qh);
            app.run_dispatcher();
            // Consume startup callbacks/read handoffs until the reader is idle.
            while let Ok(token) = rx.recv_timeout(Duration::from_millis(50)) {
                app.dispatch_pending(token);
            }
            if protocol_error {
                surface.set_buffer_scale(0); // Deliberate protocol error, this client only.
                app.conn.flush().unwrap();
            } else {
                // Shutdown a duplicate of this client's transport. It affects no
                // other client or compositor and needs no raw-fd ownership tricks.
                let fd = app.conn.backend().poll_fd().try_clone_to_owned().unwrap();
                UnixStream::from(fd)
                    .shutdown(std::net::Shutdown::Both)
                    .unwrap();
            }
            let deadline = Instant::now() + Duration::from_millis(500);
            loop {
                assert!(
                    Instant::now() < deadline,
                    "terminal dispatch failure never surfaced"
                );
                let wake = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()));
                if wake.is_err() {
                    println!(
                        "terminal backend failure recorded: {}; missing wakeup: {wake:?}",
                        app.conn.backend().last_error().is_some()
                    );
                }
                let wake = wake.expect("fatal reader error never notified the idle application");
                assert!(wake.from_wayland());
                // Unrelated readiness is allowed before the terminal event,
                // but the event loop must eventually observe the failure.
                let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    app.dispatch_pending(wake)
                }));
                if failure.is_err() {
                    break;
                }
            }
            println!("terminal dispatch failure surfaced; protocol case: {protocol_error}");
            assert!(app.conn.backend().last_error().is_some());
            assert_eq!(app.conn.protocol_error().is_some(), protocol_error);
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    app.dispatch_pending(DispatchToken::external())
                }))
                .is_err(),
                "external wakeup ignored a terminal connection failure"
            );
        }
    }

    #[test]
    #[ignore = "requires a Wayland compositor"]
    fn external_dispatch_does_not_acknowledge_a_native_reader_handoff() {
        use smithay_client_toolkit::compositor::FrameCallbackData;
        crate::test_support::require_private_compositor();
        let _display_guard;
        let mut app = Application::new(|_| {});
        _display_guard = app.conn.clone();
        let surface = app.compositor_state.create_surface(&app.qh);
        let queue = app.event_queue.take().unwrap();
        app.dispatcher = Some(InternalDispatcherThread::new(
            app.conn.clone(),
            queue,
            Arc::new(|_| {}),
        ));
        // Queue a real protocol callback without dispatching its Application
        // event queue. The reader's handoff receiver stays local for inspection.
        let _marker = app
            .conn
            .display()
            .sync(&app.qh, FrameCallbackData(surface.clone()));
        app.conn.roundtrip().unwrap();
        let events = app.dispatch_pending(DispatchToken::external());
        assert!(
            events
                .iter()
                .any(|event| matches!(event, WaylandEvent::Frame(target, _) if target == &surface)),
            "external wakeup left an already-read native callback queued"
        );
        let counts = app
            .dispatcher
            .as_ref()
            .unwrap()
            .count_reader
            .as_ref()
            .unwrap();
        assert!(
            matches!(counts.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
            "external wakeup acknowledged a native reader handoff"
        );
        let events = app.dispatch_pending(DispatchToken::wayland());
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, WaylandEvent::Frame(target, _) if target == &surface)),
            "native callback was delivered twice"
        );
        let counts = app
            .dispatcher
            .as_ref()
            .unwrap()
            .count_reader
            .as_ref()
            .unwrap();
        assert!(
            matches!(counts.try_recv(), Ok(Some(_))),
            "native handoff was not acknowledged"
        );
        assert!(
            matches!(counts.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
            "reader received surplus acknowledgement"
        );
    }

    #[test]
    #[ignore = "requires a Wayland compositor advertising a seat"]
    fn runtime_seat_notifications_update_seat_state() {
        let mut app = Application::new(|_| {});
        let seat_name = app
            .registry_state
            .globals_by_interface("wl_seat")
            .next()
            .expect("compositor must advertise a seat")
            .name;
        let initial = app.seat_state.seats().count();
        let conn = app.conn.clone();
        let qh = app.qh.clone();
        // Exercise the same delegation hooks as live registry notifications.
        app.runtime_remove_global(&conn, &qh, seat_name, "wl_seat");
        assert_eq!(app.seat_state.seats().count(), initial - 1);
        app.runtime_add_global(&conn, &qh, seat_name, "wl_seat", 7);
        assert_eq!(app.seat_state.seats().count(), initial);
    }

    #[test]
    #[ignore = "requires Wayland globals; simulates client registry notifications, not monitor \
                hotplug"]
    fn runtime_output_notifications_remove_and_rebind_output_state() {
        let mut app = Application::new(|_| {});
        let mut queue = app.event_queue.take().unwrap();
        queue.roundtrip(&mut app).unwrap();
        queue.roundtrip(&mut app).unwrap();
        let global = app
            .registry_state
            .globals_by_interface("wl_output")
            .next()
            .unwrap();
        let (name, version) = (global.name, global.version);
        let old = app
            .output_state
            .outputs()
            .find(|output| {
                app.output_state
                    .info(output)
                    .is_some_and(|info| info.id == name)
            })
            .unwrap();
        let initial = app.output_state.outputs().count();
        app.take_wayland_events();
        let conn = app.conn.clone();
        let qh = app.qh.clone();
        app.runtime_remove_global(&conn, &qh, name, "wl_output");
        assert_eq!(app.output_state.outputs().count(), initial - 1);
        assert!(app.output_state.info(&old).is_none());
        let events = app.take_wayland_events();
        assert_eq!(
            events
                .iter()
                .filter(
                    |event| matches!(event, WaylandEvent::OutputDestroyed(output) if output == &old)
                )
                .count(),
            1
        );
        assert!(!old.is_alive());
        // The private compositor still advertises its output. Rebind this
        // client's SDK state without changing any actual output configuration.
        app.runtime_add_global(&conn, &qh, name, "wl_output", version);
        queue.roundtrip(&mut app).unwrap();
        queue.roundtrip(&mut app).unwrap();
        assert_eq!(app.output_state.outputs().count(), initial);
        let replacement = app
            .output_state
            .outputs()
            .find(|output| {
                app.output_state
                    .info(output)
                    .is_some_and(|info| info.id == name)
            })
            .unwrap();
        assert_ne!(replacement.id(), old.id());
        assert!(replacement.is_alive());
        assert!(app.take_wayland_events().iter().any(
            |event| matches!(event, WaylandEvent::OutputCreated(output) if output == &replacement)
        ));
        assert!(conn.protocol_error().is_none());
    }

    struct NotifyOnDrop(std::sync::mpsc::Sender<()>);
    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    #[test]
    #[ignore = "requires a Wayland compositor"]
    fn dropping_idle_application_releases_reader_thread_callback() {
        let (released_tx, released_rx) = channel();
        let notify = NotifyOnDrop(released_tx);
        let (tx, rx) = channel();
        let mut app = Application::new(move |token| {
            let _ = &notify;
            let _ = tx.send(token);
        });
        app.run_dispatcher();
        let deadline = Instant::now() + Duration::from_secs(3);
        // Drain startup traffic until the reader is waiting for compositor I/O.
        loop {
            assert!(Instant::now() < deadline, "compositor never became idle");
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(token) => {
                    app.dispatch_pending(token);
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(error) => panic!("dispatcher stopped unexpectedly: {error}"),
            }
        }
        assert!(
            !app.dispatcher
                .as_ref()
                .unwrap()
                .reader_thread
                .as_ref()
                .unwrap()
                .is_finished(),
            "reader stopped while application was idle"
        );
        drop(app);
        released_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("blocked reader retained the application's callback after drop");
    }
}

#[cfg(test)]
mod keyboard_callback_tests {
    use super::legacy_input_tests::LegacyInput;
    use super::*;
    use crate::WaylandToEguiInput;
    use smithay_client_toolkit::seat::keyboard::KeyboardData;
    use std::io::Write;
    use std::os::fd::AsFd;
    use wayland_client::WEnum;
    use wayland_client::protocol::wl_keyboard::Event;
    use wayland_client::protocol::wl_keyboard::KeyState;
    use wayland_client::protocol::wl_keyboard::KeymapFormat;
    use smithay_client_toolkit::reexports::protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
    use smithay_client_toolkit::reexports::protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

    /// Drive real SDK keymap/key/modifier callbacks without asking the test
    /// compositor to support protocol-v10 server repetition or injecting input.
    /// A private virtual keyboard enables the seat capability without sending
    /// keys; the real proxy makes local release/liveness checks meaningful.
    struct KeyboardCallbacks {
        input: WaylandToEguiInput, // borrowed display must outlive clipboard
        data: KeyboardData<Application, ()>,
        app: Application,
        keyboard: WlKeyboard,
        surface: WlSurface,
        context: egui::Context,
        text: String,
        repeats: usize,
        repeat_events: Vec<KeyEvent>,
        _device: KeyboardDevice,
    }

    struct KeyboardDevice {
        keyboard: ZwpVirtualKeyboardV1,
        _manager: ZwpVirtualKeyboardManagerV1,
        _queue: wayland_client::EventQueue<LegacyInput>,
        connection: Connection,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for KeyboardDevice {
        fn drop(&mut self) {
            self.keyboard.destroy();
            let _ = self.connection.flush();
        }
    }

    impl KeyboardCallbacks {
        fn new(variant: &str) -> Self {
            let guard = crate::test_support::private_input_test_guard();
            let mut app = Application::new(|_| {});
            let seat = app.seat_state.seats().next().unwrap();
            let (globals, mut queue) = registry_queue_init::<LegacyInput>(&app.conn).unwrap();
            let manager: ZwpVirtualKeyboardManagerV1 =
                globals.bind(&queue.handle(), 1..=1, ()).unwrap();
            let virtual_keyboard = manager.create_virtual_keyboard(&seat, &queue.handle(), ());
            let context = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
            let keymap = xkbcommon::xkb::Keymap::new_from_names(
                &context,
                "evdev",
                "pc105",
                "us",
                variant,
                None,
                xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
            .unwrap();
            let mut bytes = keymap
                .get_as_string(xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1)
                .into_bytes();
            bytes.push(0);
            let file = anonymous_keymap(&bytes);
            virtual_keyboard.keymap(1, file.as_fd(), bytes.len().try_into().unwrap());
            queue.roundtrip(&mut LegacyInput).unwrap();
            let keyboard = seat.get_keyboard(&queue.handle(), ());
            queue.roundtrip(&mut LegacyInput).unwrap();
            assert!(keyboard.is_alive());
            let data = KeyboardData::<Application, ()>::new(seat.clone(), ());
            app.keyboard_repeat.add_keyboard(&seat, keyboard.clone());
            let surface = app.compositor_state.create_surface(&app.qh);
            // SAFETY: input drops before app (which owns the native display).
            #[cfg(feature = "clipboard")]
            let clipboard = unsafe { Clipboard::new(app.conn.display().id().as_ptr() as *mut _) };
            let device = KeyboardDevice {
                keyboard: virtual_keyboard,
                _manager: manager,
                _queue: queue,
                connection: app.conn.clone(),
                _guard: guard,
            };
            let mut harness = Self {
                #[cfg(feature = "clipboard")]
                input: WaylandToEguiInput::new(clipboard),
                #[cfg(not(feature = "clipboard"))]
                input: WaylandToEguiInput::new(),
                data,
                app,
                keyboard,
                surface,
                context: egui::Context::default(),
                text: String::new(),
                repeats: 0,
                repeat_events: Vec::new(),
                _device: device,
            };
            harness.keymap("us", variant);
            harness.event(Event::Enter {
                serial: 1,
                surface: harness.surface.clone(),
                keys: Vec::new(),
            });
            harness.event(Event::RepeatInfo { rate: 0, delay: 0 });
            harness
        }

        fn keymap(&mut self, layout: &str, variant: &str) {
            let context = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
            let keymap = xkbcommon::xkb::Keymap::new_from_names(
                &context,
                "evdev",
                "pc105",
                layout,
                variant,
                Some(String::new()),
                xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
            .unwrap();
            self.keymap_text(keymap.get_as_string(xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1));
        }

        fn keymap_text(&mut self, text: String) {
            let mut bytes = text.into_bytes();
            bytes.push(0);
            let file = anonymous_keymap(&bytes);
            self.event(Event::Keymap {
                format: WEnum::Value(KeymapFormat::XkbV1),
                fd: file.into(),
                size: bytes.len().try_into().unwrap(),
            });
        }

        fn event(&mut self, event: Event) {
            let conn = self.app.conn.clone();
            let qh = self.app.qh.clone();
            self.data
                .event(&mut self.app, &self.keyboard, event, &conn, &qh);
            self.frame();
        }

        fn frame(&mut self) {
            for event in self.app.take_wayland_events() {
                match event {
                    WaylandEvent::KeyboardEnter(..) => self.input.handle_keyboard_enter(),
                    WaylandEvent::KeyboardLeave(..) => self.input.handle_keyboard_leave(),
                    WaylandEvent::KeyPress(event) => {
                        self.input.handle_keyboard_event(&event, true, false)
                    }
                    WaylandEvent::KeyRelease(event) => {
                        self.input.handle_keyboard_event(&event, false, false)
                    }
                    WaylandEvent::KeyRepeat(event) => {
                        self.repeats += 1;
                        self.repeat_events.push(event.clone());
                        self.input.handle_keyboard_event(&event, true, true)
                    }
                    WaylandEvent::ModifiersChanged(modifiers) => {
                        self.input.update_modifiers(&modifiers)
                    }
                    _ => {}
                }
            }
            let output = self.context.run_ui(self.input.take_raw_input(), |ui| {
                ui.text_edit_singleline(&mut self.text).request_focus();
            });
            output.drop_without_applying_deltas(); // CPU-only widget test
        }

        fn client_repeat(&mut self) {
            for event in self.app.keyboard_repeat.dispatch() {
                self.app.push_wayland_event(WaylandEvent::KeyRepeat(event));
            }
            self.frame();
        }

        fn repeat_texts(&self) -> Vec<Option<&str>> {
            self.repeat_events
                .iter()
                .map(|event| event.utf8.as_deref())
                .collect()
        }

        fn key(&mut self, key: u32, state: KeyState) {
            self.event(Event::Key {
                serial: 1,
                time: 1,
                key,
                state: WEnum::Value(state),
            });
        }

        fn modifiers(&mut self, depressed: u32, locked: u32) {
            self.event(Event::Modifiers {
                serial: 1,
                mods_depressed: depressed,
                mods_latched: 0,
                mods_locked: locked,
                group: 0,
            });
        }
    }

    fn anonymous_keymap(bytes: &[u8]) -> std::fs::File {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "wayapp-callback-keymap-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let mut file = std::fs::File::create_new(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        file.write_all(bytes).unwrap();
        file
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    #[cfg(feature = "clipboard")]
    fn unavailable_clipboard_preserves_unicode_text_and_selection() {
        let mut harness = KeyboardCallbacks::new("");
        // The private compositor has no selection owner. Confirm failure rather
        // than silently testing a successful paste of an empty string.
        // SAFETY: this clipboard drops before harness's retained connection.
        let clipboard =
            unsafe { Clipboard::new(harness.app.conn.display().id().as_ptr() as *mut _) };
        assert!(
            clipboard.load().is_err(),
            "fixture unexpectedly has a selection owner"
        );
        harness.text = "Grüße 保存 🦀".into();
        harness.frame();
        harness.modifiers(1 << 2, 0);
        harness.key(30, KeyState::Pressed); // Ctrl+A selects Unicode text.
        harness.key(30, KeyState::Released);
        for _ in 0..2 {
            harness.key(47, KeyState::Pressed); // Ctrl+V: clipboard has no offer.
            harness.key(47, KeyState::Released);
            assert_eq!(harness.text, "Grüße 保存 🦀");
        }
        harness.modifiers(0, 0);
        harness.key(48, KeyState::Pressed);
        harness.key(48, KeyState::Released);
        assert_eq!(
            harness.text, "b",
            "failed paste lost the existing selection"
        );
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn client_repeat_extreme_rates_and_long_delays_do_not_replay_stalled_bursts() {
        for rate in [1, i32::MAX] {
            let mut harness = KeyboardCallbacks::new("");
            harness.event(Event::RepeatInfo { rate, delay: 0 });
            harness.key(30, KeyState::Pressed);
            std::thread::sleep(std::time::Duration::from_millis(20));
            harness.client_repeat();
            assert_eq!(
                harness.repeats, 1,
                "stalled dispatch replayed a burst at rate {rate}"
            );
            assert_eq!(harness.text, "aa");
            harness.key(30, KeyState::Released);
            harness.client_repeat();
            assert_eq!(harness.repeats, 1, "release failed at rate {rate}");
        }
        let mut harness = KeyboardCallbacks::new("");
        harness.event(Event::RepeatInfo {
            rate: i32::MAX,
            delay: i32::MAX,
        });
        harness.key(30, KeyState::Pressed);
        harness.client_repeat();
        assert_eq!(
            harness.repeats, 0,
            "long compositor delay overflowed or fired early"
        );
        harness.event(Event::Leave {
            serial: 2,
            surface: harness.surface.clone(),
        });
        harness.client_repeat();
        assert_eq!(harness.repeats, 0, "long-delay key survived focus loss");
        assert_eq!(harness.text, "a");
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn compositor_repeat_types_using_current_modifiers() {
        let mut harness = KeyboardCallbacks::new("");
        harness.key(30, KeyState::Pressed);
        harness.key(30, KeyState::Repeated);
        assert_eq!(harness.text, "aa");
        harness.modifiers(1, 0); // Shift is changed while A remains held.
        harness.key(30, KeyState::Repeated);
        assert_eq!(
            harness.text, "aaA",
            "server repeat lost text after a modifier change"
        );
        assert_eq!(harness.repeat_texts(), [Some("a"), Some("A")]);
        assert!(
            harness.app.keyboard_repeat.dispatch().is_empty(),
            "server mode also scheduled client repeats"
        );
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn server_repeat_for_enter_held_key_needs_no_synthetic_press_or_old_composition() {
        let mut harness = KeyboardCallbacks::new("intl");
        harness.key(40, KeyState::Pressed);
        harness.key(40, KeyState::Released);
        harness.key(18, KeyState::Pressed);
        assert_eq!(harness.text, "é");
        harness.event(Event::Leave {
            serial: 2,
            surface: harness.surface.clone(),
        });
        harness.event(Event::Enter {
            serial: 3,
            surface: harness.surface.clone(),
            keys: 18_u32.to_ne_bytes().to_vec(),
        });
        harness.modifiers(0, 0);
        assert_eq!(harness.text, "é", "enter synthesized a press");
        harness.key(18, KeyState::Repeated);
        assert_eq!(harness.repeats, 1, "valid enter-held repeat was discarded");
        assert_eq!(
            harness.repeat_events.last().unwrap().utf8.as_deref(),
            Some("e")
        );
        assert_eq!(
            harness.text, "ée",
            "adapter dropped the first post-enter repeat"
        );
        harness.modifiers(1, 0);
        harness.key(18, KeyState::Repeated);
        assert_eq!(
            harness.repeat_events.last().unwrap().utf8.as_deref(),
            Some("E")
        );
        assert_eq!(harness.text, "éeE");
        harness.modifiers(1 << 2, 0);
        harness.key(18, KeyState::Repeated);
        assert_eq!(harness.repeat_events.last().unwrap().keysym, Keysym::e);
        assert_eq!(
            harness.repeat_events.last().unwrap().utf8.as_deref(),
            Some("\u{5}")
        );
        assert_eq!(harness.text, "éeE", "Control repeat inserted text");
        harness.key(18, KeyState::Released);
        assert!(!harness.context.input(|input| input.key_down(egui::Key::E)));
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn compositor_repeat_preserves_composed_text_for_each_pressed_key() {
        let mut harness = KeyboardCallbacks::new("intl");
        harness.key(40, KeyState::Pressed);
        harness.key(40, KeyState::Released);
        harness.key(18, KeyState::Pressed); // é
        harness.key(18, KeyState::Repeated);
        harness.modifiers(0, 1 << 4); // Num Lock changes no interpretation of E.
        harness.key(18, KeyState::Repeated);
        assert_eq!(harness.text, "ééé");
        harness.key(48, KeyState::Pressed); // A second held key must not steal E's text cache.
        harness.key(18, KeyState::Repeated);
        harness.key(48, KeyState::Released);
        harness.key(18, KeyState::Repeated);
        assert_eq!(harness.text, "ééébéé");
        assert_eq!(harness.repeat_texts(), [Some("é"); 4]);
        harness.key(18, KeyState::Released);
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn compositor_repeat_invalidates_composition_only_when_the_key_interpretation_changes() {
        let mut harness = KeyboardCallbacks::new("intl");
        harness.key(40, KeyState::Pressed);
        harness.key(40, KeyState::Released);
        harness.key(18, KeyState::Pressed);
        harness.modifiers(1, 0); // Shift changes E to uppercase and ends the composed repeat.
        harness.key(18, KeyState::Repeated);
        harness.modifiers(0, 0);
        harness.key(18, KeyState::Repeated);
        assert_eq!(
            harness.text, "éEe",
            "a meaningful modifier change reused stale composed text"
        );
        assert_eq!(harness.repeat_texts(), [Some("E"), Some("e")]);
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn compositor_repeat_survives_a_mode_switch() {
        let mut harness = KeyboardCallbacks::new("intl");
        harness.event(Event::RepeatInfo { rate: 25, delay: 0 });
        harness.key(40, KeyState::Pressed);
        harness.key(40, KeyState::Released);
        harness.key(18, KeyState::Pressed);
        assert_eq!(harness.text, "é");
        harness.client_repeat();
        assert_eq!(harness.text, "éé");
        assert_eq!(harness.repeats, 1);
        harness.event(Event::RepeatInfo { rate: 0, delay: 0 });
        harness.key(18, KeyState::Repeated);
        assert_eq!(
            harness.text, "ééé",
            "mode switch lost the held key's composed text"
        );
        assert!(harness.app.keyboard_repeat.dispatch().is_empty());
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn queued_server_repeat_does_not_target_a_locally_destroyed_surface() {
        let mut harness = KeyboardCallbacks::new("");
        harness.key(30, KeyState::Pressed);
        harness.key(30, KeyState::Repeated);
        let repeats = harness.repeats;
        harness.surface.destroy();
        harness.key(30, KeyState::Repeated);
        assert_eq!(
            harness.repeats, repeats,
            "destroyed surface generated a stale repeat"
        );
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn keyboard_removal_clears_widget_focus_and_modifiers() {
        for whole_seat in [false, true] {
            let mut harness = KeyboardCallbacks::new("");
            harness.modifiers(1 << 2, 0); // Control remains physically held at removal.
            assert!(harness.context.input(|input| input.modifiers.ctrl));
            let conn = harness.app.conn.clone();
            let qh = harness.app.qh.clone();
            let seat = harness.data.seat().clone();
            if whole_seat {
                harness.app.remove_seat(&conn, &qh, seat);
            } else {
                harness
                    .app
                    .remove_capability(&conn, &qh, seat, Capability::Keyboard);
            }
            // Drain application-generated cleanup through the normal widget adapter.
            harness.event(Event::RepeatInfo { rate: 0, delay: 0 });
            assert!(
                !harness.context.input(|input| input.raw.focused),
                "removed keyboard retained window focus"
            );
            assert!(
                !harness.context.input(|input| input.modifiers.ctrl),
                "removed keyboard retained Control"
            );
            assert!(harness.app.keyboard_focused_surface.is_none());
            assert!(!harness.app.keyboard_modifiers.ctrl);
            // Native callbacks can already be queued when the capability/seat
            // is removed. The retired proxy must not restore input ownership.
            harness.modifiers(1 << 2, 0);
            assert!(
                !harness.context.input(|input| input.modifiers.ctrl),
                "retired keyboard callback restored Control"
            );
            harness.event(Event::Enter {
                serial: 9,
                surface: harness.surface.clone(),
                keys: Vec::new(),
            });
            assert!(
                harness.app.keyboard_focused_surface.is_none(),
                "retired keyboard callback restored focus"
            );
            harness.modifiers(0, 0);
            harness.key(30, KeyState::Pressed);
            harness.key(30, KeyState::Released);
            assert!(
                harness.text.is_empty(),
                "retired keyboard typed into the widget"
            );
        }
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn keyboard_focus_loss_clears_modifiers_in_other_unfocused_views() {
        fn fanout(
            harness: &mut KeyboardCallbacks,
            other: &mut WaylandToEguiInput,
            context: &egui::Context,
        ) {
            let events = harness.app.take_wayland_events();
            for event in events {
                if let WaylandEvent::ModifiersChanged(modifiers) = &event {
                    other.update_modifiers(modifiers);
                }
                harness.app.push_wayland_event(event);
            }
            harness.frame();
            context
                .run_ui(other.take_raw_input(), |_| {})
                .drop_without_applying_deltas();
        }
        for loss in ["leave", "capability", "seat", "destroy", "leave-destroyed"] {
            let mut harness = KeyboardCallbacks::new("");
            // SAFETY: the second adapter drops before harness's display owner.
            #[cfg(feature = "clipboard")]
            let clipboard =
                unsafe { Clipboard::new(harness.app.conn.display().id().as_ptr() as *mut _) };
            #[cfg(feature = "clipboard")]
            let mut other = WaylandToEguiInput::new(clipboard);
            #[cfg(not(feature = "clipboard"))]
            let mut other = WaylandToEguiInput::new();
            let context = egui::Context::default();
            let conn = harness.app.conn.clone();
            let qh = harness.app.qh.clone();
            harness.data.event(
                &mut harness.app,
                &harness.keyboard,
                Event::Modifiers {
                    serial: 2,
                    mods_depressed: 1 << 2,
                    mods_latched: 0,
                    mods_locked: 0,
                    group: 0,
                },
                &conn,
                &qh,
            );
            fanout(&mut harness, &mut other, &context);
            assert!(context.input(|input| input.modifiers.ctrl && !input.raw.focused));
            let seat = harness.data.seat().clone();
            match loss {
                "leave" => harness.data.event(
                    &mut harness.app,
                    &harness.keyboard,
                    Event::Leave {
                        serial: 3,
                        surface: harness.surface.clone(),
                    },
                    &conn,
                    &qh,
                ),
                "capability" => {
                    harness
                        .app
                        .remove_capability(&conn, &qh, seat, Capability::Keyboard)
                }
                "seat" => harness.app.remove_seat(&conn, &qh, seat),
                "destroy" => {
                    harness.surface.destroy();
                    assert!(
                        !harness.app.current_keyboard_modifiers().ctrl,
                        "constructor metadata retained a dead focus owner's Control"
                    );
                    for event in harness.app.dispatch_pending(DispatchToken::external()) {
                        harness.app.push_wayland_event(event);
                    }
                }
                "leave-destroyed" => {
                    harness.surface.destroy();
                    harness.data.event(
                        &mut harness.app,
                        &harness.keyboard,
                        Event::Leave {
                            serial: 3,
                            surface: harness.surface.clone(),
                        },
                        &conn,
                        &qh,
                    );
                }
                _ => unreachable!(),
            }
            fanout(&mut harness, &mut other, &context);
            assert!(
                context.input(|input| !input.modifiers.ctrl && !input.raw.focused),
                "{loss} left stale Control in another unfocused view"
            );
        }
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn control_shortcut_on_an_international_key_does_not_insert_text() {
        for shifted in [false, true] {
            let mut harness = KeyboardCallbacks::new("");
            harness.keymap("de", "");
            harness.modifiers((1 << 2) | u32::from(shifted), 0);
            harness.key(39, KeyState::Pressed); // German ö, not ASCII control text.
            harness.key(39, KeyState::Released);
            assert!(
                harness.text.is_empty(),
                "Control shortcut inserted {:?}",
                harness.text
            );
            harness.modifiers(0, 0);
            harness.key(39, KeyState::Pressed);
            harness.key(39, KeyState::Released);
            assert_eq!(
                harness.text, "ö",
                "normal international text did not recover"
            );
        }
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn control_alt_x_does_not_cut_selected_text_or_disable_normal_cut() {
        for shifted in [false, true] {
            let mut harness = KeyboardCallbacks::new("");
            harness.text = "protected é中🦀".into();
            harness.frame();
            harness.modifiers(1 << 2, 0);
            harness.key(30, KeyState::Pressed); // Control+A selects all.
            harness.key(30, KeyState::Released);
            harness.modifiers((1 << 2) | (1 << 3) | u32::from(shifted), 0);
            harness.key(45, KeyState::Pressed); // Control+Alt+X is not Cut.
            harness.key(45, KeyState::Released);
            assert_eq!(
                harness.text, "protected é中🦀",
                "extra Alt modifier triggered Cut"
            );
            harness.modifiers(1 << 2, 0);
            harness.key(45, KeyState::Pressed);
            harness.key(45, KeyState::Released);
            assert!(harness.text.is_empty(), "ordinary Control+X no longer cuts");
        }
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn reentry_resets_modifiers_previously_reported_without_keyboard_focus() {
        let mut harness = KeyboardCallbacks::new("");
        harness.event(Event::Leave {
            serial: 2,
            surface: harness.surface.clone(),
        });
        harness.modifiers(1 << 2, 0);
        assert!(harness.app.current_keyboard_modifiers().ctrl);
        harness.event(Event::Enter {
            serial: 6,
            surface: harness.surface.clone(),
            keys: Vec::new(),
        });
        assert!(!harness.app.keyboard_modifiers.ctrl);
        harness.modifiers(0, 0);
        harness.key(30, KeyState::Pressed);
        harness.key(30, KeyState::Released);
        assert_eq!(
            harness.text, "a",
            "live keyboard did not recover after reentry"
        );
        assert!(
            harness
                .context
                .input(|input| input.raw.focused && !input.modifiers.ctrl)
        );
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn cached_focus_modifiers_reset_before_another_owner_enters() {
        let mut harness = KeyboardCallbacks::new("");
        let other = harness.app.compositor_state.create_surface(&harness.app.qh);
        harness.modifiers(1 << 2, 0);
        assert!(
            harness
                .app
                .keyboard_focus_modifiers(&harness.surface)
                .unwrap()
                .ctrl
        );
        assert!(harness.app.keyboard_focus_modifiers(&other).is_none());
        harness.event(Event::Leave {
            serial: 2,
            surface: harness.surface.clone(),
        });
        assert!(
            harness
                .app
                .keyboard_focus_modifiers(&harness.surface)
                .is_none()
        );
        assert!(!harness.app.keyboard_modifiers.ctrl);
        harness.event(Event::Enter {
            serial: 3,
            surface: other.clone(),
            keys: Vec::new(),
        });
        assert!(!harness.app.keyboard_focus_modifiers(&other).unwrap().ctrl);
        assert!(
            harness
                .app
                .keyboard_focus_modifiers(&harness.surface)
                .is_none()
        );
        harness.modifiers(1, 0); // New owner receives Shift, not old Control.
        let modifiers = harness.app.keyboard_focus_modifiers(&other).unwrap();
        assert!(modifiers.shift && !modifiers.ctrl);
    }

    #[test]
    #[ignore = "requires private headless runner; real caller-owned proxy, direct SDK callbacks"]
    fn caller_owned_live_keyboard_callbacks_forward_without_adoption() {
        let mut h = KeyboardCallbacks::new("");
        let seat = h.data.seat().clone();
        let conn = h.app.conn.clone();
        let qh = h.app.qh.clone();
        let external =
            seat.get_keyboard(&qh, KeyboardData::<Application, ()>::new(seat.clone(), ()));
        conn.roundtrip().unwrap();
        assert!(external.is_alive());
        assert!(!h.app.keyboard_repeat.contains_keyboard(&external));
        h.app.take_wayland_events();
        h.app.enter(&conn, &qh, &external, &h.surface, 1, &[], &[]);
        assert!(matches!(
            h.app.take_wayland_events().as_slice(),
            [WaylandEvent::KeyboardEnter(..)]
        ));
        h.app.update_modifiers(
            &conn,
            &qh,
            &external,
            2,
            KeyboardModifiers {
                shift: true,
                ..Default::default()
            },
            smithay_client_toolkit::seat::keyboard::RawModifiers {
                depressed: 1,
                ..Default::default()
            },
            0,
        );
        assert!(
            matches!(h.app.take_wayland_events().as_slice(), [WaylandEvent::ModifiersChanged(modifiers)] if modifiers.shift)
        );
        let event = KeyEvent {
            time: 3,
            raw_code: 18,
            keysym: Keysym::e,
            utf8: Some("é".into()),
        };
        h.app.press_key(&conn, &qh, &external, 3, event.clone());
        assert!(
            matches!(h.app.take_wayland_events().as_slice(), [WaylandEvent::KeyPress(event)] if event.utf8.as_deref() == Some("é"))
        );
        h.app.repeat_key(&conn, &qh, &external, 4, event.clone());
        assert!(
            matches!(h.app.take_wayland_events().as_slice(), [WaylandEvent::KeyRepeat(event)] if event.utf8.as_deref() == Some("é"))
        );
        h.app.release_key(&conn, &qh, &external, 5, event);
        assert!(matches!(
            h.app.take_wayland_events().as_slice(),
            [WaylandEvent::KeyRelease(_)]
        ));
        h.app.leave(&conn, &qh, &external, &h.surface, 6);
        assert!(
            h.app
                .take_wayland_events()
                .iter()
                .any(|event| matches!(event, WaylandEvent::KeyboardLeave(_)))
        );
        assert!(!h.app.keyboard_repeat.contains_keyboard(&external));
        h.app.remove_keyboard(&seat);
        assert!(
            external.is_alive(),
            "Application retired a caller-owned keyboard"
        );
        external.release();
        conn.roundtrip().unwrap();
    }

    #[test]
    #[ignore = "requires Wayland globals; drives legal unfocused SDK modifier callbacks"]
    fn modifiers_without_keyboard_focus_are_forwarded_without_claiming_focus() {
        let mut h = KeyboardCallbacks::new("");
        h.event(Event::Leave {
            serial: 1,
            surface: h.surface.clone(),
        });
        let conn = h.app.conn.clone();
        let qh = h.app.qh.clone();
        h.data.event(
            &mut h.app,
            &h.keyboard,
            Event::Modifiers {
                serial: 2,
                mods_depressed: 1 << 2,
                mods_latched: 0,
                mods_locked: 0,
                group: 0,
            },
            &conn,
            &qh,
        );
        assert!(
            matches!(h.app.take_wayland_events().as_slice(), [WaylandEvent::ModifiersChanged(modifiers)] if modifiers.ctrl)
        );
        assert!(h.app.current_keyboard_modifiers().ctrl);
        assert!(h.app.keyboard_focus_modifiers(&h.surface).is_none());
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn keymap_change_while_held_releases_the_key_that_was_actually_pressed() {
        let mut harness = KeyboardCallbacks::new("");
        harness.key(21, KeyState::Pressed); // Y in US.
        assert!(harness.context.input(|input| input.key_down(egui::Key::Y)));
        harness.keymap("de", "");
        harness.modifiers(0, 0);
        harness.key(21, KeyState::Released); // Now reported as Z.
        assert!(
            harness.context.input(|input| input.keys_down.is_empty()),
            "layout replacement left the old logical key down"
        );
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn logical_key_stays_down_until_all_physical_aliases_are_released() {
        let mut harness = KeyboardCallbacks::new("");
        harness.key(28, KeyState::Pressed); // Return
        harness.key(96, KeyState::Pressed); // Keypad Enter
        harness.key(28, KeyState::Released);
        assert!(
            harness
                .context
                .input(|input| input.key_down(egui::Key::Enter)),
            "releasing Return cleared a still-held keypad Enter"
        );
        harness.key(96, KeyState::Released);
        assert!(
            !harness
                .context
                .input(|input| input.key_down(egui::Key::Enter))
        );
    }

    fn check_repeat_after_keymap_change(client_mode: bool) {
        let mut harness = KeyboardCallbacks::new("");
        if client_mode {
            harness.event(Event::RepeatInfo { rate: 25, delay: 0 });
        }
        harness.key(21, KeyState::Pressed);
        assert_eq!(harness.text, "y");
        harness.keymap("de", "");
        harness.modifiers(0, 0);
        if client_mode {
            harness.client_repeat();
        } else {
            harness.key(21, KeyState::Repeated);
        }
        assert_eq!(
            harness.text, "yz",
            "keymap replacement stopped a logically held key's repetition"
        );
        assert!(!harness.context.input(|input| input.key_down(egui::Key::Y)));
        assert!(harness.context.input(|input| input.key_down(egui::Key::Z)));
        harness.key(21, KeyState::Released);
        assert!(harness.context.input(|input| input.keys_down.is_empty()));
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn keymap_change_keeps_server_repetition_for_logically_pressed_keys() {
        check_repeat_after_keymap_change(false);
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn keymap_change_keeps_client_repetition_for_logically_pressed_keys() {
        check_repeat_after_keymap_change(true);
    }

    #[test]
    #[ignore = "requires Wayland globals; drives SDK callbacks, not compositor input"]
    fn keymap_can_disable_repetition_without_releasing_a_held_key() {
        let mut harness = KeyboardCallbacks::new("");
        harness.event(Event::RepeatInfo { rate: 25, delay: 0 });
        harness.key(21, KeyState::Pressed);
        harness.keymap_text(
            r#"xkb_keymap {
                xkb_keycodes { include "evdev+aliases(qwerty)" };
                xkb_types { include "complete" };
                xkb_compatibility { include "complete" };
                xkb_symbols {
                    include "pc+us+inet(evdev)"
                    key <AD06> { repeat=no };
                };
            };"#
            .into(),
        );
        harness.modifiers(0, 0);
        harness.client_repeat();
        assert_eq!(harness.text, "y", "new keymap's no-repeat flag was ignored");
        assert!(harness.context.input(|input| input.key_down(egui::Key::Y)));
        harness.key(21, KeyState::Released);
        assert!(harness.context.input(|input| input.keys_down.is_empty()));
    }
}

#[cfg(test)]
mod legacy_input_tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsFd;
    use wayland_client::delegate_noop;
    use wayland_client::globals::GlobalListContents;
    use smithay_client_toolkit::reexports::protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
    use smithay_client_toolkit::reexports::protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
    use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
    use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

    pub(super) struct LegacyInput;
    impl Dispatch<wayland_client::protocol::wl_registry::WlRegistry, GlobalListContents>
        for LegacyInput
    {
        fn event(
            _: &mut Self,
            _: &wayland_client::protocol::wl_registry::WlRegistry,
            _: wayland_client::protocol::wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    delegate_noop!(LegacyInput: ignore wl_seat::WlSeat);
    delegate_noop!(LegacyInput: ignore WlKeyboard);
    delegate_noop!(LegacyInput: ignore WlPointer);
    delegate_noop!(LegacyInput: ZwpVirtualKeyboardManagerV1);
    delegate_noop!(LegacyInput: ZwpVirtualKeyboardV1);
    delegate_noop!(LegacyInput: ZwlrVirtualPointerManagerV1);
    delegate_noop!(LegacyInput: ZwlrVirtualPointerV1);

    #[test]
    #[ignore = "requires private headless runner and virtual input protocols; never sends \
                keys/motion"]
    fn version_two_input_removal_destroys_local_proxies_without_release_requests() {
        let _guard = crate::test_support::private_input_test_guard();
        let mut app = Application::new(|_| {});
        let (globals, mut queue) = registry_queue_init::<LegacyInput>(&app.conn).unwrap();
        let qh = queue.handle();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 2..=2, ()).unwrap();
        let keyboard_manager: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).unwrap();
        let pointer_manager: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).unwrap();
        let virtual_keyboard = keyboard_manager.create_virtual_keyboard(&seat, &qh, ());
        let context = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
        let keymap = xkbcommon::xkb::Keymap::new_from_names(
            &context,
            "evdev",
            "pc105",
            "us",
            "",
            None,
            xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .unwrap();
        let mut bytes = keymap
            .get_as_string(xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1)
            .into_bytes();
        bytes.push(0);
        let path =
            std::env::temp_dir().join(format!("wayapp-legacy-input-keymap-{}", std::process::id()));
        let mut file = std::fs::File::create_new(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        file.write_all(&bytes).unwrap();
        virtual_keyboard.keymap(1, file.as_fd(), bytes.len().try_into().unwrap());
        let virtual_pointer = pointer_manager.create_virtual_pointer(Some(&seat), &qh, ());
        queue.roundtrip(&mut LegacyInput).unwrap();
        // These devices merely enable capabilities on the private seat. No
        // keys, pointer motion, selection changes or screenshots are requested.
        let keyboard = seat.get_keyboard(&qh, ());
        let pointer = seat.get_pointer(&qh, ());
        queue.roundtrip(&mut LegacyInput).unwrap();
        assert_eq!(keyboard.version(), 2);
        assert_eq!(pointer.version(), 2);
        assert!(keyboard.is_alive() && pointer.is_alive());
        app.keyboard_repeat.add_keyboard(&seat, keyboard.clone());
        app.track_pointer(&seat, pointer.clone());
        app.remove_keyboard(&seat);
        app.remove_pointers(&seat);
        assert!(
            !keyboard.is_alive(),
            "retired v2 keyboard retained local protocol state"
        );
        assert!(
            !pointer.is_alive(),
            "retired v2 pointer retained local protocol state"
        );
        queue
            .roundtrip(&mut LegacyInput)
            .expect("cleanup sent an unsupported release request");
        virtual_pointer.destroy();
        virtual_keyboard.destroy();
        queue.roundtrip(&mut LegacyInput).unwrap();
    }
}

#[cfg(test)]
mod pointer_cleanup_tests {
    use super::legacy_input_tests::LegacyInput;
    use super::*;
    use wayland_client::EventQueue;
    use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
    use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

    struct PointerFixture {
        device: ZwlrVirtualPointerV1,
        manager: ZwlrVirtualPointerManagerV1,
        _queue: EventQueue<LegacyInput>,
        connection: Connection,
    }

    impl Drop for PointerFixture {
        fn drop(&mut self) {
            self.device.destroy();
            self.manager.destroy();
            let _ = self.connection.flush();
        }
    }

    fn live_pointer(app: &mut Application) -> (PointerFixture, wl_seat::WlSeat, WlPointer) {
        crate::test_support::require_private_compositor();
        let (globals, mut queue) = registry_queue_init::<LegacyInput>(&app.conn).unwrap();
        let manager: ZwlrVirtualPointerManagerV1 =
            globals.bind(&queue.handle(), 1..=2, ()).unwrap();
        let seat = app.seat_state.seats().next().unwrap();
        // Enable only the private seat capability. All pointer callbacks below
        // are driven directly; this fixture sends no motion/buttons to Sway.
        let device = manager.create_virtual_pointer(Some(&seat), &queue.handle(), ());
        queue.roundtrip(&mut LegacyInput).unwrap();
        let mut app_queue = app.event_queue.take().unwrap();
        app_queue.roundtrip(app).unwrap();
        app_queue.roundtrip(app).unwrap();
        app.event_queue = Some(app_queue);
        let pointer = app
            .pointers
            .values()
            .find(|tracked| tracked.seat == seat.id())
            .expect("private capability did not register a native pointer")
            .pointer
            .clone();
        assert!(pointer.is_alive());
        app.take_wayland_events(); // Discard initial output/seat metadata only.
        (
            PointerFixture {
                device,
                manager,
                _queue: queue,
                connection: app.conn.clone(),
            },
            seat,
            pointer,
        )
    }

    #[test]
    #[ignore = "requires private headless runner; real proxy, direct callbacks without pointer \
                injection"]
    fn dropping_application_releases_owned_pointer_and_cursor_devices() {
        let _guard = crate::test_support::private_input_test_guard();
        let mut app = Application::new(|_| {});
        let (_fixture, seat, pointer) = live_pointer(&mut app);
        let external = app.seat_state.get_pointer(&app.qh, &seat).unwrap();
        let managed_device = app.cursor_shape_manager.get_shape_device(&pointer, &app.qh);
        let external_device = app
            .cursor_shape_manager
            .get_shape_device(&external, &app.qh);
        // Cache exactly the resources created by set_cursor, without injecting
        // input or using an invented native enter serial to change the cursor.
        app.pointer_shape_devices
            .insert(pointer.id(), OwnedCursorShapeDevice(managed_device.clone()));
        app.pointer_shape_devices.insert(
            external.id(),
            OwnedCursorShapeDevice(external_device.clone()),
        );
        let connection = app.conn.clone();
        connection.roundtrip().unwrap();
        drop(app);
        assert_eq!(
            (
                pointer.is_alive(),
                managed_device.is_alive(),
                external_device.is_alive()
            ),
            (false, false, false),
            "Application drop retained owned pointer/cursor resources"
        );
        assert!(
            external.is_alive(),
            "Application released a caller-owned pointer"
        );
        connection
            .roundtrip()
            .expect("pointer/cursor cleanup damaged the display");
        external.release();
    }

    #[test]
    #[ignore = "requires private headless runner; real proxy, direct callbacks without pointer \
                injection"]
    fn externally_created_live_pointer_is_forwarded_without_becoming_owned() {
        let _guard = crate::test_support::private_input_test_guard();
        let mut app = Application::new(|_| {});
        let (_fixture, seat, primary) = live_pointer(&mut app);
        let external = app.seat_state.get_pointer(&app.qh, &seat).unwrap();
        let surface = app.compositor_state.create_surface(&app.qh);
        app.pointer_frame(
            &app.conn.clone(),
            &app.qh.clone(),
            &external,
            &[PointerEvent {
                surface: surface.clone(),
                position: (24.0, 24.0),
                kind: PointerEventKind::Enter { serial: 7 },
            }],
        );
        assert!(matches!(
            app.take_wayland_events().as_slice(),
            [WaylandEvent::PointerEvent((
                _,
                (24.0, 24.0),
                PointerEventKind::Enter { serial: 7 }
            ))]
        ));
        assert!(!app.pointers.contains_key(&external.id()));
        assert!(app.pointers.contains_key(&primary.id()) && primary.is_alive());
        external.release(); // Its creator, not Application, owns this proxy.
    }

    #[test]
    #[ignore = "requires private headless runner; real proxy, direct callbacks without pointer \
                injection"]
    fn pointer_position_cache_tracks_motion_and_clears_on_leave() {
        let _guard = crate::test_support::private_input_test_guard();
        let mut app = Application::new(|_| {});
        let (_fixture, _seat, pointer) = live_pointer(&mut app);
        let surface = app.compositor_state.create_surface(&app.qh);
        let other = app.compositor_state.create_surface(&app.qh);
        let conn = app.conn.clone();
        let qh = app.qh.clone();
        for (kind, position, expected) in [
            (
                PointerEventKind::Enter { serial: 1 },
                (24.0, 24.0),
                Some((24.0, 24.0)),
            ),
            (
                PointerEventKind::Motion { time: 1 },
                (70.0, 50.0),
                Some((70.0, 50.0)),
            ),
            (PointerEventKind::Leave { serial: 2 }, (70.0, 50.0), None),
        ] {
            app.pointer_frame(
                &conn,
                &qh,
                &pointer,
                &[PointerEvent {
                    surface: surface.clone(),
                    position,
                    kind,
                }],
            );
            assert_eq!(app.pointer_focus_position(&surface), expected);
            assert_eq!(app.pointer_focus_position(&other), None);
        }
        app.pointer_frame(
            &conn,
            &qh,
            &pointer,
            &[PointerEvent {
                surface: other.clone(),
                position: (9.0, 8.0),
                kind: PointerEventKind::Enter { serial: 3 },
            }],
        );
        assert_eq!(app.pointer_focus_position(&other), Some((9.0, 8.0)));
        assert_eq!(app.pointer_focus_position(&surface), None);
    }

    #[test]
    #[ignore = "requires private headless runner; real proxy, direct callbacks without pointer \
                injection"]
    fn destroyed_pointer_surface_clears_cached_ownership_without_retiring_device() {
        let _guard = crate::test_support::private_input_test_guard();
        let mut app = Application::new(|_| {});
        let (_fixture, _seat, pointer) = live_pointer(&mut app);
        let surface = app.compositor_state.create_surface(&app.qh);
        let conn = app.conn.clone();
        let qh = app.qh.clone();
        for kind in [
            PointerEventKind::Enter { serial: 1 },
            PointerEventKind::Press {
                serial: 2,
                time: 1,
                button: 0x110,
            },
        ] {
            app.pointer_frame(
                &conn,
                &qh,
                &pointer,
                &[PointerEvent {
                    surface: surface.clone(),
                    position: (24.0, 24.0),
                    kind,
                }],
            );
        }
        app.take_wayland_events();
        surface.destroy();
        app.hide_cursor();
        app.set_cursor(Shape::Default);
        assert!(
            app.pointer_shape_devices.is_empty(),
            "dead focus created a cursor device"
        );
        assert!(app.dispatch_pending(DispatchToken::external()).is_empty());
        assert!(pointer.is_alive());
        let tracked = app.pointers.get(&pointer.id()).unwrap();
        assert!(tracked.focus.is_none());
        assert!(tracked.buttons.is_empty());
        assert!(app.last_pointer.is_none());
        assert!(app.last_pointer_enter_serial.is_none());
        assert!(app.pointer_position.is_none());
        assert!(app.pointer_focused_surface.is_none());
        for kind in [
            PointerEventKind::Enter { serial: 3 },
            PointerEventKind::Press {
                serial: 4,
                time: 2,
                button: 0x110,
            },
            PointerEventKind::Leave { serial: 5 },
        ] {
            app.pointer_frame(
                &conn,
                &qh,
                &pointer,
                &[PointerEvent {
                    surface: surface.clone(),
                    position: (24.0, 24.0),
                    kind,
                }],
            );
        }
        assert!(
            app.take_wayland_events().is_empty(),
            "dead surface callback was forwarded"
        );
        assert!(app.last_pointer.is_none());
        assert!(app.pointers.get(&pointer.id()).unwrap().buttons.is_empty());
        let replacement = app.compositor_state.create_surface(&app.qh);
        app.pointer_frame(
            &conn,
            &qh,
            &pointer,
            &[PointerEvent {
                surface: replacement.clone(),
                position: (8.0, 9.0),
                kind: PointerEventKind::Enter { serial: 6 },
            }],
        );
        assert_eq!(app.pointer_focus_position(&replacement), Some((8.0, 9.0)));
        assert_eq!(app.take_wayland_events().len(), 1);
        conn.roundtrip().unwrap();
    }
}

#[cfg(test)]
mod pointer_owner_regression {
    use super::*;
    use crate::test_support as private;
    use wayland_client::delegate_noop;
    use wayland_client::globals::GlobalListContents;
    use wayland_client::protocol::wl_pointer;
    use wayland_client::protocol::wl_registry;
    use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
    use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

    struct Device;
    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Device {
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
    delegate_noop!(Device: ignore wl_seat::WlSeat);
    delegate_noop!(Device: ignore wl_pointer::WlPointer);
    delegate_noop!(Device: ZwlrVirtualPointerManagerV1);
    delegate_noop!(Device: ZwlrVirtualPointerV1);

    #[derive(Clone, Copy)]
    enum Retirement {
        Application,
        Capability,
        Seat,
    }

    fn check_retirement(retirement: Retirement) {
        use smithay_client_toolkit::shell::wlr_layer::Anchor;
        use smithay_client_toolkit::shell::wlr_layer::Layer;
        let _guard = private::private_input_test_guard();
        let _display_guard;
        let mut app = Application::new(|_| {});
        _display_guard = app.conn.clone();
        let (globals, mut device_queue) = registry_queue_init::<Device>(&app.conn).unwrap();
        let qh = device_queue.handle();
        let manager: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).unwrap();
        let virtual_pointer = manager.create_virtual_pointer(None, &qh, ());
        device_queue.roundtrip(&mut Device).unwrap();
        let mut queue = app.event_queue.take().unwrap();
        queue.roundtrip(&mut app).unwrap();
        queue.roundtrip(&mut app).unwrap();
        let seat = app.seat_state.seats().next().unwrap();
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            Layer::Top,
            Some("wayapp-pointer-owner-test"),
            None,
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_size(0, 0);
        layer.commit();
        let mut color = crate::SingleColorState::new(layer.clone(), (0, 0, 0), 800, 600);
        queue.roundtrip(&mut app).unwrap();
        let events = app.take_wayland_events();
        color.handle_events(&app, &events);
        queue.roundtrip(&mut app).unwrap();
        // The managed handle is observed through normal native Enter delivery,
        // not by inspecting fields introduced by the ownership fix.
        virtual_pointer.motion_absolute(1, 399, 300, 800, 600);
        virtual_pointer.frame();
        virtual_pointer.motion_absolute(2, 400, 300, 800, 600);
        virtual_pointer.frame();
        device_queue.roundtrip(&mut Device).unwrap();
        queue.roundtrip(&mut app).unwrap();
        let pointer = app
            .last_pointer
            .clone()
            .expect("native Enter did not reach Application");
        assert!(pointer.is_alive());
        let external = app.seat_state.get_pointer(&app.qh, &seat).unwrap();
        let cursor = if matches!(retirement, Retirement::Application) {
            None // Cursor-cache Application Drop is the independent 0020 issue.
        } else {
            app.set_cursor(Shape::Default);
            Some(
                app.pointer_shape_devices
                    .get(&pointer.id())
                    .unwrap()
                    .0
                    .clone(),
            )
        };
        let connection = app.conn.clone();
        app.event_queue = Some(queue);
        match retirement {
            Retirement::Application => drop(app),
            Retirement::Capability => {
                let qh = app.qh.clone();
                app.remove_capability(&connection, &qh, seat, Capability::Pointer);
                assert!(app.last_pointer.is_none() && app.last_pointer_enter_serial.is_none());
                app.set_cursor(Shape::Default); // No retired enter serial may be reused.
            }
            Retirement::Seat => {
                let qh = app.qh.clone();
                app.remove_seat(&connection, &qh, seat);
                assert!(app.last_pointer.is_none() && app.last_pointer_enter_serial.is_none());
                app.set_cursor(Shape::Default);
            }
        }
        assert!(
            !pointer.is_alive(),
            "Application retained its retired pointer"
        );
        assert!(
            external.is_alive(),
            "Application released a caller-owned pointer"
        );
        if let Some(cursor) = cursor {
            assert!(
                !cursor.is_alive(),
                "retired pointer retained its cursor device"
            );
        }
        connection
            .roundtrip()
            .expect("pointer retirement damaged the display");
        external.release();
        virtual_pointer.destroy();
        manager.destroy();
        device_queue.roundtrip(&mut Device).unwrap();
    }

    #[test]
    #[ignore = "requires private headless runner; native virtual-pointer motion, no capture/GPU"]
    fn application_drop_releases_its_managed_pointer() {
        check_retirement(Retirement::Application);
    }

    #[test]
    #[ignore = "requires private headless runner; native motion, controlled capability callback"]
    fn capability_removal_releases_its_managed_pointer() {
        check_retirement(Retirement::Capability);
    }

    #[test]
    #[ignore = "requires private headless runner; native motion, controlled seat callback"]
    fn seat_removal_releases_its_managed_pointer() {
        check_retirement(Retirement::Seat);
    }
}

#[cfg(test)]
mod keyboard_owner_regression {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::AsFd;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use wayland_client::{delegate_noop, globals::GlobalListContents, protocol::wl_registry};
    use smithay_client_toolkit::reexports::protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
    use smithay_client_toolkit::reexports::protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
    use crate::test_support as private;

    struct Device;
    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Device {
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
    delegate_noop!(Device: ignore wl_seat::WlSeat);
    delegate_noop!(Device: ZwpVirtualKeyboardManagerV1);
    delegate_noop!(Device: ZwpVirtualKeyboardV1);

    const BEFORE_EXTERNAL: &str = "WAYAPP_KEYBOARD_OWNER_BEFORE_EXTERNAL";
    const AFTER_RETIREMENT: &str = "WAYAPP_KEYBOARD_OWNER_AFTER_RETIREMENT";
    const CHILD: &str = "WAYAPP_KEYBOARD_OWNER_CHILD";

    fn child(retirement: &str) {
        private::require_private_compositor();
        let device_connection = Connection::connect_to_env().unwrap();
        let (globals, mut devices) = registry_queue_init::<Device>(&device_connection).unwrap();
        let qh = devices.handle();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=7, ()).unwrap();
        let manager: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).unwrap();
        let virtual_keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
        let compiled = Command::new("xkbcli")
            .args(["compile-keymap", "--layout", "us"])
            .output()
            .unwrap();
        assert!(compiled.status.success());
        let mut bytes = compiled.stdout;
        bytes.push(0);
        let path = std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap())
            .join(format!("keyboard-owner-{}.keymap", std::process::id()));
        let mut keymap = std::fs::File::create_new(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        keymap.write_all(&bytes).unwrap();
        virtual_keyboard.keymap(1, keymap.as_fd(), bytes.len().try_into().unwrap());
        devices.roundtrip(&mut Device).unwrap();

        let _display_guard;
        let mut app = Application::new(|_| {});
        _display_guard = app.conn.clone();
        let mut queue = app.event_queue.take().unwrap();
        queue.roundtrip(&mut app).unwrap();
        queue.roundtrip(&mut app).unwrap();
        app.event_queue = Some(queue);
        let seat = app.seat_state.seats().next().unwrap();
        eprintln!("WAYAPP_KEYBOARD_OWNER_SEAT={}", seat.id().protocol_id());
        eprintln!("{BEFORE_EXTERNAL}");
        let external = app.seat_state.get_keyboard(&app.qh, &seat, None).unwrap();
        let connection = app.conn.clone();
        match retirement {
            "application" => drop(app),
            "capability" => {
                let qh = app.qh.clone();
                app.remove_capability(&connection, &qh, seat, Capability::Keyboard);
            }
            "seat" => {
                let qh = app.qh.clone();
                app.remove_seat(&connection, &qh, seat);
            }
            _ => unreachable!(),
        }
        assert!(
            external.is_alive(),
            "Application released a caller-owned keyboard"
        );
        connection
            .roundtrip()
            .expect("keyboard retirement damaged the display");
        eprintln!("{AFTER_RETIREMENT}");
        external.release();
        virtual_keyboard.destroy();
        devices.roundtrip(&mut Device).unwrap();
    }

    fn check(retirement: &str) {
        if std::env::var_os(CHILD).is_some() {
            child(retirement);
            return;
        }
        let _guard = private::private_input_test_guard();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                std::thread::current().name().unwrap(),
                "--include-ignored",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("WAYLAND_DEBUG", "client")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let trace = std::thread::spawn(move || {
            let mut trace = String::new();
            stderr.read_to_string(&mut trace).unwrap();
            trace
        });
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("native keyboard ownership child timed out");
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let trace = trace.join().unwrap();
        assert!(status.success(), "ownership child failed: {trace}");
        let (owned_creation, rest) = trace
            .split_once(BEFORE_EXTERNAL)
            .expect("child did not reach external creation");
        let before_retirement = rest
            .split_once(AFTER_RETIREMENT)
            .expect("child did not finish retirement")
            .0;
        let seat = owned_creation
            .lines()
            .find_map(|line| line.strip_prefix("WAYAPP_KEYBOARD_OWNER_SEAT="))
            .unwrap();
        // smithay-clipboard has its own bound seat/keyboard on this display.
        // Match Application's exact seat object, not every get_keyboard trace.
        let keyboard_created = |line: &str| {
            let (head, tail) = line.split_once(".get_keyboard(new id ")?;
            (head.rsplit(['#', '@']).next() == Some(seat))
                .then(|| tail.split(')').next().unwrap().to_owned())
        };
        let keyboards: Vec<_> = owned_creation
            .lines()
            .filter_map(keyboard_created)
            .collect();
        assert_eq!(
            keyboards.len(),
            1,
            "Application seat {seat} must create exactly one owned keyboard: {keyboards:?}"
        );
        let release = format!("{}.release(", keyboards[0]);
        assert!(
            before_retirement.contains(&release),
            "managed keyboard had no native release on {retirement} (expected {release})"
        );
        let external = before_retirement
            .lines()
            .find_map(keyboard_created)
            .expect("caller-owned keyboard was not created");
        assert!(
            !before_retirement.contains(&format!("{external}.release(")),
            "Application released caller-owned keyboard"
        );
    }

    #[test]
    #[ignore = "requires private headless runner, xkbcli and native protocol tracing; no \
                keys/GPU/capture"]
    fn application_drop_releases_only_its_owned_keyboard() {
        check("application");
    }

    #[test]
    #[ignore = "requires private headless runner; controlled capability callback, no key injection"]
    fn capability_removal_releases_only_its_owned_keyboard() {
        check("capability");
    }

    #[test]
    #[ignore = "requires private headless runner; controlled seat callback, no key injection"]
    fn seat_removal_releases_only_its_owned_keyboard() {
        check("seat");
    }
}

#[cfg(test)]
mod composition_scope_regression {
    use super::*;
    use crate::SingleColorState;
    use smithay_client_toolkit::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerSurface};
    use std::io::Write;
    use std::os::fd::AsFd;
    use std::process::Command;
    use wayland_client::{delegate_noop, globals::GlobalListContents, protocol::wl_registry};
    use smithay_client_toolkit::reexports::protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
    use smithay_client_toolkit::reexports::protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
    use crate::test_support as private;

    struct Device;
    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Device {
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
    delegate_noop!(Device: ignore wl_seat::WlSeat);
    delegate_noop!(Device: ZwpVirtualKeyboardManagerV1);
    delegate_noop!(Device: ZwpVirtualKeyboardV1);

    struct Harness {
        color: SingleColorState<LayerSurface>,
        layer: LayerSurface,
        queue: EventQueue<Application>,
        app: Application,
        keyboard: ZwpVirtualKeyboardV1,
        devices: EventQueue<Device>,
        _device_connection: Connection,
        _display: Connection,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.keyboard.modifiers(0, 0, 0, 0);
            self.keyboard.destroy();
            let _ = self._device_connection.flush();
        }
    }

    impl Harness {
        fn new() -> Self {
            let guard = private::private_input_test_guard();
            let device_connection = Connection::connect_to_env().unwrap();
            let (globals, devices) = registry_queue_init::<Device>(&device_connection).unwrap();
            let qh = devices.handle();
            let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=7, ()).unwrap();
            let manager: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).unwrap();
            let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
            let mut app = Application::new(|_| {});
            let queue = app.event_queue.take().unwrap();
            let layer = app.layer_shell.create_layer_surface(
                &app.qh,
                app.compositor_state.create_surface(&app.qh),
                Layer::Top,
                Some("wayapp-composition-scope"),
                None,
            );
            layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
            layer.set_size(0, 0);
            layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
            layer.commit();
            let color = SingleColorState::new(layer.clone(), (0, 0, 0), 800, 600);
            let display = app.conn.clone();
            let mut result = Self {
                color,
                layer,
                queue,
                app,
                keyboard,
                devices,
                _device_connection: device_connection,
                _display: display,
                _guard: guard,
            };
            result.keymap("us", "intl");
            let events = result.pump();
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, WaylandEvent::KeyboardEnter(..))),
                "native keyboard focus did not reach the SHM layer"
            );
            result
        }

        fn pump(&mut self) -> Vec<WaylandEvent> {
            let mut events = Vec::new();
            for _ in 0..3 {
                self.queue.roundtrip(&mut self.app).unwrap();
                let batch = self.app.take_wayland_events();
                self.color.handle_events(&self.app, &batch);
                events.extend(batch);
            }
            events
        }

        fn keymap(&mut self, layout: &str, variant: &str) {
            let compiled = Command::new("xkbcli")
                .args([
                    "compile-keymap",
                    "--layout",
                    layout,
                    "--variant",
                    variant,
                    "--options",
                    "",
                ])
                .output()
                .unwrap();
            assert!(compiled.status.success());
            let mut bytes = compiled.stdout;
            bytes.push(0);
            let path = std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap())
                .join(format!("composition-scope-{}.keymap", std::process::id()));
            let mut keymap = std::fs::File::create_new(&path).unwrap();
            std::fs::remove_file(&path).unwrap();
            keymap.write_all(&bytes).unwrap();
            self.keyboard
                .keymap(1, keymap.as_fd(), bytes.len().try_into().unwrap());
            self.devices.roundtrip(&mut Device).unwrap();
        }

        fn tap(&mut self, key: u32) -> String {
            self.keyboard.key(1, key, 1);
            self.keyboard.key(2, key, 0);
            self.devices.roundtrip(&mut Device).unwrap();
            self.pump()
                .into_iter()
                .filter_map(|e| match e {
                    WaylandEvent::KeyPress(key) => Some(key.utf8.unwrap_or_default()),
                    _ => None,
                })
                .collect()
        }
    }

    #[test]
    #[ignore = "requires private headless runner; native virtual keyboard, SHM layer, no \
                GPU/capture"]
    fn unfinished_dead_key_does_not_cross_native_focus_change() {
        let mut h = Harness::new();
        assert_eq!(h.tap(40), "", "dead acute produced premature text");
        let other = h.app.layer_shell.create_layer_surface(
            &h.app.qh,
            h.app.compositor_state.create_surface(&h.app.qh),
            Layer::Overlay,
            Some("wayapp-composition-other-focus"),
            None,
        );
        other.set_size(64, 64);
        other.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        other.commit();
        let mut color = SingleColorState::new(other.clone(), (0, 0, 0), 64, 64);
        let mut switched = Vec::new();
        for _ in 0..3 {
            let events = h.pump();
            color.handle_events(&h.app, &events);
            switched.extend(events);
        }
        assert!(
            switched
                .iter()
                .any(|e| matches!(e, WaylandEvent::KeyboardLeave(s) if s == h.layer.wl_surface()))
        );
        assert!(
            switched.iter().any(
                |e| matches!(e, WaylandEvent::KeyboardEnter(s, _, _) if s == other.wl_surface())
            )
        );
        other.wl_surface().attach(None, 0, 0);
        other.commit();
        assert!(h.pump().iter().any(
            |e| matches!(e, WaylandEvent::KeyboardEnter(s, _, _) if s == h.layer.wl_surface())
        ));
        assert_eq!(
            h.tap(18),
            "e",
            "old focus's dead acute changed the next focus's text"
        );
        assert_eq!(h.tap(40), "");
        assert_eq!(h.tap(18), "é", "ordinary in-focus composition broke");
        assert_eq!(
            h.tap(30),
            "a",
            "completed composition leaked into the next key"
        );
    }

    #[test]
    #[ignore = "requires private headless runner; native keymap replacement, SHM layer, no \
                GPU/capture"]
    fn unfinished_dead_key_does_not_cross_native_keymap_change() {
        let mut h = Harness::new();
        assert_eq!(h.tap(40), "", "dead acute produced premature text");
        h.keymap("de", "");
        h.pump();
        assert_eq!(
            h.tap(18),
            "e",
            "old keymap's dead acute changed replacement-layout text"
        );
        assert_eq!(h.tap(39), "ö", "replacement keymap's Unicode text broke");
        h.keyboard.modifiers(1, 0, 0, 0);
        h.devices.roundtrip(&mut Device).unwrap();
        h.pump();
        assert_eq!(h.tap(40), "Ä", "replacement keymap modifiers were lost");
    }
}

#[cfg(test)]
mod pointer_cancel_regression {
    use super::legacy_input_tests::LegacyInput as Device;
    use super::*;
    use crate::EguiSurfaceState;
    use crate::test_support as private;
    use smithay_client_toolkit::shell::wlr_layer::Anchor;
    use smithay_client_toolkit::shell::wlr_layer::Layer;
    use smithay_client_toolkit::shell::wlr_layer::LayerSurface;
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::sync::mpsc::Receiver;
    use std::sync::mpsc::channel;
    use std::time::Duration;
    use std::time::Instant;
    use wayland_client::protocol::wl_pointer::ButtonState;
    use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
    use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

    struct VirtualPointer {
        pointer: ZwlrVirtualPointerV1,
        connection: Connection,
        _queue: EventQueue<Device>,
        pressed: Cell<bool>,
    }
    impl VirtualPointer {
        fn new() -> Self {
            private::require_private_compositor();
            let connection = Connection::connect_to_env().unwrap();
            let (globals, queue) = registry_queue_init::<Device>(&connection).unwrap();
            let manager: ZwlrVirtualPointerManagerV1 =
                globals.bind(&queue.handle(), 1..=2, ()).unwrap();
            let pointer = manager.create_virtual_pointer(None, &queue.handle(), ());
            connection.roundtrip().unwrap();
            Self {
                pointer,
                connection,
                _queue: queue,
                pressed: Cell::new(false),
            }
        }

        fn move_to(&self, x: u32) {
            self.pointer.motion_absolute(1, x - 1, 300, 800, 600);
            self.pointer.frame();
            self.pointer.motion_absolute(2, x, 300, 800, 600);
            self.pointer.frame();
            self.connection.roundtrip().unwrap();
        }

        fn press(&self) {
            self.pressed.set(true);
            self.pointer.button(3, 0x110, ButtonState::Pressed);
            self.pointer.frame();
            self.connection.roundtrip().unwrap();
        }
    }
    impl Drop for VirtualPointer {
        fn drop(&mut self) {
            if self.pressed.get() {
                self.pointer.button(4, 0x110, ButtonState::Released);
                self.pointer.frame();
            }
            self.pointer.destroy();
            let _ = self.connection.flush();
        }
    }

    fn pump(
        app: &mut Application,
        view: &mut EguiSurfaceState<&LayerSurface>,
        rx: &Receiver<DispatchToken>,
        ui: &mut impl FnMut(&mut egui::Ui),
        ready: impl Fn(&[WaylandEvent]) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let token = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            let events = app.dispatch_pending(token);
            view.handle_events(app, &events, ui);
            if ready(&events) {
                break;
            }
        }
    }

    fn check_loss(whole_seat: bool, leave_first: bool, drag_first: bool) {
        let _guard = private::private_input_test_guard();
        let device = VirtualPointer::new();
        let _display_guard;
        let (tx, rx) = channel();
        let mut app = Application::new(move |token| {
            let _ = tx.send(token);
        });
        _display_guard = app.conn.clone();
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            Layer::Overlay,
            Some("wayapp-pointer-cancel"),
            None,
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_size(0, 0);
        layer.commit();
        let mut view = EguiSurfaceState::new(&app, &layer, 800, 600);
        let context = RefCell::<Option<egui::Context>>::new(None);
        let clicks = Cell::new(0);
        let mut ui = |ui: &mut egui::Ui| {
            *context.borrow_mut() = Some(ui.ctx().clone());
            let rect = egui::Rect::from_min_size(egui::pos2(350.0, 280.0), egui::vec2(100.0, 40.0));
            if ui
                .put(
                    rect,
                    egui::Button::new("").sense(egui::Sense::click_and_drag()),
                )
                .clicked()
            {
                clicks.set(clicks.get() + 1);
            }
        };
        app.run_dispatcher();
        pump(&mut app, &mut view, &rx, &mut ui, |events| {
            events
                .iter()
                .any(|e| matches!(e, WaylandEvent::LayerShellConfigure(..)))
        });
        // Model independent keyboard focus/modifiers while actual native pointer
        // input drives the widget. This fixture does not create a keyboard.
        view.handle_events(
            &mut app,
            &[
                WaylandEvent::KeyboardEnter(layer.wl_surface().clone(), Vec::new(), Vec::new()),
                WaylandEvent::ModifiersChanged(smithay_client_toolkit::seat::keyboard::Modifiers {
                    ctrl: true,
                    ..Default::default()
                }),
            ],
            &mut ui,
        );
        device.move_to(400);
        pump(&mut app, &mut view, &rx, &mut ui, |events| {
            events.iter().any(|e| {
                matches!(
                    e,
                    WaylandEvent::PointerEvent((
                        _,
                        (400.0, 300.0),
                        PointerEventKind::Motion { .. }
                    ))
                )
            })
        });
        let native_pointer = app.last_pointer.clone().unwrap();
        device.press();
        pump(&mut app, &mut view, &rx, &mut ui, |events| {
            events.iter().any(|e| {
                matches!(
                    e,
                    WaylandEvent::PointerEvent((
                        _,
                        _,
                        PointerEventKind::Press { button: 0x110, .. }
                    ))
                )
            })
        });
        let context = context.borrow().as_ref().unwrap().clone();
        assert!(
            context.input(|i| i.pointer.primary_down()),
            "fixture did not press the widget"
        );
        assert_eq!(clicks.get(), 0);
        if drag_first {
            device.move_to(412);
            pump(&mut app, &mut view, &rx, &mut ui, |events| {
                events.iter().any(|e| {
                    matches!(
                        e,
                        WaylandEvent::PointerEvent((
                            _,
                            (412.0, 300.0),
                            PointerEventKind::Motion { .. }
                        ))
                    )
                })
            });
            assert!(
                context.dragged_id().is_some(),
                "fixture did not start a widget drag"
            );
        }
        if leave_first {
            app.pointer_frame(
                &app.conn.clone(),
                &app.qh.clone(),
                &native_pointer,
                &[PointerEvent {
                    surface: layer.wl_surface().clone(),
                    position: (400.0, 300.0),
                    kind: PointerEventKind::Leave { serial: 10 },
                }],
            );
            let events = app.take_wayland_events();
            view.handle_events(&mut app, &events, &mut ui);
            assert!(
                context.input(|i| i.pointer.primary_down()),
                "ordinary Leave canceled a live drag"
            );
        }
        let seat = app.seat_state.seats().next().unwrap();
        // Seed the independent keyboard assertion directly: the upstream
        // modifier-event delivery bug (0017) is not this cancellation fix.
        context.input_mut(|input| input.modifiers.ctrl = true);
        assert!(context.input(|input| input.modifiers.ctrl && input.raw.focused));
        let conn = app.conn.clone();
        let qh = app.qh.clone();
        if whole_seat {
            app.remove_seat(&conn, &qh, seat);
        } else {
            app.remove_capability(&conn, &qh, seat, Capability::Pointer);
        }
        let events = app.take_wayland_events();
        view.handle_events(&mut app, &events, &mut ui);
        // Force UI observation even when upstream emits no removal notification.
        view.handle_events(
            &mut app,
            &[WaylandEvent::Frame(layer.wl_surface().clone(), 0)],
            &mut ui,
        );
        assert!(
            !context.input(|i| i.pointer.primary_down()),
            "device loss left the widget button held"
        );
        assert!(
            !context.input(|i| i.pointer.has_pointer()),
            "device loss retained pointer presence"
        );
        assert!(
            context.dragged_id().is_none(),
            "device loss left a widget drag active"
        );
        assert_eq!(clicks.get(), 0, "device loss synthesized a widget click");
        assert!(
            context.input(|i| i.modifiers.ctrl && i.raw.focused),
            "pointer-only cancellation discarded keyboard modifiers/focus"
        );
        assert!(
            !native_pointer.is_alive(),
            "retired managed pointer stayed alive"
        );
        assert!(app.last_pointer.is_none() && app.last_pointer_enter_serial.is_none());
        assert!(app.pointer_shape_devices.is_empty());
        app.pointer_frame(
            &conn,
            &qh,
            &native_pointer,
            &[PointerEvent {
                surface: layer.wl_surface().clone(),
                position: (400.0, 300.0),
                kind: PointerEventKind::Enter { serial: 11 },
            }],
        );
        assert!(
            !app.take_wayland_events()
                .iter()
                .any(|event| matches!(event, WaylandEvent::PointerEvent(_))),
            "already queued callback resurrected a retired device"
        );
        conn.roundtrip()
            .expect("pointer retirement damaged the display");
    }

    #[test]
    #[ignore = "requires private headless runner; native pointer/WGPU, controlled capability loss"]
    fn capability_loss_cancels_a_native_widget_press_without_click() {
        check_loss(false, false, false);
    }

    #[test]
    #[ignore = "requires private headless runner; native press, controlled Leave/capability \
                callbacks"]
    fn capability_loss_after_leave_remembers_the_pressed_surface() {
        check_loss(false, true, false);
    }

    #[test]
    #[ignore = "requires private headless runner; native widget drag, controlled seat removal"]
    fn seat_loss_cancels_a_native_widget_drag_without_blurring_keyboard() {
        check_loss(true, false, true);
    }
}

#[cfg(test)]
mod client_repeat_regression {
    use super::*;
    use crate::SingleColorState;
    use smithay_client_toolkit::seat::keyboard::Modifiers;
    use smithay_client_toolkit::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerSurface};
    use std::cell::Cell;
    use std::io::Write;
    use std::os::fd::AsFd;
    use std::process::Command;
    use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
    use std::time::{Duration, Instant};
    use smithay_client_toolkit::reexports::protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
    use smithay_client_toolkit::reexports::protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;
    use crate::test_support as private;
    use super::legacy_input_tests::LegacyInput as Device;

    struct Harness {
        color: Option<SingleColorState<LayerSurface>>,
        layer: Option<LayerSurface>,
        app: Application,
        rx: Receiver<DispatchToken>,
        keyboard: ZwpVirtualKeyboardV1,
        _devices: EventQueue<Device>,
        device_connection: Connection,
        held: Cell<Option<u32>>,
        _display: Connection,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            if let Some(code) = self.held.get() {
                self.keyboard.key(100, code, 0);
            }
            self.keyboard.modifiers(0, 0, 0, 0);
            self.keyboard.destroy();
            let _ = self.device_connection.flush();
        }
    }

    impl Harness {
        fn new() -> Self {
            let guard = private::private_input_test_guard();
            let connection = Connection::connect_to_env().unwrap();
            let (globals, devices) = registry_queue_init::<Device>(&connection).unwrap();
            let qh = devices.handle();
            let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=7, ()).unwrap();
            let manager: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).unwrap();
            let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
            let compiled = Command::new("xkbcli")
                .args([
                    "compile-keymap",
                    "--layout",
                    "us",
                    "--variant",
                    "intl",
                    "--options",
                    "",
                ])
                .output()
                .unwrap();
            assert!(compiled.status.success());
            let mut bytes = compiled.stdout;
            bytes.push(0);
            let path = std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap())
                .join(format!("client-repeat-{}.keymap", std::process::id()));
            let mut file = std::fs::File::create_new(&path).unwrap();
            std::fs::remove_file(&path).unwrap();
            file.write_all(&bytes).unwrap();
            keyboard.keymap(1, file.as_fd(), bytes.len().try_into().unwrap());
            connection.roundtrip().unwrap();
            let (tx, rx) = channel();
            let mut app = Application::new(move |token| {
                let _ = tx.send(token);
            });
            let layer = app.layer_shell.create_layer_surface(
                &app.qh,
                app.compositor_state.create_surface(&app.qh),
                Layer::Top,
                Some("wayapp-client-repeat"),
                None,
            );
            layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
            layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
            layer.set_size(0, 0);
            layer.commit();
            let color = SingleColorState::new(layer.clone(), (0, 0, 0), 800, 600);
            let display = app.conn.clone();
            app.run_dispatcher();
            let mut h = Self {
                color: Some(color),
                layer: Some(layer),
                app,
                rx,
                keyboard,
                _devices: devices,
                device_connection: connection,
                held: Cell::new(None),
                _display: display,
                _guard: guard,
            };
            h.wait(|events| {
                events
                    .iter()
                    .any(|e| matches!(e, WaylandEvent::KeyboardEnter(..)))
            });
            h
        }

        fn next(&mut self, timeout: Duration) -> Option<Vec<WaylandEvent>> {
            self.app.conn.flush().unwrap();
            match self.rx.recv_timeout(timeout) {
                Ok(token) => {
                    let events = self.app.dispatch_pending(token);
                    if let Some(color) = self.color.as_mut() {
                        color.handle_events(&self.app, &events);
                    }
                    Some(events)
                }
                Err(RecvTimeoutError::Timeout) => None,
                Err(error) => panic!("native dispatcher stopped: {error}"),
            }
        }

        fn wait(&mut self, mut ready: impl FnMut(&[WaylandEvent]) -> bool) -> Vec<WaylandEvent> {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let events = self
                    .next(deadline.saturating_duration_since(Instant::now()))
                    .expect("held native key produced no expected repeat/event");
                if ready(&events) {
                    return events;
                }
                assert!(
                    Instant::now() < deadline,
                    "native repeat/event deadline expired"
                );
            }
        }

        fn hold(&mut self, code: u32) {
            self.held.set(Some(code));
            self.keyboard.key(1, code, 1);
            self.device_connection.roundtrip().unwrap();
            self.wait(|events| {
                events.iter().any(|e| {
                    matches!(e,
                WaylandEvent::KeyPress(key) if key.raw_code == code)
                })
            });
        }

        fn release(&mut self) {
            let code = self.held.take().unwrap();
            self.keyboard.key(2, code, 0);
            self.device_connection.roundtrip().unwrap();
            self.wait(|events| {
                events.iter().any(|e| {
                    matches!(e,
                WaylandEvent::KeyRelease(key) if key.raw_code == code)
                })
            });
        }

        fn repeat(&mut self, expected: &str) {
            self.wait(|events| {
                events.iter().any(|e| {
                    matches!(e,
                WaylandEvent::KeyRepeat(key) if key.utf8.as_deref() == Some(expected))
                })
            });
        }

        fn no_repeat_for(&mut self, duration: Duration) {
            let deadline = Instant::now() + duration;
            while let Some(events) = self.next(deadline.saturating_duration_since(Instant::now())) {
                assert!(
                    !events
                        .iter()
                        .any(|e| matches!(e, WaylandEvent::KeyRepeat(_))),
                    "retired key/focus kept repeating"
                );
                if Instant::now() >= deadline {
                    break;
                }
            }
        }
    }

    #[test]
    #[ignore = "requires private Sway, native virtual keyboard and SHM; no egui/WGPU/capture"]
    fn native_hold_repeats_and_release_stops_without_replaying_stalls() {
        let mut h = Harness::new();
        h.hold(30);
        h.repeat("a");
        std::thread::sleep(Duration::from_millis(150));
        let events = h.app.dispatch_pending(DispatchToken::default());
        assert!(
            events
                .iter()
                .filter(|e| matches!(e, WaylandEvent::KeyRepeat(_)))
                .count()
                <= 1,
            "stalled main loop replayed a burst"
        );
        h.release();
        h.no_repeat_for(Duration::from_millis(180));
    }

    #[test]
    #[ignore = "requires private Sway, native virtual keyboard and SHM; no egui/WGPU/capture"]
    fn repeat_retains_composed_text_until_native_modifiers_change_its_meaning() {
        let mut h = Harness::new();
        h.hold(40);
        h.release(); // Pending dead acute.
        h.hold(18);
        h.repeat("é");
        h.keyboard.modifiers(0, 0, 1 << 4, 0); // Unrelated Num Lock.
        h.device_connection.roundtrip().unwrap();
        h.wait(|events| {
            events
                .iter()
                .any(|e| matches!(e, WaylandEvent::ModifiersChanged(_)))
        });
        h.repeat("é");
        h.keyboard.modifiers(1 << 2, 0, 1 << 4, 0);
        h.device_connection.roundtrip().unwrap();
        h.wait(|events| {
            events.iter().any(|e| {
                matches!(
                    e,
                    WaylandEvent::ModifiersChanged(Modifiers { ctrl: true, .. })
                )
            })
        });
        h.repeat("\u{5}"); // Control changes UTF-8 without changing keysym e.
        h.keyboard.modifiers(0, 0, 1 << 4, 0);
        h.device_connection.roundtrip().unwrap();
        h.wait(|events| {
            events.iter().any(|e| {
                matches!(
                    e,
                    WaylandEvent::ModifiersChanged(Modifiers { ctrl: false, .. })
                )
            })
        });
        h.repeat("e");
        h.keyboard.modifiers(1, 0, 1 << 4, 0);
        h.device_connection.roundtrip().unwrap();
        h.wait(|events| {
            events.iter().any(|e| {
                matches!(
                    e,
                    WaylandEvent::ModifiersChanged(Modifiers { shift: true, .. })
                )
            })
        });
        h.repeat("E");
        h.keyboard.modifiers(0, 0, 0, 0);
        h.device_connection.roundtrip().unwrap();
        h.wait(|events| {
            events.iter().any(|e| {
                matches!(
                    e,
                    WaylandEvent::ModifiersChanged(Modifiers { shift: false, .. })
                )
            })
        });
        h.repeat("e");
        h.release();
    }

    #[test]
    #[ignore = "requires private Sway, native focus transfer and SHM; no egui/WGPU/capture"]
    fn native_focus_loss_and_local_surface_death_stop_held_repetition() {
        let mut h = Harness::new();
        h.hold(30);
        h.repeat("a");
        let other = h.app.layer_shell.create_layer_surface(
            &h.app.qh,
            h.app.compositor_state.create_surface(&h.app.qh),
            Layer::Overlay,
            Some("wayapp-repeat-other-focus"),
            None,
        );
        other.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        other.set_size(64, 64);
        other.commit();
        let mut color = SingleColorState::new(other.clone(), (0, 0, 0), 64, 64);
        let focus_deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let events = h
                .next(focus_deadline.saturating_duration_since(Instant::now()))
                .expect("other focus did not configure");
            color.handle_events(&h.app, &events);
            if events.iter().any(|e| {
                matches!(e,
                WaylandEvent::KeyboardEnter(surface, _, _) if surface == other.wl_surface())
            }) {
                break;
            }
            assert!(
                Instant::now() < focus_deadline,
                "focus transfer deadline expired"
            );
        }
        h.no_repeat_for(Duration::from_millis(180));
        h.release();
        other.wl_surface().attach(None, 0, 0);
        other.commit();
        let original = h.layer.as_ref().unwrap().wl_surface().clone();
        h.wait(|events| {
            events.iter().any(|e| {
                matches!(e,
            WaylandEvent::KeyboardEnter(surface, _, _) if surface == &original)
            })
        });
        h.hold(30);
        h.repeat("a");
        let target = h.layer.as_ref().unwrap().wl_surface().clone();
        drop(h.color.take());
        drop(h.layer.take());
        assert!(
            !target.is_alive(),
            "fixture retained the supposedly destroyed surface"
        );
        let events = h.app.dispatch_pending(DispatchToken::default());
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, WaylandEvent::KeyRepeat(_)))
        );
        h.no_repeat_for(Duration::from_millis(180));
    }
}
