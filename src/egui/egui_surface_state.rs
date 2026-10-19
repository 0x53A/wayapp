//! EGUI view manager implementation
//!
//! This module provides a ViewManager-based approach to handling EGUI surfaces
//! following the pattern from single_color.rs

use super::input_coordinates::logical_input_to_egui_points;
use crate::Application;
use crate::EguiWgpuRenderer;
use crate::FrameScheduler;
use crate::Kind;
use crate::WaylandEvent;
use crate::WaylandToEguiInput;
use crate::egui_to_cursor_shape;
use egui::Context;
use log::trace;
use smithay_client_toolkit::compositor::FrameCallbackData;
use smithay_client_toolkit::compositor::SurfaceData;
use smithay_client_toolkit::reexports::csd_frame::WindowState;
use smithay_client_toolkit::seat::keyboard::KeyEvent;
use smithay_client_toolkit::seat::keyboard::Modifiers as WaylandModifiers;
use smithay_client_toolkit::seat::pointer::PointerEvent;
use smithay_clipboard::Clipboard;
use std::num::NonZero;
use std::ops::Deref;
use std::ops::DerefMut;
use std::time::Duration;
use std::time::Instant;
use wayland_client::Proxy;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

/// Surface-specific EGUI state.
/// Releases its viewport on drop, so callers retaining the native surface can
/// attach a replacement view without leaving a duplicate protocol object.
pub struct EguiSurfaceState<T: Into<Kind> + Clone> {
    viewport: Option<WpViewport>,
    // renderer: EguiWgpuRendererThread, // for async rendering thread
    // Clipboard inside input_state borrows the display retained by renderer.
    // Drop input_state before renderer releases its Connection clone.
    input_state: WaylandToEguiInput,
    renderer: EguiWgpuRenderer, // surface can be suspended via renderer.suspend()
    // The renderer borrows the native wl_surface. Keep its role owners alive
    // through GPU resource/surface cleanup, not merely a cloned proxy handle.
    t: T,
    kind: Kind,
    width: u32,  // WGPU Surface width in logical pixels
    height: u32, // WGPU Surface height in logical pixels
    scale_factor: i32,
    suspended: bool,
    last_fulloutput: Option<egui::FullOutput>,
    last_ime_output: Option<egui::output::IMEOutput>,
    frame_timings: Option<(Instant, Instant)>,
    has_keyboard_focus: bool,
    cursor_icon: egui::CursorIcon,
    input_zoom_factor: f32,
    egui_context: Context,
    frame_scheduler: FrameScheduler,
}

impl<T: Into<Kind> + Clone> Drop for EguiSurfaceState<T> {
    fn drop(&mut self) {
        // Proxy drop alone does not send the protocol destructor. The native
        // surface may outlive this view and must be free to get a new viewport.
        if let Some(viewport) = self.viewport.take()
            && viewport.is_alive()
        {
            viewport.destroy();
        }
        // Closing a surface with unpainted output intentionally abandons its
        // deltas: the renderer and all its GPU textures are being destroyed.
        if let Some(output) = self.last_fulloutput.take() {
            output.drop_without_applying_deltas();
        }
    }
}

impl<T: Into<Kind> + Clone> EguiSurfaceState<T> {
    /// Create a view, retaining cached scale from SCTK's default surface data.
    /// Custom/unknown surface data falls back to scale 1 until a scale event.
    /// An already-focused surface also retains its known window
    /// focus/modifiers; egui widget/input memory is fresh, with no
    /// synthetic keypress replay.
    /// Unfocused views inherit current modifiers without claiming focus.
    /// A focused pointer retains its known position, but not held buttons.
    pub fn new(app: &Application, t: T, width: u32, height: u32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let kind = t.clone().into();
        let wl_surface = kind.get_wl_surface();
        let scale_factor = wl_surface
            .data::<SurfaceData<()>>()
            .map_or(1, |data| data.scale_factor().max(1));
        let egui_context = Context::default();
        let mut renderer = EguiWgpuRenderer::new(&egui_context, wl_surface, &app.conn);
        renderer.set_buffer_scale(scale_factor);
        let clipboard = unsafe { Clipboard::new(app.conn.display().id().as_ptr() as *mut _) };
        let mut input_state = WaylandToEguiInput::new(clipboard);
        input_state.set_screen_size(width, height);
        if let Some(position) = app.pointer_focus_position(wl_surface) {
            // View replacement does not make a stationary native pointer enter
            // again. Restore only location, never held buttons or widget state.
            input_state.update_pointer_position(position);
        }
        let keyboard_modifiers = app.keyboard_focus_modifiers(wl_surface);
        if keyboard_modifiers.is_some() {
            // A retained native surface may already own focus; no second
            // enter/modifier event is guaranteed when only the view is replaced.
            input_state.handle_keyboard_enter();
        }
        // Modifier events are application-wide, even for unfocused views.
        // Bootstrap the same state without fabricating a native focus change.
        input_state.update_modifiers(&app.current_keyboard_modifiers());
        let emitter = app.get_event_emitter();
        let wl_surface_clone = wl_surface.clone();
        let frame_scheduler = FrameScheduler::new(move || {
            // Note: Using wl_surface.frame(), wl_surface.commit(), conn.flush()
            // caused crashes with WGPU handling, so I created a way to emit Frame
            // event without Wayland dispatching.
            emitter.emit_events(vec![crate::WaylandEvent::Frame(
                wl_surface_clone.clone(),
                0,
            )]);
        });
        let frame_scheduler_fn = frame_scheduler.create_scheduler();
        egui_context.set_request_repaint_callback(move |i| {
            frame_scheduler_fn(i.delay);
        });

        Self {
            viewport: None,
            t,
            kind,
            renderer,
            input_state,
            width,
            height,
            scale_factor,
            suspended: false,
            last_fulloutput: None,
            last_ime_output: None,
            frame_timings: None,
            has_keyboard_focus: keyboard_modifiers.is_some(),
            cursor_icon: egui::CursorIcon::Default,
            input_zoom_factor: 1.0,
            egui_context,
            frame_scheduler,
        }
    }

    pub fn get_content(&self) -> &T {
        &self.t
    }

    fn wl_surface(&self) -> &WlSurface {
        self.kind.get_wl_surface()
    }

    // pub fn get_kind(&self) -> &Kind {
    //     &self.kind
    // }

    // pub fn contains<V: Into<Kind>>(&self, other: V) -> bool {
    //     self.kind == other.into()
    // }

    fn configure(
        &mut self,
        app: &Application,
        width: u32,
        height: u32,
        window_state: Option<WindowState>,
    ) {
        let was_suspended = self.suspended;
        // Zero means the compositor leaves that dimension to the client.
        // Preserve the current size rather than sending an invalid viewport.
        let width = if width == 0 { self.width } else { width };
        let height = if height == 0 { self.height } else { height };
        self.resize_viewport(app, width, height);
        self.width = width.max(1);
        self.height = height.max(1);
        self.input_state.set_screen_size(self.width, self.height);
        self.suspended = window_state.map_or(false, |state| state.contains(WindowState::SUSPENDED));

        if self.suspended && !was_suspended {
            // Surface just got suspended — destroy the WGPU surface to free GPU
            // resources. Device, queue, and egui renderer are kept to preserve
            // texture state (EGUI doesn't handle renderer recreate well).
            trace!(
                "[EGUI] Suspending renderer for surface {:?}",
                self.wl_surface().id()
            );
            self.renderer.suspend();
        } else if !self.suspended && was_suspended {
            // Surface just got resumed — recreate the WGPU surface
            trace!(
                "[EGUI] Resuming renderer for surface {:?}",
                self.wl_surface().id()
            );
            self.renderer.resume();
        }

        self.frame_scheduler.set_fps_target(
            if window_state.map_or(false, |state| state.contains(WindowState::SUSPENDED)) {
                0.05 // 0.0001
            } else {
                60.0
            },
        );
    }

