use egui::Context;
use egui_wgpu::Renderer;
use egui_wgpu::RendererOptions;
use egui_wgpu::ScreenDescriptor;
use egui_wgpu::wgpu;
use egui_wgpu::wgpu::Device;
use egui_wgpu::wgpu::Queue;
use egui_wgpu::wgpu::StoreOp;
use egui_wgpu::wgpu::Surface;
use egui_wgpu::wgpu::SurfaceConfiguration;
use egui_wgpu::wgpu::TextureFormat;
use raw_window_handle::RawDisplayHandle;
use raw_window_handle::RawWindowHandle;
use raw_window_handle::WaylandDisplayHandle;
use raw_window_handle::WaylandWindowHandle;
use std::ptr::NonNull;
use wayland_client::Connection;
use wayland_client::Proxy;
use wayland_client::protocol::wl_surface::WlSurface;

// How this works:
// 1. `new()` creates a wgpu Instance, Device, Queue, and the egui Renderer. The
//    WlSurface + Connection are saved so the surface can be recreated.
// 2. `suspend()` drops only the wgpu Surface (the swapchain), freeing GPU
//    memory. The Device, Queue, and egui Renderer stay alive — dropping the
//    egui Renderer would lose texture state and panic on the next frame.
// 3. `resume()` recreates the wgpu Surface from the saved WlSurface, using the
//    *same* Instance (a surface from a new Instance can't find the Device).
//    Width/height are left at 0 to force a reconfigure on the next render.
// 4. `render_to_wgpu()` acquires the next swapchain image, clears it, then
//    draws the EGUI shapes / textures and presents. Skips silently if
//    suspended.

/// WGPU renderer for EGUI.
pub struct EguiWgpuRenderer {
    egui_context: Context,
    egui_renderer: Renderer,

    // Fields are dropped in declaration order. `wgpu_surface` must come before
    // `wgpu_device`, and `wgpu_device` before `wgpu_instance` — otherwise the
    // surface cleanup will panic trying to access a device that no longer
    // exists.
    wgpu_surface: Option<Surface<'static>>,
    wgpu_device: Device,
    wgpu_queue: Queue,
    wgpu_surface_config: Option<SurfaceConfiguration>,
    wgpu_instance: wgpu::Instance,
    output_format: TextureFormat,
    present_mode: wgpu::PresentMode,
    alpha_mode: wgpu::CompositeAlphaMode,
    width: u32,
    height: u32,
    wl_surface: WlSurface,
    wl_conn: Connection,
    // Only high-level views set this; standalone low-level callers keep
    // managing their own native buffer scale.
    buffer_scale: Option<i32>,
    #[cfg(test)]
    pub(crate) acquisition_override: std::collections::VecDeque<wgpu::CurrentSurfaceTexture>,
}

#[derive(Debug)]
struct NativeDisplay(Connection);

impl raw_window_handle::HasDisplayHandle for NativeDisplay {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        let raw = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            NonNull::new(self.0.backend().display_ptr().cast())
                .expect("Wayland display pointer was null"),
        ));
        // SAFETY: this Connection owns the display for the borrowed handle's lifetime.
        Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(raw) })
    }
}

impl EguiWgpuRenderer {
    #[cfg(test)]
    pub(crate) fn texture_is_allocated(&self, id: egui::TextureId) -> bool {
        self.egui_renderer.texture(&id).is_some()
    }

    #[cfg(test)]
    pub(crate) fn max_adapter_texture_dimension(&self) -> u32 {
        futures::executor::block_on(self.wgpu_instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                compatible_surface: self.wgpu_surface.as_ref(),
                ..Default::default()
            },
        ))
        .expect("fixture needs the renderer's compatible adapter")
        .limits()
        .max_texture_dimension_2d
    }

    /// Create a low-level renderer for a live native surface.
    ///
    /// The native surface must remain alive until this renderer is dropped:
    /// cloning a `WlSurface` proxy does not retain a role's RAII owner or
    /// prevent explicit destruction. `EguiSurfaceState` retains its native
    /// role owners through renderer cleanup for the normal owned-surface
    /// path.
    pub fn new(
        egui_context: &Context,
        wl_surface: &WlSurface,
        conn: &Connection,
    ) -> EguiWgpuRenderer {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..wgpu::InstanceDescriptor::new_with_display_handle(Box::new(NativeDisplay(
                conn.clone(),
            )))
        });

        let surface = Self::create_wgpu_surface(&instance, conn, wl_surface);

        let adapter =
            futures::executor::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            }))
            .expect("Failed to find a suitable adapter");

        let (wgpu_device, wgpu_queue) =
            futures::executor::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                // Match the adapter's texture dimensions, including adapters
                // below default limits and large/high-DPI desktop surfaces.
                required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
                ..Default::default()
            }))
            .expect("Failed to request WGPU device");

        let caps = surface.get_capabilities(&adapter);
        let (present_mode, alpha_mode) = preferred_surface_modes(&caps);
        let output_format = *caps
            .formats
            .get(0)
            .unwrap_or(&wgpu::TextureFormat::Bgra8Unorm);

        let egui_renderer = Renderer::new(
            &wgpu_device,
            output_format,
            RendererOptions {
                msaa_samples: 1,
                depth_stencil_format: None,
                ..Default::default()
            },
        );

        EguiWgpuRenderer {
            egui_context: egui_context.clone(),
            egui_renderer,
            wgpu_surface: Some(surface),
            wgpu_device,
            wgpu_queue,
            wgpu_surface_config: None,
            output_format,
            present_mode,
            alpha_mode,
            width: 0,
            height: 0,
            wl_surface: wl_surface.clone(),
            wl_conn: conn.clone(),
            wgpu_instance: instance,
            buffer_scale: None,
            #[cfg(test)]
            acquisition_override: Default::default(),
        }
    }

    /// Suspend the renderer — drops the WGPU surface and configuration to
    /// free GPU resources. The device, queue, and egui renderer are kept
    /// alive to preserve texture state.
    pub fn suspend(&mut self) {
        if self.wgpu_surface.is_some() {
            log::trace!("[EGUI] Suspending WGPU surface to free GPU resources");
            self.wgpu_surface = None;
            self.wgpu_surface_config = None;
            self.width = 0;
            self.height = 0;
        }
    }

    pub(crate) fn set_buffer_scale(&mut self, scale: i32) {
        self.buffer_scale = Some(scale);
    }

    /// Resume the renderer — recreates the WGPU surface from the saved
    /// Wayland surface handle, using the same Instance that owns the Device.
    pub fn resume(&mut self) {
        if self.wgpu_surface.is_none() {
            log::trace!("[EGUI] Resuming WGPU surface");
            self.wgpu_surface = Some(Self::create_wgpu_surface(
                &self.wgpu_instance,
                &self.wl_conn,
                &self.wl_surface,
            ));
        }
    }

    /// Create a WGPU surface from Wayland connection and surface.
    fn create_wgpu_surface(
        instance: &wgpu::Instance,
        conn: &Connection,
        wl_surface: &WlSurface,
    ) -> Surface<'static> {
        let raw_display_handle = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            NonNull::new(conn.backend().display_ptr() as *mut _)
                .expect("Wayland display pointer was null"),
        ));
        let raw_window_handle = RawWindowHandle::Wayland(WaylandWindowHandle::new(
            NonNull::new(wl_surface.id().as_ptr() as *mut _)
                .expect("Wayland surface handle was null"),
        ));
        // SAFETY: conn retains the native display. The native surface's owner
        // must outlive this GPU surface (see new); a cloned proxy alone does
        // not guarantee native liveness. EguiSurfaceState preserves that order.
        //
        // We must use SurfaceTargetUnsafe::RawHandle because the safe
        // SurfaceTarget enum has no Wayland variant — DisplayAndWindow
        // requires HasDisplayHandle + HasWindowHandle, which Wayland
        // proxy types don't implement.
        unsafe {
            instance
                .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                    raw_display_handle: Some(raw_display_handle),
                    raw_window_handle,
                })
                .expect("Failed to create WGPU surface")
        }
    }

    /// Resize and reconfigure the WGPU surface
    fn reconfigure_surface(&mut self, width: u32, height: u32) {
        if self.wgpu_surface.is_none() {
            return; // A future surface must still be configured after resume.
        }
        let width = width.max(1);
        let height = height.max(1);
        self.width = width;
        self.height = height;
        let config = SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: self.output_format,
            width,
            height,
            present_mode: self.present_mode,
            alpha_mode: self.alpha_mode,
            color_space: wgpu::SurfaceColorSpace::Auto,
            view_formats: vec![self.output_format],
            desired_maximum_frame_latency: 2,
        };
        if let Some(surface) = &self.wgpu_surface {
            surface.configure(&self.wgpu_device, &config);
        }
        self.wgpu_surface_config = Some(config);
    }

    /// Acquire the next surface texture
    ///
    /// Follows the recommended pattern for handling surface acquisition:
    /// https://docs.rs/wgpu/latest/wgpu/enum.CurrentSurfaceTexture.html
    fn acquire_surface_texture(&mut self) -> Option<wgpu::SurfaceTexture> {
        {
            if self.wgpu_surface.is_none() {
                log::trace!("[EGUI] Skipping render, surface is suspended");
                return None;
            }

            match self.current_surface_texture() {
                wgpu::CurrentSurfaceTexture::Success(texture) => return Some(texture),
                wgpu::CurrentSurfaceTexture::Suboptimal(_texture) => {
                    log::warn!(
                        "[EGUI] Surface texture is suboptimal, reconfiguring surface and retrying"
                    );
                }
                wgpu::CurrentSurfaceTexture::Outdated => {
                    log::trace!("[EGUI] Surface configuration is outdated, reconfiguring");
                }
                wgpu::CurrentSurfaceTexture::Lost => {
                    log::warn!("[EGUI] Surface was lost, recreating and reconfiguring");
                    self.wgpu_surface = Some(Self::create_wgpu_surface(
                        &self.wgpu_instance,
                        &self.wl_conn,
                        &self.wl_surface,
                    ));

                    // TODO: From the docs, quote:
                    //
                    // https://docs.rs/wgpu/latest/wgpu/enum.CurrentSurfaceTexture.html
                    // > If the device as a whole is lost (see
                    // > set_device_lost_callback()), then you need to recreate
                    // > the device and all resources. Otherwise, call
                    // > Instance::create_surface() to recreate the surface,
                    // > then Surface::configure(), and try again.
                }
                wgpu::CurrentSurfaceTexture::Timeout => {
                    log::trace!("[EGUI] Surface acquisition timed out, skipping frame");
                    self.request_acquisition_retry();
                    return None;
                }
                wgpu::CurrentSurfaceTexture::Occluded => {
                    log::trace!("[EGUI] Surface is occluded, skipping frame");
                    return None;
                }
                wgpu::CurrentSurfaceTexture::Validation => {
                    log::warn!("[EGUI] Surface acquisition failed validation");
                    return None;
                }
            }
        }

        self.reconfigure_surface(self.width, self.height);

        match self.current_surface_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) => Some(texture),
            wgpu::CurrentSurfaceTexture::Suboptimal(texture) => {
                log::warn!("[EGUI] Surface texture is suboptimal after retry, presenting anyway");
                Some(texture)
            }
            retry => {
                log::warn!(
                    "[EGUI] Failed to acquire surface texture after retry: {:?}, skipping frame",
                    retry
                );
                if matches!(
                    retry,
                    wgpu::CurrentSurfaceTexture::Timeout
                        | wgpu::CurrentSurfaceTexture::Outdated
                        | wgpu::CurrentSurfaceTexture::Lost
                ) {
                    self.request_acquisition_retry();
                }
                None
            }
        }
    }

    fn request_acquisition_retry(&self) {
        // This frame's shapes are consumed even on a transient acquisition
        // failure. Wake an idle UI to rebuild them, without an immediate spin.
        // Occlusion, suspension and validation failures do not use this retry.
        self.egui_context
            .request_repaint_after(std::time::Duration::from_millis(100));
    }

    fn current_surface_texture(&mut self) -> wgpu::CurrentSurfaceTexture {
        #[cfg(test)]
        if let Some(result) = self.acquisition_override.pop_front() {
            return result;
        }
        self.wgpu_surface
            .as_ref()
            .expect("live surface during acquisition")
            .get_current_texture()
    }

    /// Renders EGUI output to the WGPU surface
    /// Skips drawing when suspended or outside the device's
    /// texture limits. Texture deltas are still applied so a later valid
    /// frame recovers. Transient acquisition failures request a deferred egui
    /// repaint so otherwise-idle content can retry without an immediate spin.
    pub fn render_to_wgpu(
        &mut self,
        mut egui_fulloutput: egui::FullOutput,
        width: u32,
        height: u32,
        pixels_per_point: f32,
    ) {
        // Egui emits texture deltas once. Apply uploads even if acquisition is
        // skipped; otherwise the next frame refers to textures never uploaded.
        for (id, image_deltas) in &egui_fulloutput.textures_delta.set {
            for image_delta in image_deltas {
                self.egui_renderer.update_texture(
                    &self.wgpu_device,
                    &self.wgpu_queue,
                    *id,
                    image_delta,
                );
            }
        }
        let max_dimension = self.wgpu_device.limits().max_texture_dimension_2d;
        if width > max_dimension || height > max_dimension {
            log::trace!(
                "[EGUI] Skipping unrenderable frame {width}x{height} at {pixels_per_point} \
                 pixels/point"
            );
            self.finish_skipped_frame(&mut egui_fulloutput.textures_delta);
            return;
        }
        // EGUI Screen descriptor for this frame
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [width, height],
            pixels_per_point,
        };

        // Reconfigure if size changed (must happen before borrowing surface)
        let needs_reconfig = (width != self.width) || (height != self.height);
        if needs_reconfig {
            self.reconfigure_surface(width, height);
        }

        let surface_texture = match self.acquire_surface_texture() {
            Some(texture) => texture,
            None => {
                self.finish_skipped_frame(&mut egui_fulloutput.textures_delta);
                return;
            }
        };

        let texture_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.wgpu_device.create_command_encoder(&Default::default());

        // Clear pass
        let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("egui clear pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &texture_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        // Draw EGUI shapes with WGPU
        let tris = self
            .egui_context
            .tessellate(egui_fulloutput.shapes, egui_fulloutput.pixels_per_point);

        let callback_commands = self.egui_renderer.update_buffers(
            &self.wgpu_device,
            &self.wgpu_queue,
            &mut encoder,
            &tris,
            &screen_descriptor,
        );

        // Render pass to draw EGUI output to the surface
        let rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &texture_view,
                resolve_target: None,
                depth_slice: None,
                ops: egui_wgpu::wgpu::Operations {
                    load: egui_wgpu::wgpu::LoadOp::Load,
                    store: StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            label: Some("egui main render pass"),
            occlusion_query_set: None,
            multiview_mask: None,
        });

        // Render before freeing textures so they remain available to this frame.
        self.egui_renderer
            .render(&mut rpass.forget_lifetime(), &tris, &screen_descriptor);

        // Submit before freeing textures because the commands may still reference them.
        self.wgpu_queue
            .submit(callback_commands.into_iter().chain([encoder.finish()]));
        self.free_textures(&mut egui_fulloutput.textures_delta);

        // Apply scale with the new buffer, not during a skipped frame or while
        // the previous (possibly odd-sized) buffer could be committed again.
        if let Some(scale) = self.buffer_scale {
            self.wl_surface.set_buffer_scale(scale);
        }
        // Present after submitting the frame.
        self.wgpu_queue.present(surface_texture);
    }

    fn finish_skipped_frame(&mut self, delta: &mut egui::TexturesDelta) {
        if !delta.set.is_empty() {
            // write_texture queues staging allocations and retains textures until
            // submission. Flush uploads even if no surface will be presented;
            // freeing egui's texture entries or polling the device is not enough.
            self.wgpu_queue.submit([]);
        }
        self.free_textures(delta);
    }

    fn free_textures(&mut self, delta: &mut egui::TexturesDelta) {
        for id in &delta.free {
            self.egui_renderer.free_texture(id);
        }
        delta.clear();
    }
}