    fn resize_viewport(&mut self, app: &Application, width: u32, height: u32) {
        let wl_surface = self.wl_surface().clone();
        let viewport = self.viewport.get_or_insert_with(|| {
            trace!("[EGUI] Creating viewport for surface {:?}", wl_surface.id());
            app.viewporter
                .get()
                .expect("wp_viewporter not available")
                .get_viewport(&wl_surface, &app.qh, ())
        });

        viewport.set_destination(width as i32, height as i32);
    }

    fn handle_pointer_event(&mut self, event: &PointerEvent) {
        self.input_state.handle_pointer_event(event);
    }

    fn handle_keyboard_enter(&mut self) {
        self.input_state.handle_keyboard_enter();
    }

    fn handle_keyboard_leave(&mut self) {
        self.input_state.handle_keyboard_leave();
    }

    fn handle_keyboard_event(&mut self, event: &KeyEvent, pressed: bool, repeat: bool) {
        self.input_state
            .handle_keyboard_event(event, pressed, repeat);
    }

    fn update_modifiers(&mut self, modifiers: &WaylandModifiers) {
        self.input_state.update_modifiers(modifiers);
    }

    fn scale_factor_changed(&mut self, new_factor: i32) {
        let factor = new_factor.max(1);
        if factor == self.scale_factor {
            return;
        }
        self.renderer.set_buffer_scale(factor);
        self.scale_factor = factor;
    }

    /// Request a frame via dispatching
    ///
    /// Strictly this wouldn't be necessary, as
    /// `egui_frame_scheduler.schedule_frame` could be used. However, this
    /// method can be used to break the FPS limit, and I wanted to break it
    /// for the window resizing events at least for now.
    fn request_dispatch_frame(&mut self, app: &mut Application) {
        self.wl_surface()
            .frame(&app.qh, FrameCallbackData(self.wl_surface().clone()));
        self.wl_surface().commit();
        app.conn.flush().unwrap();
    }

    /// Request a frame via Frame scheduler
    pub fn request_frame(&mut self) {
        self.frame_scheduler.schedule_frame(Duration::ZERO);
    }

    /// Process EGUI frame (layout, input) without GPU rendering
    /// This is cheap and can be called frequently
    fn process_egui_frame(&mut self, ui: &mut impl FnMut(&mut egui::Ui)) {
        let zoom = self.egui_context.zoom_factor();
        if zoom != self.input_zoom_factor {
            // egui stores pointer state across frames in points. Its own zoom
            // transition only rescales the screen rectangle, not that state.
            self.input_state.refresh_pointer_position();
            self.input_zoom_factor = zoom;
        }
        let mut raw_input = self.input_state.take_raw_input();
        // Native monitor scale and application zoom are separate. Setting
        // pixels_per_point here would overwrite egui's zoom every frame.
        raw_input
            .viewports
            .entry(raw_input.viewport_id)
            .or_default()
            .native_pixels_per_point = Some(self.physical_scale() as f32);
        logical_input_to_egui_points(&mut raw_input, zoom);
        let mut full_output = self.egui_context.run_ui(raw_input, ui);
        self.cursor_icon = full_output.platform_output.cursor_icon;
        // Commands are handled now; retaining them until painting would keep
        // already-consumed payloads alive while a surface remains suspended.
        for command in full_output.platform_output.commands.drain(..) {
            self.input_state.handle_output_command(&command);
        }
        if let Some(last_fulloutput) = &mut self.last_fulloutput {
            last_fulloutput.append(full_output);
        } else {
            self.last_fulloutput = Some(full_output);
        }
    }

    /// Render the last processed EGUI frame to WGPU
    /// Only call this when necessary (e.g., on frame callback or when content
    /// changed)
    fn render_to_wgpu(&mut self, full_output: egui::FullOutput) {
        let width = self.width.saturating_mul(self.physical_scale());
        let height = self.height.saturating_mul(self.physical_scale());
        let pixels_per_point = full_output.pixels_per_point;

        self.renderer
            .render_to_wgpu(full_output, width, height, pixels_per_point);
        if self.suspended {
            return; // Suspended calls only maintain textures, as before.
        }

        // Update frame timings
        let now = Instant::now();
        let old = self
            .frame_timings
            .as_ref()
            .map(|(_, end)| *end)
            .unwrap_or(now);
        self.frame_timings = Some((old, now));
    }

    /// Full render of EGUI frame (layout, input + GPU rendering)
    fn render(&mut self, ui: &mut impl FnMut(&mut egui::Ui)) {
        self.process_egui_frame(ui);

        if self.suspended {
            trace!(
                "[EGUI] Consuming texture deltas without drawing suspended surface {:?}",
                self.wl_surface().id()
            );
        }

        if let Some(full_output) = self.last_fulloutput.take() {
            self.render_to_wgpu(full_output);
        }
    }

    fn physical_scale(&self) -> u32 {
        self.scale_factor.max(1) as u32
    }

    /// Send cursor position to Wayland
    fn sync_text_input_cursor(&mut self, app: &Application) {
        let new_ime = self
            .last_fulloutput
            .as_ref()
            .and_then(|o| o.platform_output.ime.clone());

        if new_ime != self.last_ime_output {
            trace!(
                "[IME] IME output changed: {:?} -> {:?}",
                self.last_ime_output, new_ime
            );
            self.last_ime_output = new_ime.clone();

            match new_ime {
                Some(ime) => {
                    if let Some(ti) = &app.text_input {
                        trace!(
                            "[IME] Activating Text Input with cursor rect: {:?}",
                            ime.cursor_rect
                        );
                        ti.enable();
                        ti.set_cursor_rectangle(
                            ime.cursor_rect.min.x as i32,
                            ime.cursor_rect.min.y as i32,
                            ime.cursor_rect.width() as i32,
                            ime.cursor_rect.height() as i32,
                        );
                        ti.commit();
                    }
                }
                None => {
                    if let Some(ti) = &app.text_input {
                        trace!("[IME] Deactivating Text Input");
                        ti.disable();
                        ti.commit();
                    }
                }
            }
        }
    }

    /// Get timings of the last render calls (previous and current time).
    pub fn get_frame_timings(&self) -> Option<(Instant, Instant)> {
        self.frame_timings
    }

    /// Get FPS of last two frames
    ///
    /// EGUI is immediate mode GUI, this means it easier to show historical FPS,
    /// as most of the time it could be zero.
    pub fn get_fps(&self) -> f32 {
        if let Some((prev_frame, next_frame)) = &self.frame_timings {
            let frame_time = next_frame.duration_since(prev_frame.clone()).as_secs_f32();
            if frame_time > 0.0 {
                return 1.0 / frame_time;
            }
        }
        return 0.0;
    }