fn preferred_surface_modes(
    caps: &wgpu::SurfaceCapabilities,
) -> (wgpu::PresentMode, wgpu::CompositeAlphaMode) {
    // Mailbox and premultiplied alpha are preferences, not universal surface
    // capabilities. FIFO is guaranteed; otherwise use an advertised alpha mode.
    let present = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
        wgpu::PresentMode::Mailbox
    } else {
        wgpu::PresentMode::Fifo
    };
    let alpha = if caps
        .alpha_modes
        .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
    {
        wgpu::CompositeAlphaMode::PreMultiplied
    } else {
        caps.alpha_modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Auto)
    };
    (present, alpha)
}

#[cfg(test)]
mod surface_capability_tests {
    use super::*;

    #[test]
    fn fifo_opaque_surface_does_not_require_mailbox_or_transparency() {
        let caps = wgpu::SurfaceCapabilities {
            present_modes: vec![wgpu::PresentMode::Fifo],
            alpha_modes: vec![wgpu::CompositeAlphaMode::Opaque],
            ..Default::default()
        };
        assert_eq!(
            preferred_surface_modes(&caps),
            (wgpu::PresentMode::Fifo, wgpu::CompositeAlphaMode::Opaque)
        );
    }

    #[test]
    fn preferred_modes_are_selected_when_supported() {
        let caps = wgpu::SurfaceCapabilities {
            present_modes: vec![wgpu::PresentMode::Fifo, wgpu::PresentMode::Mailbox],
            alpha_modes: vec![
                wgpu::CompositeAlphaMode::Opaque,
                wgpu::CompositeAlphaMode::PreMultiplied,
            ],
            ..Default::default()
        };
        assert_eq!(
            preferred_surface_modes(&caps),
            (
                wgpu::PresentMode::Mailbox,
                wgpu::CompositeAlphaMode::PreMultiplied
            )
        );
    }
}