    /// Handle Wayland events and update surfaces accordingly
    /// Applies cursor changes only while this surface owns pointer focus.
    pub fn handle_events(
        &mut self,
        app: &mut Application,
        events: &[WaylandEvent],
        ui: &mut impl FnMut(&mut egui::Ui),
    ) {
        for event in events {
            if let Some(surface) = event.get_wl_surface() {
                if surface.id() != self.wl_surface().id() {
                    continue;
                }
            }
            match event {
                WaylandEvent::WindowConfigure(_, configure) => {
                    let width = configure
                        .new_size
                        .0
                        .unwrap_or_else(|| NonZero::new(self.width).unwrap())
                        .get();
                    let height = configure
                        .new_size
                        .1
                        .unwrap_or_else(|| NonZero::new(self.height).unwrap())
                        .get();

                    self.configure(app, width, height, Some(configure.state));
                    self.render(ui);
                    self.request_dispatch_frame(app);
                }
                WaylandEvent::LayerShellConfigure(_, config) => {
                    let width = config.new_size.0;
                    let height = config.new_size.1;

                    self.configure(app, width, height, None);
                    self.render(ui);
                    self.request_dispatch_frame(app);
                }
                WaylandEvent::PopupConfigure(_, config) => {
                    let width = config.width as u32;
                    let height = config.height as u32;

                    self.configure(app, width, height, None);
                    self.render(ui);
                    self.request_dispatch_frame(app);
                }
                WaylandEvent::Frame(_, _) => {
                    self.render(ui);
                }
                WaylandEvent::ScaleFactorChanged(_, factor) => {
                    self.scale_factor_changed(*factor);
                    self.process_egui_frame(ui);
                    self.sync_text_input_cursor(app);
                    self.request_frame();
                }
                WaylandEvent::PointerEvent((surface, position, event_kind)) => {
                    self.handle_pointer_event(&PointerEvent {
                        surface: surface.clone(),
                        position: position.clone(),
                        kind: event_kind.clone(),
                    });
                    self.process_egui_frame(ui);
                    self.sync_text_input_cursor(app);
                }
                WaylandEvent::PointerRemoved(surface) => {
                    self.input_state.handle_pointer_removed(&self.egui_context);
                    if let Some(position) = app.pointer_focus_position(surface) {
                        self.input_state.update_pointer_position(position);
                    }
                    self.process_egui_frame(ui);
                    self.request_frame();
                }
                WaylandEvent::KeyboardEnter(_, _serials, _keysyms) => {
                    self.handle_keyboard_enter();
                    self.has_keyboard_focus = true;
                    self.process_egui_frame(ui);
                    self.sync_text_input_cursor(app);
                }
                WaylandEvent::KeyboardLeave(_) => {
                    self.handle_keyboard_leave();
                    self.has_keyboard_focus = false;
                    self.process_egui_frame(ui);
                    self.sync_text_input_cursor(app);
                }
                WaylandEvent::KeyPress(key_event) => {
                    if self.has_keyboard_focus {
                        self.handle_keyboard_event(key_event, true, false);
                        self.process_egui_frame(ui);
                        self.sync_text_input_cursor(app);
                    }
                }
                WaylandEvent::KeyRelease(key_event) => {
                    if self.has_keyboard_focus {
                        self.handle_keyboard_event(key_event, false, false);
                        self.process_egui_frame(ui);
                        self.sync_text_input_cursor(app);
                    }
                }
                WaylandEvent::KeyRepeat(key_event) => {
                    if self.has_keyboard_focus {
                        self.handle_keyboard_event(key_event, true, true);
                        self.process_egui_frame(ui);
                        self.sync_text_input_cursor(app);
                    }
                }
                WaylandEvent::ModifiersChanged(modifiers) => {
                    self.update_modifiers(modifiers);
                    self.process_egui_frame(ui);
                    self.sync_text_input_cursor(app);

                    // Modifier-only changes can affect hover and interaction
                    // feedback even without an accompanying key press.
                    self.request_frame();
                }
                WaylandEvent::ImeEnter(surface) => {
                    trace!("[IME] ImeEnter for surface {:?}", surface.id());
                    self.input_state.handle_ime_enter();
                    self.process_egui_frame(ui);
                }
                WaylandEvent::ImeLeave(surface) => {
                    trace!("[IME] ImeLeave for surface {:?}", surface.id());
                    self.input_state.handle_ime_leave();
                    self.process_egui_frame(ui);
                }
                // Payload events have no surface ID and are broadcast to views.
                // Like ordinary keys, only the current keyboard owner may
                // consume them, even if another context retains widget focus.
                WaylandEvent::ImeCommitString(text) if self.has_keyboard_focus => {
                    trace!("[IME] ImeCommitString buffered: {:?}", text);
                    self.input_state
                        .handle_ime_commit(text.as_ref().unwrap_or(&"".to_string()));
                    self.process_egui_frame(ui);
                    self.sync_text_input_cursor(app);
                }
                WaylandEvent::ImePreeditString(text, cursor_begin, cursor_end)
                    if self.has_keyboard_focus =>
                {
                    trace!("[IME] ImePreeditString buffered: {:?}", text);
                    self.input_state.handle_ime_preedit_string(
                        text.as_ref().unwrap_or(&"".to_string()),
                        *cursor_begin,
                        *cursor_end,
                    );
                    self.process_egui_frame(ui);
                    self.sync_text_input_cursor(app);
                }
                WaylandEvent::ImeDeleteSurroundingText(before_length, after_length) => {
                    trace!(
                        "[IME] ImeDeleteSurroundingText buffered: before={}, after={}",
                        before_length, after_length
                    );
                    // Egui expects character counts. Exact conversion needs
                    // surrounding editor text, which is not available here.
                }
                WaylandEvent::ImeDone(_serial) => {
                    // Native text-input-v3 binding is disabled. Enabling it
                    // requires buffering/applying complete transactions here,
                    // not merely forwarding preedit/commit callbacks eagerly.
                }
                _ => {}
            }
        }
        if app.pointer_has_focus(self.wl_surface()) {
            if self.cursor_icon == egui::CursorIcon::None {
                app.hide_cursor();
            } else {
                app.set_cursor(egui_to_cursor_shape(self.cursor_icon));
            }
        }
    }
}

impl<T: Into<Kind> + Clone> Deref for EguiSurfaceState<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.t
    }
}

impl<T: Into<Kind> + Clone> DerefMut for EguiSurfaceState<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.t
    }
}

/// Helper trait to allow calling `contains` on `Option<EguiSurfaceState<T>>`
pub trait OptionEguiSurfaceStateExt<T: Into<Kind> + Clone> {
    // fn contains<V: Into<Kind>>(&self, other: V) -> bool;

    fn handle_events(
        &mut self,
        app: &mut Application,
        events: &[WaylandEvent],
        ui: &mut impl FnMut(&mut egui::Ui),
    ) -> ();
}

impl<T: Into<Kind> + Clone> OptionEguiSurfaceStateExt<T> for Option<EguiSurfaceState<T>> {
    // fn contains<V: Into<Kind>>(&self, other: V) -> bool {
    //     self.as_ref().map_or(false, |s| s.contains(other))
    // }