#[cfg(test)]
mod regression_probes {
    use super::*;
    use smithay_client_toolkit::shell::WaylandSurface;
    use std::time::Duration;
    use std::time::Instant;

    fn idle_output(context: &Context) -> egui::FullOutput {
        for _ in 0..8 {
            context
                .run_ui(egui::RawInput::default(), |_| {})
                .drop_without_applying_deltas();
        }
        let output = context.run_ui(egui::RawInput::default(), |_| {});
        assert!(!context.has_requested_repaint(), "test UI is not idle");
        output
    }

    #[test]
    #[ignore = "requires Wayland, wlr-layer-shell and a WGPU adapter"]
    fn transient_acquisition_failure_requests_a_deferred_repaint_and_recovers() {
        let (_app, _layer, context, mut renderer) = configured_renderer();
        let (tx, rx) = std::sync::mpsc::channel();
        context.set_request_repaint_callback(move |request| {
            let _ = tx.send(request.delay);
        });
        use wgpu::CurrentSurfaceTexture::Lost;
        use wgpu::CurrentSurfaceTexture::Outdated;
        use wgpu::CurrentSurfaceTexture::Timeout;
        for failures in [
            vec![Timeout],
            vec![Outdated, Timeout],
            vec![Outdated, Outdated],
            vec![Outdated, Lost],
        ] {
            let output = idle_output(&context);
            while rx.try_recv().is_ok() {}
            // Controlled acquisition results, not a real driver/device fault.
            renderer.acquisition_override = failures.into();
            renderer.render_to_wgpu(output, 64, 64, 1.0);
            let delay = rx
                .try_recv()
                .expect("transient acquisition failure left an idle UI asleep");
            assert!(delay > Duration::ZERO && delay <= Duration::from_millis(100));
            drop(
                renderer
                    .acquire_surface_texture()
                    .expect("native acquisition did not recover"),
            );
        }
    }

    #[test]
    #[ignore = "requires Wayland, wlr-layer-shell and a WGPU adapter"]
    fn intentional_or_nontransient_skips_do_not_start_a_retry_loop() {
        let (_app, _layer, context, mut renderer) = configured_renderer();
        let (tx, rx) = std::sync::mpsc::channel();
        context.set_request_repaint_callback(move |request| {
            let _ = tx.send(request.delay);
        });
        use wgpu::CurrentSurfaceTexture::Occluded;
        use wgpu::CurrentSurfaceTexture::Outdated;
        use wgpu::CurrentSurfaceTexture::Validation;
        for failures in [
            vec![Occluded],
            vec![Validation],
            vec![Outdated, Occluded],
            vec![Outdated, Validation],
        ] {
            let output = idle_output(&context);
            while rx.try_recv().is_ok() {}
            renderer.acquisition_override = failures.into();
            renderer.render_to_wgpu(output, 64, 64, 1.0);
            assert!(
                rx.try_recv().is_err(),
                "nontransient skip scheduled a retry"
            );
        }
        renderer.suspend();
        let output = idle_output(&context);
        while rx.try_recv().is_ok() {}
        renderer.render_to_wgpu(output, 64, 64, 1.0);
        assert!(rx.try_recv().is_err(), "suspension scheduled a retry");
    }

    struct CopyCallback(wgpu::Buffer);