    fn handle_events(
        &mut self,
        app: &mut Application,
        events: &[WaylandEvent],
        ui: &mut impl FnMut(&mut egui::Ui),
    ) -> () {
        if let Some(surface_state) = self {
            surface_state.handle_events(app, events, ui);
        }
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    use smithay_client_toolkit::shell::wlr_layer::Layer;

    #[test]
    #[ignore = "requires Wayland, wlr-layer-shell and a WGPU adapter; adapter events only"]
    fn ime_payloads_reach_only_the_keyboard_focused_view() {
        use smithay_client_toolkit::shell::WaylandSurface;
        let mut app = Application::new(|_| {});
        let make_layer = || {
            app.layer_shell.create_layer_surface(
                &app.qh,
                app.compositor_state.create_surface(&app.qh),
                Layer::Top,
                Some("wayapp-ime-routing"),
                None,
            )
        };
        let first_layer = make_layer();
        let second_layer = make_layer();
        let mut first = EguiSurfaceState::new(&app, &first_layer, 64, 48);
        let mut second = EguiSurfaceState::new(&app, &second_layer, 64, 48);
        let first_text = std::cell::RefCell::new(String::new());
        let second_text = std::cell::RefCell::new(String::new());
        let mut first_ui = |ui: &mut egui::Ui| {
            ui.text_edit_singleline(&mut *first_text.borrow_mut())
                .request_focus();
        };
        let mut second_ui = |ui: &mut egui::Ui| {
            ui.text_edit_singleline(&mut *second_text.borrow_mut())
                .request_focus();
        };
        for _ in 0..2 {
            first.process_egui_frame(&mut first_ui);
            second.process_egui_frame(&mut second_ui);
        }
        let first_enter =
            WaylandEvent::KeyboardEnter(first_layer.wl_surface().clone(), Vec::new(), Vec::new());
        first.handle_events(&mut app, std::slice::from_ref(&first_enter), &mut first_ui);
        second.handle_events(&mut app, std::slice::from_ref(&first_enter), &mut second_ui);
        let commit = WaylandEvent::ImeCommitString(Some("保存".into()));
        first.handle_events(&mut app, std::slice::from_ref(&commit), &mut first_ui);
        second.handle_events(&mut app, std::slice::from_ref(&commit), &mut second_ui);
        assert_eq!(*first_text.borrow(), "保存");
        assert!(
            second_text.borrow().is_empty(),
            "unfocused editor consumed another view's IME commit"
        );
        for event in [
            WaylandEvent::KeyboardLeave(first_layer.wl_surface().clone()),
            WaylandEvent::KeyboardEnter(second_layer.wl_surface().clone(), Vec::new(), Vec::new()),
            WaylandEvent::ImePreeditString(Some("未確定".into()), 0, 9),
        ] {
            first.handle_events(&mut app, std::slice::from_ref(&event), &mut first_ui);
            second.handle_events(&mut app, std::slice::from_ref(&event), &mut second_ui);
        }
        assert_eq!(
            *first_text.borrow(),
            "保存",
            "old focus consumed the new owner's preedit"
        );
        assert_eq!(*second_text.borrow(), "未確定");
        for event in [
            WaylandEvent::ImeCommitString(Some("確定".into())),
            WaylandEvent::KeyboardLeave(second_layer.wl_surface().clone()),
            WaylandEvent::ImeCommitString(Some("ignored".into())),
            WaylandEvent::ImePreeditString(Some("ignored".into()), 0, 7),
        ] {
            first.handle_events(&mut app, std::slice::from_ref(&event), &mut first_ui);
            second.handle_events(&mut app, std::slice::from_ref(&event), &mut second_ui);
        }
        assert_eq!(*first_text.borrow(), "保存");
        assert_eq!(
            *second_text.borrow(),
            "確定",
            "IME payload without keyboard focus changed text"
        );
    }

    #[test]
    #[ignore = "requires Wayland, wlr-layer-shell and a WGPU adapter"]
    fn replacement_view_uses_the_surfaces_existing_sdk_scale() {
        let app = Application::new(|_| {});
        let native = app
            .compositor_state
            .create_surface_with_data(&app.qh, None, 2, ());
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            native,
            Layer::Top,
            Some("wayapp-existing-scale"),
            None,
        );
        for _ in 0..2 {
            let mut state = EguiSurfaceState::new(&app, &layer, 63, 47);
            assert_eq!(state.scale_factor, 2, "view ignored cached native scale");
            let mut pixels_per_point = 0.0;
            state.process_egui_frame(&mut |ui| pixels_per_point = ui.ctx().pixels_per_point());
            state
                .last_fulloutput
                .take()
                .unwrap()
                .drop_without_applying_deltas();
            assert_eq!(pixels_per_point, 2.0);
        }
    }

    #[test]
    #[ignore = "requires Wayland, wlr-layer-shell and a WGPU adapter"]
    fn scale_change_does_not_make_an_unrelated_commit_reuse_an_invalid_old_buffer() {
        use smithay_client_toolkit::shell::WaylandSurface;
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = Application::new(move |token| {
            let _ = tx.send(token);
        });
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            Layer::Top,
            Some("wayapp-pending-scale"),
            None,
        );
        layer.set_size(63, 47);
        layer.commit();
        let mut state = EguiSurfaceState::new(&app, &layer, 63, 47);
        let status = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0));
        let mut ui = |ui: &mut egui::Ui| {
            ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                ui.max_rect(),
                NativeLivenessCallback {
                    surface: layer.wl_surface().clone(),
                    status: status.clone(),
                },
            ));
        };
        app.run_dispatcher();
        let deadline = Instant::now() + Duration::from_secs(5);
        while status.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            let token = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            let events = app.dispatch_pending(token);
            state.handle_events(&mut app, &events, &mut ui);
        }
        app.conn.roundtrip().unwrap();
        state.scale_factor_changed(2);
        // A caller may commit a role change before the scheduled repaint.
        // The previous 63x47 buffer must still be committed at scale 1.
        layer.commit();
        app.conn
            .roundtrip()
            .expect("pending DPI change invalidated the old buffer");
        use egui_wgpu::wgpu::CurrentSurfaceTexture::Occluded;
        use egui_wgpu::wgpu::CurrentSurfaceTexture::Outdated;
        use egui_wgpu::wgpu::CurrentSurfaceTexture::Timeout;
        use egui_wgpu::wgpu::CurrentSurfaceTexture::Validation;
        for failures in [
            vec![Timeout],
            vec![Occluded],
            vec![Validation],
            vec![Outdated, Timeout],
        ] {
            state.renderer.acquisition_override = failures.into();
            state.render(&mut ui);
            layer.commit();
            app.conn
                .roundtrip()
                .expect("skipped DPI frame changed the old buffer's scale");
        }
        state.render(&mut ui);
        app.conn
            .roundtrip()
            .expect("new scaled frame disconnected Wayland");
        assert_eq!(state.physical_scale(), 2);
    }

    #[test]
    #[ignore = "requires Wayland, wlr-layer-shell and a WGPU adapter"]
    fn dropping_view_state_releases_viewport_on_a_retained_native_surface() {
        let app = Application::new(|_| {});
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            Layer::Top,
            Some("wayapp-egui-viewport-lifetime"),
            None,
        );
        for _ in 0..2 {
            let mut state = EguiSurfaceState::new(&app, &layer, 64, 64);
            state.configure(&app, 64, 64, None);
            let viewport = state.viewport.as_ref().unwrap().clone();
            drop(state);
            assert!(
                !viewport.is_alive(),
                "view drop retained its native viewport object"
            );
            app.conn
                .roundtrip()
                .expect("viewport reuse disconnected Wayland");
        }
    }

    struct NativeLivenessOnDrop {
        surface: WlSurface,
        status: std::sync::Arc<std::sync::atomic::AtomicU8>,
    }

    impl Drop for NativeLivenessOnDrop {
        fn drop(&mut self) {
            self.status.store(
                if self.surface.is_alive() { 2 } else { 3 },
                std::sync::atomic::Ordering::SeqCst,
            );
        }
    }

    struct NativeLivenessCallback {
        surface: WlSurface,
        status: std::sync::Arc<std::sync::atomic::AtomicU8>,
    }

    impl egui_wgpu::CallbackTrait for NativeLivenessCallback {
        fn prepare(
            &self,
            _: &egui_wgpu::wgpu::Device,
            _: &egui_wgpu::wgpu::Queue,
            _: &egui_wgpu::ScreenDescriptor,
            _: &mut egui_wgpu::wgpu::CommandEncoder,
            resources: &mut egui_wgpu::CallbackResources,
        ) -> Vec<egui_wgpu::wgpu::CommandBuffer> {
            if resources.get::<NativeLivenessOnDrop>().is_none() {
                resources.insert(NativeLivenessOnDrop {
                    surface: self.surface.clone(),
                    status: self.status.clone(),
                });
                self.status.store(1, std::sync::atomic::Ordering::SeqCst);
            }
            Vec::new()
        }

        fn paint(
            &self,
            _: egui::epaint::PaintCallbackInfo,
            _: &mut egui_wgpu::wgpu::RenderPass<'static>,
            _: &egui_wgpu::CallbackResources,
        ) {
        }
    }

    #[test]
    #[ignore = "requires Wayland, wlr-layer-shell and a WGPU adapter"]
    fn owned_native_surface_outlives_gpu_resource_cleanup() {
        check_owned_native_surface_cleanup(false);
        check_owned_native_surface_cleanup(true);
    }

    fn check_owned_native_surface_cleanup(drop_application_first: bool) {
        use smithay_client_toolkit::shell::WaylandSurface;
        use std::sync::Arc;
        use std::sync::atomic::AtomicU8;
        use std::sync::atomic::Ordering;
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = Application::new(move |token| {
            let _ = tx.send(token);
        });
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            Layer::Top,
            Some("wayapp-native-gpu-drop-order-probe"),
            None,
        );
        layer.set_size(64, 64);
        layer.commit();
        let target = layer.wl_surface().clone();
        let mut surface = EguiSurfaceState::new(&app, layer.clone(), 64, 64);
        let status = Arc::new(AtomicU8::new(0));
        app.run_dispatcher();
        let deadline = Instant::now() + Duration::from_secs(5);
        while status.load(Ordering::SeqCst) == 0 {
            let token = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            let events = app.dispatch_pending(token);
            surface.handle_events(&mut app, &events, &mut |ui| {
                let rect = egui::Rect::from_min_size(ui.min_rect().min, egui::vec2(16.0, 16.0));
                ui.painter().add(egui::Shape::Callback(
                    egui_wgpu::Callback::new_paint_callback(
                        rect,
                        NativeLivenessCallback {
                            surface: target.clone(),
                            status: status.clone(),
                        },
                    ),
                ));
            });
        }
        drop(layer); // EguiSurfaceState now owns the last native role references.
        assert!(target.is_alive());
        if drop_application_first {
            drop(app);
        }
        drop(surface);
        assert!(
            !target.is_alive(),
            "a test fixture retained the native surface owner"
        );
        assert_eq!(
            status.load(Ordering::SeqCst),
            2,
            "native wl_surface was destroyed before renderer callback resources dropped"
        );
    }

    #[test]
    #[ignore = "requires private headless runner, wlr-layer-shell and a WGPU adapter"]
    fn processed_platform_commands_are_not_retained_in_pending_frames() {
        let _guard = crate::test_support::private_input_test_guard();
        let app = Application::new(|_| {});
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            Layer::Top,
            Some("wayapp-platform-output-probe"),
            None,
        );
        let mut surface = EguiSurfaceState::new(&app, &layer, 64, 64);
        for _ in 0..3 {
            surface.process_egui_frame(&mut |ui| {
                // The existing adapter only logs this command: no clipboard
                // contents, browser or external URL are involved in this check.
                ui.ctx().open_url(egui::OpenUrl::new_tab("about:blank"));
            });
        }
        assert!(
            surface
                .last_fulloutput
                .as_ref()
                .unwrap()
                .platform_output
                .commands
                .is_empty()
        );
    }

    #[test]
    #[ignore = "requires Wayland/WGPU; feeds adapter events, not native IME transactions"]
    fn empty_or_absent_ime_payload_cancels_only_active_composition() {
        use smithay_client_toolkit::seat::keyboard::Keysym;
        for cancel in [
            WaylandEvent::ImeCommitString(None),
            WaylandEvent::ImeCommitString(Some(String::new())),
            WaylandEvent::ImePreeditString(None, 0, 0),
            WaylandEvent::ImePreeditString(Some(String::new()), 0, 0),
        ] {
            let mut app = Application::new(|_| {});
            let layer = app.layer_shell.create_layer_surface(
                &app.qh,
                app.compositor_state.create_surface(&app.qh),
                Layer::Top,
                Some("wayapp-empty-ime-payload"),
                None,
            );
            let mut view = EguiSurfaceState::new(&app, &layer, 400, 100);
            let native = view.wl_surface().clone();
            let text = std::cell::RefCell::new(String::new());
            let mut ui = |ui: &mut egui::Ui| {
                ui.text_edit_singleline(&mut *text.borrow_mut())
                    .request_focus();
            };
            view.handle_events(
                &mut app,
                &[
                    WaylandEvent::KeyboardEnter(native, Vec::new(), Vec::new()),
                    WaylandEvent::ImeCommitString(Some("保存".into())),
                    WaylandEvent::ImePreeditString(Some("未確定".into()), 0, 9),
                ],
                &mut ui,
            );
            assert_eq!(*text.borrow(), "保存未確定");
            view.handle_events(&mut app, std::slice::from_ref(&cancel), &mut ui);
            assert_eq!(
                *text.borrow(),
                "保存",
                "{cancel:?} retained preedit or deleted committed text"
            );
            let select = KeyEvent {
                time: 1,
                raw_code: 30,
                keysym: Keysym::a,
                utf8: None,
            };
            view.handle_events(
                &mut app,
                &[
                    WaylandEvent::ModifiersChanged(WaylandModifiers {
                        ctrl: true,
                        ..Default::default()
                    }),
                    WaylandEvent::KeyPress(select.clone()),
                    WaylandEvent::KeyRelease(select),
                    WaylandEvent::ModifiersChanged(WaylandModifiers::default()),
                ],
                &mut ui,
            );
            view.handle_events(&mut app, std::slice::from_ref(&cancel), &mut ui);
            assert_eq!(
                *text.borrow(),
                "保存",
                "idle cancellation deleted ordinary selection"
            );
            let replace = KeyEvent {
                time: 2,
                raw_code: 48,
                keysym: Keysym::b,
                utf8: Some("b".into()),
            };
            view.handle_events(&mut app, &[WaylandEvent::KeyPress(replace)], &mut ui);
            assert_eq!(
                *text.borrow(),
                "b",
                "idle cancellation changed ordinary selection"
            );
        }
    }

    #[test]
    fn clipboard_output_test_refuses_unprotected_execution_before_connecting() {
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "egui::egui_surface_state::regression_tests::processed_platform_commands_are_not_retained_in_pending_frames",
                "--ignored",
                "--nocapture",
            ])
            .env_remove("WAYAPP_HEADLESS_TEST")
            // No display is available even if the guard is accidentally lost.
            .env_remove("WAYLAND_SOCKET")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("XDG_RUNTIME_DIR")
            .env_remove("DISPLAY")
            .output()
            .unwrap();
        assert!(!child.status.success());
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(
            output.contains("use bash repros/run-headless.sh"),
            "clipboard-writing test bypassed private-compositor guard: {output}"
        );
    }

    #[test]
    fn private_runtime_guard_rejects_indirect_or_shared_directories() {
        const CHILD: &str = "WAYAPP_RUNTIME_GUARD_PROBE";
        if std::env::var_os(CHILD).is_some() {
            crate::test_support::require_private_compositor();
            return; // Check only; never connect to the dummy display socket.
        }
        use std::os::unix::fs::PermissionsExt;
        struct Fixture {
            root: std::path::PathBuf,
            link: std::path::PathBuf,
            link_created: bool,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(self.root.join("wayland-probe"));
                let _ = std::fs::remove_file(self.root.join("wayland-alias"));
                if self.link_created {
                    let _ = std::fs::remove_file(&self.link);
                }
                let _ = std::fs::remove_dir(&self.root);
            }
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::path::PathBuf::from(format!(
            "/tmp/wayapp-test-runtime.guard-{}-{nonce}",
            std::process::id()
        ));
        let link = std::path::PathBuf::from(format!("{}.link", root.display()));
        std::fs::create_dir(&root).unwrap();
        let mut fixture = Fixture {
            root,
            link,
            link_created: false,
        };
        std::fs::set_permissions(&fixture.root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _socket =
            std::os::unix::net::UnixListener::bind(fixture.root.join("wayland-probe")).unwrap();
        std::os::unix::fs::symlink(&fixture.root, &fixture.link).unwrap();
        fixture.link_created = true;
        std::os::unix::fs::symlink(
            fixture.root.join("wayland-probe"),
            fixture.root.join("wayland-alias"),
        )
        .unwrap();
        let probe = |runtime: &std::path::Path, display: &str, inherited_socket: bool| {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", "egui::egui_surface_state::regression_tests::private_runtime_guard_rejects_indirect_or_shared_directories", "--nocapture"])
                .env(CHILD, "1")
                .env("WAYAPP_HEADLESS_TEST", "1")
                .env("XDG_RUNTIME_DIR", runtime)
                .env("WAYLAND_DISPLAY", display)
                .env_remove("WAYLAND_SOCKET")
                .env_remove("DISPLAY");
            if inherited_socket {
                command.env("WAYLAND_SOCKET", "999999"); // Guard check only, never use this fd.
            }
            let mut child = command
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break child.wait_with_output().unwrap(),
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    state => {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("runtime guard probe did not finish: {state:?}");
                    }
                }
            }
        };
        let direct = probe(&fixture.root, "wayland-probe", false);
        assert!(
            direct.status.success(),
            "valid private directory rejected: {}",
            String::from_utf8_lossy(&direct.stderr)
        );
        assert!(
            !probe(&fixture.link, "wayland-probe", false)
                .status
                .success(),
            "symlinked runtime bypassed safety guard"
        );
        let traversal = fixture
            .root
            .join("..")
            .join(fixture.root.file_name().unwrap());
        assert!(
            !probe(&traversal, "wayland-probe", false).status.success(),
            "path traversal bypassed safety guard"
        );
        let absolute_socket = fixture.root.join("wayland-probe");
        assert!(
            !probe(&fixture.root, absolute_socket.to_str().unwrap(), false)
                .status
                .success(),
            "absolute display bypassed safety guard"
        );
        assert!(
            !probe(&fixture.root, "wayland-probe", true).status.success(),
            "inherited display fd bypassed safety guard"
        );
        assert!(
            !probe(&fixture.root, "wayland-alias", false)
                .status
                .success(),
            "symlinked display socket bypassed safety guard"
        );
        std::fs::set_permissions(&fixture.root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            !probe(&fixture.root, "wayland-probe", false)
                .status
                .success(),
            "shared runtime bypassed safety guard"
        );
    }

    #[test]
    fn zoom_converts_pointer_and_pixel_scroll_but_not_wheel_lines() {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(64.0, 32.0),
            )),
            events: vec![
                egui::Event::PointerMoved(egui::pos2(20.0, 10.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(8.0, -4.0),
                    modifiers: Default::default(),
                    phase: egui::TouchPhase::Move,
                },
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, -1.0),
                    modifiers: Default::default(),
                    phase: egui::TouchPhase::Move,
                },
            ],
            ..Default::default()
        };
        logical_input_to_egui_points(&mut input, 2.0);
        assert_eq!(input.screen_rect.unwrap().size(), egui::vec2(32.0, 16.0));
        assert!(
            matches!(input.events[0], egui::Event::PointerMoved(pos) if pos == egui::pos2(10.0, 5.0))
        );
        assert!(
            matches!(input.events[1], egui::Event::MouseWheel {delta, ..} if delta == egui::vec2(4.0, -2.0))
        );
        assert!(
            matches!(input.events[2], egui::Event::MouseWheel {delta, ..} if delta == egui::vec2(0.0, -1.0))
        );
    }

    #[test]
    #[ignore = "requires Wayland, wlr-layer-shell and a WGPU adapter"]
    fn suspended_view_consumes_texture_updates_without_retaining_frame_output() {
        use smithay_client_toolkit::shell::WaylandSurface;
        use std::sync::atomic::Ordering;
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = Application::new(move |token| {
            let _ = tx.send(token);
        });
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            Layer::Top,
            Some("wayapp-suspended-view-output"),
            None,
        );
        let mut surface = EguiSurfaceState::new(&app, &layer, 64, 64);
        surface.configure(&app, 64, 64, Some(WindowState::SUSPENDED));
        for _ in 0..3 {
            let texture = surface.egui_context.load_texture(
                "suspended-view-texture",
                egui::ColorImage::filled([16, 16], egui::Color32::GREEN),
                egui::TextureOptions::default(),
            );
            let id = texture.id();
            surface.render(&mut |_| {});
            assert!(
                surface.last_fulloutput.is_none(),
                "suspended view retained consumed output and texture payloads"
            );
            assert!(
                surface.renderer.texture_is_allocated(id),
                "suspended upload was lost"
            );
            drop(texture);
            surface.render(&mut |_| {});
            assert!(surface.last_fulloutput.is_none());
            assert!(
                !surface.renderer.texture_is_allocated(id),
                "suspended texture was not freed"
            );
            assert!(
                surface.get_frame_timings().is_none(),
                "resource-only update counted as a drawn frame"
            );
        }
        // Retain a live texture while mapping/resuming the real native surface.
        let texture = surface.egui_context.load_texture(
            "survives-suspension",
            egui::ColorImage::filled([16, 16], egui::Color32::GREEN),
            egui::TextureOptions::default(),
        );
        surface.render(&mut |_| {});
        assert!(surface.renderer.texture_is_allocated(texture.id()));
        let status = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(0));
        layer.set_size(64, 64);
        layer.commit();
        app.run_dispatcher();
        let deadline = Instant::now() + Duration::from_secs(5);
        while status.load(Ordering::SeqCst) == 0 {
            let token = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            let events = app.dispatch_pending(token);
            surface.handle_events(&mut app, &events, &mut |ui| {
                ui.image((texture.id(), egui::vec2(16.0, 16.0)));
                ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                    ui.max_rect(),
                    NativeLivenessCallback {
                        surface: layer.wl_surface().clone(),
                        status: status.clone(),
                    },
                ));
            });
        }
        assert!(!surface.suspended);
        assert!(surface.get_frame_timings().is_some());
        assert!(surface.renderer.texture_is_allocated(texture.id()));
        app.conn
            .roundtrip()
            .expect("suspended view disconnected Wayland");
    }

    #[test]
    #[ignore = "requires Wayland, wlr-layer-shell and a WGPU adapter"]
    fn stationary_pointer_tracks_zoom_without_native_motion_and_stays_gone_after_leave() {
        use smithay_client_toolkit::seat::pointer::PointerEventKind;
        let app = Application::new(|_| {});
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            Layer::Top,
            Some("wayapp-stationary-zoom-probe"),
            None,
        );
        let mut surface = EguiSurfaceState::new(&app, &layer, 800, 600);
        let native = surface.wl_surface().clone();
        surface.handle_pointer_event(&PointerEvent {
            surface: native.clone(),
            position: (400.0, 300.0),
            kind: PointerEventKind::Enter { serial: 1 },
        });
        surface.process_egui_frame(&mut |_| {});
        for zoom in [2.0, 0.5, 1.0] {
            surface.egui_context.set_zoom_factor(zoom);
            // egui applies a requested zoom at the start of its next pass.
            // Check the subsequent settled frame without supplying native motion.
            surface.process_egui_frame(&mut |_| {});
            surface.process_egui_frame(&mut |_| {});
            assert_eq!(
                surface
                    .egui_context
                    .input(|input| input.pointer.hover_pos()),
                Some(egui::pos2(400.0 / zoom, 300.0 / zoom))
            );
        }
        surface.handle_pointer_event(&PointerEvent {
            surface: native.clone(),
            position: (400.0, 300.0),
            kind: PointerEventKind::Leave { serial: 2 },
        });
        surface.process_egui_frame(&mut |_| {});
        surface.egui_context.set_zoom_factor(2.0);
        surface.process_egui_frame(&mut |_| {});
        surface.process_egui_frame(&mut |_| {});
        assert_eq!(
            surface
                .egui_context
                .input(|input| input.pointer.hover_pos()),
            None
        );
        surface.handle_pointer_event(&PointerEvent {
            surface: native,
            position: (400.0, 300.0),
            kind: PointerEventKind::Enter { serial: 3 },
        });
        surface.process_egui_frame(&mut |_| {});
        surface
            .input_state
            .handle_pointer_removed(&surface.egui_context);
        surface.process_egui_frame(&mut |_| {});
        surface.egui_context.set_zoom_factor(0.5);
        surface.process_egui_frame(&mut |_| {});
        surface.process_egui_frame(&mut |_| {});
        assert_eq!(
            surface
                .egui_context
                .input(|input| input.pointer.hover_pos()),
            None
        );
    }

    #[test]
    #[ignore = "requires Wayland, wlr-layer-shell and a WGPU adapter"]
    fn egui_zoom_is_preserved_across_frames() {
        let app = Application::new(|_| {});
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            Layer::Top,
            Some("wayapp-zoom-probe"),
            None,
        );
        let mut surface = EguiSurfaceState::new(&app, &layer, 64, 64);
        surface.process_egui_frame(&mut |_| {});
        surface.egui_context.set_zoom_factor(2.0);
        surface.process_egui_frame(&mut |_| {});
        surface.process_egui_frame(&mut |_| {});
        assert_eq!(surface.egui_context.zoom_factor(), 2.0);
        assert_eq!(
            surface.last_fulloutput.as_ref().unwrap().pixels_per_point,
            2.0
        );
        assert_eq!(
            surface.egui_context.input(|i| i.content_rect().width()),
            32.0
        );
        surface.scale_factor_changed(2);
        surface.process_egui_frame(&mut |_| {});
        assert_eq!(surface.egui_context.zoom_factor(), 2.0);
        assert_eq!(
            surface.last_fulloutput.as_ref().unwrap().pixels_per_point,
            4.0
        );
    }

    #[test]
    #[ignore = "requires Wayland, wlr-layer-shell and a WGPU adapter"]
    fn dropping_surface_with_unpainted_output_is_safe() {
        let app = Application::new(|_| {});
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            Layer::Top,
            Some("wayapp-pending-output-drop"),
            None,
        );
        let mut surface = EguiSurfaceState::new(&app, &layer, 64, 64);
        surface.process_egui_frame(&mut |ui| {
            ui.label("pending atlas upload");
        });
        assert!(
            !surface
                .last_fulloutput
                .as_ref()
                .unwrap()
                .textures_delta
                .is_empty()
        );
        drop(surface);
    }

    #[test]
    #[ignore = "requires Wayland, wlr-layer-shell and a WGPU adapter"]
    fn zero_size_configuration_does_not_disconnect_wayland() {
        let app = Application::new(|_| {});
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            Layer::Top,
            Some("wayapp-zero-size-probe"),
            None,
        );
        let mut surface = EguiSurfaceState::new(&app, &layer, 64, 64);
        surface.configure(&app, 0, 0, None);
        assert_eq!((surface.width, surface.height), (64, 64));
        surface.scale_factor_changed(0);
        app.conn
            .roundtrip()
            .expect("zero viewport destination disconnected Wayland");
    }

    #[test]
    #[ignore = "requires a Wayland compositor and WGPU adapter"]
    fn unspecified_window_size_preserves_current_dimensions() {
        let mut app = Application::new(|_| {});
        let window = app.xdg_shell.create_window(
            app.compositor_state.create_surface(&app.qh),
            smithay_client_toolkit::shell::xdg::window::WindowDecorations::ServerDefault,
            &app.qh,
        );
        let mut surface = EguiSurfaceState::new(&app, &window, 0, 0);
        assert_eq!((surface.width, surface.height), (1, 1));
        surface.configure(&app, 128, 96, None);
        let mut configure = smithay_client_toolkit::shell::xdg::window::WindowConfigure::default();
        // Skip GPU presentation here; test size negotiation independently.
        configure.state = WindowState::SUSPENDED;
        surface.handle_events(
            &mut app,
            &[WaylandEvent::WindowConfigure(window.clone(), configure)],
            &mut |_| {},
        );
        assert_eq!((surface.width, surface.height), (128, 96));
        app.conn
            .roundtrip()
            .expect("window configure produced a protocol error");
    }
}