    impl egui_wgpu::CallbackTrait for CopyCallback {
        fn prepare(
            &self,
            device: &Device,
            _: &Queue,
            _: &ScreenDescriptor,
            _: &mut wgpu::CommandEncoder,
            _: &mut egui_wgpu::CallbackResources,
        ) -> Vec<wgpu::CommandBuffer> {
            let source = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("callback sentinel source"),
                size: 4,
                usage: wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: true,
            });
            source
                .slice(..)
                .get_mapped_range_mut()
                .unwrap()
                .copy_from_slice(&42u32.to_ne_bytes());
            source.unmap();
            let mut encoder = device.create_command_encoder(&Default::default());
            encoder.copy_buffer_to_buffer(&source, 0, &self.0, 0, 4);
            vec![encoder.finish()]
        }

        fn paint(
            &self,
            _: egui::epaint::PaintCallbackInfo,
            _: &mut wgpu::RenderPass<'static>,
            _: &egui_wgpu::CallbackResources,
        ) {
        }
    }

    fn configured_renderer() -> (
        crate::Application,
        smithay_client_toolkit::shell::wlr_layer::LayerSurface,
        Context,
        EguiWgpuRenderer,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = crate::Application::new(move |token| {
            let _ = tx.send(token);
        });
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            smithay_client_toolkit::shell::wlr_layer::Layer::Top,
            Some("wayapp-command-buffer-probe"),
            None,
        );
        layer.set_size(64, 64);
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
                .any(|event| matches!(event, crate::WaylandEvent::LayerShellConfigure(..)))
            {
                break;
            }
        }
        let context = Context::default();
        let renderer = EguiWgpuRenderer::new(&context, layer.wl_surface(), &app.conn);
        (app, layer, context, renderer)
    }

    #[test]
    #[ignore = "requires Wayland, wlr-layer-shell and a WGPU adapter"]
    fn custom_callback_command_buffers_are_submitted() {
        let (_app, _layer, context, mut renderer) = configured_renderer();
        assert_callback_submitted(&context, &mut renderer);
    }

    fn assert_callback_submitted(context: &Context, renderer: &mut EguiWgpuRenderer) {
        let readback = renderer.wgpu_device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("callback sentinel readback"),
            size: 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(32.0, 32.0));
        let mut output = context.run_ui(egui::RawInput::default(), |_| {});
        output.shapes = vec![egui::epaint::ClippedShape {
            clip_rect: rect,
            shape: egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(
                rect,
                CopyCallback(readback.clone()),
            )),
        }];
        renderer.render_to_wgpu(output, 64, 64, 1.0);
        let (tx, rx) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        renderer
            .wgpu_device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(5)),
            })
            .unwrap();
        rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        let bytes = readback.slice(..).get_mapped_range().unwrap();
        let sentinel = u32::from_ne_bytes(bytes[..4].try_into().unwrap());
        drop(bytes);
        readback.unmap();
        assert_eq!(sentinel, 42, "custom callback GPU commands were discarded");
    }

    #[test]
    #[ignore = "requires Wayland, wlr-layer-shell and a WGPU adapter"]
    fn repeated_suspend_resume_preserves_textures_and_reconfigures_surface() {
        let (app, _layer, context, mut renderer) = configured_renderer();
        let texture = context.load_texture(
            "persistent-resume-texture",
            egui::ColorImage::filled([2, 2], egui::Color32::RED),
            egui::TextureOptions::default(),
        );
        let output = context.run_ui(egui::RawInput::default(), |_| {});
        renderer.render_to_wgpu(output, 64, 64, 1.0);
        for cycle in 0..3 {
            renderer.suspend();
            let output = context.run_ui(egui::RawInput::default(), |ui| {
                ui.label(format!("suspended cycle {cycle}"));
            });
            renderer.render_to_wgpu(output, 64, 64, 1.0);
            assert!(renderer.wgpu_surface_config.is_none());
            renderer.resume();
            let output = context.run_ui(egui::RawInput::default(), |ui| {
                ui.label("resumed");
            });
            renderer.render_to_wgpu(output, 64, 64, 1.0);
            assert!(renderer.wgpu_surface_config.is_some());
            assert_eq!((renderer.width, renderer.height), (64, 64));
            assert!(renderer.egui_renderer.texture(&texture.id()).is_some());
            renderer
                .wgpu_device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(Duration::from_secs(5)),
                })
                .unwrap();
            app.conn
                .roundtrip()
                .expect("resumed renderer disconnected Wayland");
        }
    }

    #[test]
    #[ignore = "requires Wayland, wlr-layer-shell and a WGPU adapter"]
    fn oversized_frames_preserve_textures_and_recover() {
        let (app, _layer, context, mut renderer) = configured_renderer();
        let output = context.run_ui(egui::RawInput::default(), |_| {});
        renderer.render_to_wgpu(output, 64, 64, 1.0);
        let too_wide = renderer.wgpu_device.limits().max_texture_dimension_2d + 1;
        // Compositor-suggested extents can exceed GPU texture limits.
        for (width, height) in [(too_wide, 1), (1, too_wide)] {
            let texture = context.load_texture(
                "invalid-size-probe",
                egui::ColorImage::filled([2, 2], egui::Color32::GREEN),
                egui::TextureOptions::default(),
            );
            let id = texture.id();
            let output = context.run_ui(egui::RawInput::default(), |_| {});
            renderer.render_to_wgpu(output, width, height, 1.0);
            assert_eq!(
                (renderer.width, renderer.height),
                (64, 64),
                "an invalid frame replaced the last valid surface size"
            );
            assert!(
                renderer.egui_renderer.texture(&id).is_some(),
                "invalid frame lost its upload"
            );
            drop(texture);
            let output = context.run_ui(egui::RawInput::default(), |_| {});
            renderer.render_to_wgpu(output, width, height, 1.0);
            assert!(
                renderer.egui_renderer.texture(&id).is_none(),
                "invalid frame retained a freed texture"
            );
        }
        // Verify actual GPU execution after recovery, not just a stale config.
        assert_callback_submitted(&context, &mut renderer);
        app.conn
            .roundtrip()
            .expect("invalid frame size disconnected Wayland");
    }

    #[test]
    #[ignore = "requires private Wayland compositor and GPU allocator reports (e.g. Vulkan)"]
    fn skipped_frames_release_upload_allocations_without_presenting() {
        for skip in ["suspended", "oversized", "timeout", "occluded"] {
            let (_app, _layer, context, mut renderer) = configured_renderer();
            let output = context.run_ui(egui::RawInput::default(), |_| {});
            renderer.render_to_wgpu(output, 64, 64, 1.0);
            let wait = |renderer: &EguiWgpuRenderer| {
                renderer
                    .wgpu_device
                    .poll(wgpu::PollType::Wait {
                        submission_index: None,
                        timeout: Some(Duration::from_secs(5)),
                    })
                    .unwrap();
            };
            wait(&renderer);
            let Some(baseline) = renderer.wgpu_device.generate_allocator_report() else {
                eprintln!("allocator reporting unavailable; skipping allocation regression");
                return;
            };
            if skip == "suspended" {
                renderer.suspend();
            }
            let width = if skip == "oversized" {
                renderer.wgpu_device.limits().max_texture_dimension_2d + 1
            } else {
                64
            };
            let mut skip_frame = |output| {
                match skip {
                    "timeout" => renderer
                        .acquisition_override
                        .push_back(wgpu::CurrentSurfaceTexture::Timeout),
                    "occluded" => renderer
                        .acquisition_override
                        .push_back(wgpu::CurrentSurfaceTexture::Occluded),
                    _ => {}
                }
                renderer.render_to_wgpu(output, width, 64, 1.0);
            };
            for _ in 0..8 {
                let texture = context.load_texture(
                    "skipped-upload-allocation",
                    egui::ColorImage::filled([512, 512], egui::Color32::RED),
                    egui::TextureOptions::default(),
                );
                let output = context.run_ui(egui::RawInput::default(), |_| {});
                skip_frame(output);
                drop(texture);
            }
            // Deliver the last free, without recovery or a test-side submit.
            skip_frame(context.run_ui(egui::RawInput::default(), |_| {}));
            wait(&renderer);
            let after = renderer.wgpu_device.generate_allocator_report().unwrap();
            assert!(
                after.total_allocated_bytes <= baseline.total_allocated_bytes + 2 * 1024 * 1024,
                "{skip} retained queued uploads: {} -> {} live bytes",
                baseline.total_allocated_bytes,
                after.total_allocated_bytes,
            );
        }
    }

    /// Texture uploads and frees must be applied even when no frame is drawn.
    #[test]
    #[ignore = "requires a Wayland compositor and WGPU adapter"]
    fn skipped_surface_preserves_texture_updates() {
        let app = crate::Application::new(|_| {});
        let wl_surface = app.compositor_state.create_surface(&app.qh);
        let context = Context::default();
        let texture = context.load_texture(
            "skipped-surface-probe",
            egui::ColorImage::filled([2, 2], egui::Color32::RED),
            egui::TextureOptions::default(),
        );
        let id = texture.id();
        let mut renderer = EguiWgpuRenderer::new(&context, &wl_surface, &app.conn);
        // Force acquire_surface_texture to return None deterministically.
        // This is the same renderer branch used for Timeout/Occluded/etc.
        renderer.suspend();
        let first = context.run_ui(egui::RawInput::default(), |ui| {
            ui.label("probe");
        });
        assert!(first.textures_delta.set.contains_key(&id));
        renderer.render_to_wgpu(first, 64, 64, 1.0);
        assert!(renderer.egui_renderer.texture(&id).is_some());
        // Rendering while suspended must not install a surface configuration:
        // resume still needs to configure the newly created surface.
        assert!(renderer.wgpu_surface_config.is_none());
        assert_eq!((renderer.width, renderer.height), (0, 0));
        let mut next = context.run_ui(egui::RawInput::default(), |ui| {
            ui.label("probe");
        });
        let resent = next.textures_delta.set.contains_key(&id);
        next.textures_delta.clear();
        assert!(!resent);
        drop(texture);
        let freeing = context.run_ui(egui::RawInput::default(), |_| {});
        renderer.render_to_wgpu(freeing, 64, 64, 1.0);
        assert!(renderer.egui_renderer.texture(&id).is_none());
    }
}