#[cfg(test)]
mod initial_input_size_regression {
    use super::*;
    use crate::test_support as private;
    use smithay_client_toolkit::seat::pointer::PointerEventKind;
    use smithay_client_toolkit::shell::WaylandSurface;
    use smithay_client_toolkit::shell::wlr_layer::Layer;

    #[test]
    #[ignore = "requires private headless Sway and WGPU; SDK/CPU-egui input only"]
    fn first_ui_pass_uses_the_constructor_size_for_layout_and_clicks() {
        let _private = private::private_input_test_guard();
        let _display_guard;
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = Application::new(move |token| {
            let _ = tx.send(token);
        });
        _display_guard = app.conn.clone();
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            Layer::Top,
            Some("wayapp-initial-input-size"),
            None,
        );
        layer.set_size(64, 48);
        layer.commit();
        app.run_dispatcher();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let token = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if app
                .dispatch_pending(token)
                .iter()
                .any(|event| matches!(event, WaylandEvent::LayerShellConfigure(..)))
            {
                break;
            }
        }
        // The native role is already configured. Creating/replacing only its
        // view need not generate another configure event before input arrives.
        let mut view = EguiSurfaceState::new(&app, &layer, 64, 48);
        view.input_state.handle_keyboard_enter();
        let mut bounds = egui::Vec2::ZERO;
        let mut clicked = false;
        let mut ui = |ui: &mut egui::Ui| {
            let content = ui.ctx().input(|i| i.content_rect());
            bounds = content.size();
            let button = egui::Rect::from_center_size(content.center(), egui::vec2(16.0, 16.0));
            clicked |= ui.allocate_rect(button, egui::Sense::click()).clicked();
        };
        view.process_egui_frame(&mut ui);
        for kind in [
            PointerEventKind::Motion { time: 1 },
            PointerEventKind::Press {
                time: 2,
                serial: 2,
                button: 0x110,
            },
            PointerEventKind::Release {
                time: 3,
                serial: 3,
                button: 0x110,
            },
        ] {
            view.handle_pointer_event(&PointerEvent {
                surface: layer.wl_surface().clone(),
                position: (32.0, 24.0),
                kind,
            });
            view.process_egui_frame(&mut ui);
        }
        // No presentation is needed; isolate the separate unapplied-output Drop bug.
        view.last_fulloutput
            .take()
            .unwrap()
            .drop_without_applying_deltas();
        assert_eq!(
            (bounds, clicked),
            (egui::vec2(64.0, 48.0), true),
            "initial adapter dimensions displaced the centered widget"
        );
    }
}

#[cfg(test)]
mod oversized_configure_regression {
    use super::*;
    use crate::test_support as private;
    use smithay_client_toolkit::shell::WaylandSurface;
    use smithay_client_toolkit::shell::wlr_layer::Layer;

    #[test]
    #[ignore = "requires private headless Sway and software WGPU"]
    fn oversized_configure_preserves_resources_and_recovers_at_a_supported_size() {
        private::require_private_compositor();
        const CHILD: &str = "WAYAPP_OVERSIZED_CONFIGURE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "egui::egui_surface_state::oversized_configure_regression::oversized_configure_preserves_resources_and_recovers_at_a_supported_size", "--ignored", "--nocapture"])
                .env(CHILD, "1").stderr(std::process::Stdio::null()).spawn().unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(
                        status.success(),
                        "oversized configure child failed: {status}"
                    );
                    return;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    let _ = child.wait();
                    panic!("oversized configure child exceeded deadline");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let _private = private::private_input_test_guard();
        let _display_guard;
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = Application::new(move |token| {
            let _ = tx.send(token);
        });
        _display_guard = app.conn.clone();
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            Layer::Top,
            Some("wayapp-oversized-configure"),
            None,
        );
        layer.set_size(64, 48);
        layer.commit();
        app.run_dispatcher();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut configure = loop {
            let token = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if let Some(configure) = app.dispatch_pending(token).into_iter().find_map(|event| {
                if let WaylandEvent::LayerShellConfigure(_, configure) = event {
                    Some(configure)
                } else {
                    None
                }
            }) {
                break configure;
            }
        };
        let mut view = EguiSurfaceState::new(&app, &layer, 64, 48);
        view.configure(&app, 64, 48, None);
        view.render(&mut |_| {});
        let dimension = view.renderer.max_adapter_texture_dimension() + 1;
        eprintln!("configure hint exceeds actual adapter limit: {dimension}");
        assert!(
            dimension <= i32::MAX as u32,
            "fixture must stay inside native viewport range"
        );
        for size in [(dimension, 1), (1, dimension)] {
            let texture = view.egui_context.load_texture(
                "oversized-configure",
                egui::ColorImage::filled([2, 2], egui::Color32::RED),
                egui::TextureOptions::default(),
            );
            let id = texture.id();
            // A protocol-valid compositor suggestion, not a direct invalid
            // call into the public low-level rendering API.
            configure.new_size = size;
            let event = WaylandEvent::LayerShellConfigure(layer.clone(), configure.clone());
            view.handle_events(&mut app, &[event], &mut |_| {});
            assert!(view.last_fulloutput.is_none());
            assert!(
                view.renderer.texture_is_allocated(id),
                "oversized frame lost upload"
            );
            drop(texture);
            view.render(&mut |_| {});
            assert!(
                !view.renderer.texture_is_allocated(id),
                "oversized frame lost free"
            );
            app.conn
                .roundtrip()
                .expect("legal oversized viewport disconnected native client");
        }
        configure.new_size = (64, 48);
        let event = WaylandEvent::LayerShellConfigure(layer.clone(), configure);
        view.handle_events(&mut app, &[event], &mut |_| {});
        app.conn
            .roundtrip()
            .expect("supported configure did not recover");
    }
}