#[cfg(test)]
mod adapter_limits_regression {
    use super::*;
    use smithay_client_toolkit::shell::WaylandSurface;
    use smithay_client_toolkit::shell::wlr_layer::Layer;

    #[test]
    #[ignore = "requires Wayland and a WGPU adapter exceeding default texture limits"]
    fn desktop_surface_can_exceed_default_device_texture_limits() {
        // Retain the display separately to isolate Application's teardown bug.
        let _display_guard;
        let app = crate::Application::new(|_| {});
        _display_guard = app.conn.clone();
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            Layer::Top,
            Some("wayapp-adapter-limits"),
            None,
        );
        let context = Context::default();
        let mut renderer = EguiWgpuRenderer::new(&context, layer.wl_surface(), &app.conn);
        let adapter = futures::executor::block_on(renderer.wgpu_instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                compatible_surface: renderer.wgpu_surface.as_ref(),
                ..Default::default()
            },
        ))
        .unwrap();
        let adapter_limit = adapter.limits().max_texture_dimension_2d;
        let default_limit = wgpu::Limits::default().max_texture_dimension_2d;
        assert!(
            adapter_limit > default_limit,
            "fixture needs an adapter exceeding {default_limit}; got {adapter_limit}"
        );
        let width = default_limit + 1;
        assert!(
            renderer.wgpu_device.limits().max_texture_dimension_2d >= width,
            "adapter supports {adapter_limit}, but default device limit rejects width {width}"
        );
        // Configure only a wide, one-pixel-tall swapchain. Do not map a large
        // desktop window or allocate a square maximum-dimension texture.
        renderer.reconfigure_surface(width, 1);
        assert_eq!(renderer.wgpu_surface_config.as_ref().unwrap().width, width);
        renderer.reconfigure_surface(64, 64);
        app.conn
            .roundtrip()
            .expect("surface configuration damaged the connection");
    }
}