#[cfg(test)]
mod repaint_delay_regression {
    use super::*;

    #[test]
    fn egui_max_delay_callback_does_not_panic_or_block_a_later_repaint() {
        let context = Context::default();
        for _ in 0..8 {
            context
                .run_ui(egui::RawInput::default(), |_| {})
                .drop_without_applying_deltas();
        }
        assert!(
            !context.has_requested_repaint(),
            "fixture context must be idle"
        );
        let (frame_tx, frame_rx) = std::sync::mpsc::channel();
        let (delay_tx, delay_rx) = std::sync::mpsc::channel();
        let scheduler = FrameScheduler::new(move || {
            let _ = frame_tx.send(());
        });
        let request = scheduler.create_scheduler();
        context.set_request_repaint_callback(move |info| {
            let _ = delay_tx.send(info.delay);
            request(info.delay);
        });
        context.request_repaint_after(Duration::MAX);
        let delay = delay_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        // egui subtracts predicted frame time before deciding to invoke the
        // backend, so it forwards a finite but unrepresentable Instant delay.
        assert!(delay < Duration::MAX);
        assert!(Instant::now().checked_add(delay).is_none());
        assert!(
            frame_rx.try_recv().is_err(),
            "no-repaint sentinel emitted a frame"
        );
        context.request_repaint();
        assert_eq!(
            delay_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Duration::ZERO
        );
        frame_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("later valid repaint was lost");
    }
}