#[cfg(test)]
mod native_surface_modes_regression {
    use super::*;
    use crate::test_support as private;
    use smithay_client_toolkit::shell::WaylandSurface;
    use smithay_client_toolkit::shell::wlr_layer::Layer;
    use std::time::Duration;
    use std::time::Instant;

    #[test]
    #[ignore = "requires private headless Sway and GLES-only software environment"]
    fn ordinary_gles_surface_uses_supported_configuration_modes() {
        private::require_private_compositor();
        const CHILD: &str = "WAYAPP_TEST_GLES_ONLY";
        if std::env::var_os(CHILD).is_none() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "egui::egui_wgpu_renderer::native_surface_modes_regression::ordinary_gles_surface_uses_supported_configuration_modes", "--ignored", "--nocapture"])
                .env(CHILD, "1")
                .env("VK_LOADER_DRIVERS_DISABLE", "*")
                .env("LIBGL_ALWAYS_SOFTWARE", "1")
                .spawn().unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(
                        status.success(),
                        "native GLES configuration child failed: {status}"
                    );
                    return;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    let _ = child.wait();
                    panic!("native GLES configuration child exceeded deadline");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let _private = private::private_input_test_guard();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapters =
            futures::executor::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
        let infos: Vec<_> = adapters.iter().map(|a| a.get_info()).collect();
        assert!(!infos.is_empty(), "no adapter in GLES fixture");
        assert!(
            infos
                .iter()
                .all(|info| info.device_type == wgpu::DeviceType::Cpu),
            "GLES fixture must use software rendering: {infos:?}"
        );
        assert!(
            infos.iter().all(|info| info.backend == wgpu::Backend::Gl),
            "fixture must disable other backends, not silently exercise Vulkan: {infos:?}"
        );
        eprintln!("real adapter(s): {infos:?}");
        let _display_guard;
        let app = crate::Application::new(|_| {});
        _display_guard = app.conn.clone();
        let layer = app.layer_shell.create_layer_surface(
            &app.qh,
            app.compositor_state.create_surface(&app.qh),
            Layer::Top,
            Some("wayapp-native-surface-modes"),
            None,
        );
        let context = Context::default();
        let mut renderer = EguiWgpuRenderer::new(&context, layer.wl_surface(), &app.conn);
        renderer.reconfigure_surface(64, 64);
        app.conn
            .roundtrip()
            .expect("supported mode configuration failed");
    }
}
