# Migration Notes

This fork has been rebased onto upstream `Ciantic/wayapp` `main` at `4fdd54e`.

## Upstream-aligned API changes

- Monitor hot-plug events now use upstream names:
  - `WaylandEvent::OutputAdded(output)` -> `WaylandEvent::OutputCreated(output)`
  - `WaylandEvent::OutputRemoved(output)` -> `WaylandEvent::OutputDestroyed(output)`
  - `WaylandEvent::OutputUpdated(output)` is unchanged
- `Application::outputs()` and `Application::output_info(&WlOutput)` remain available for monitor enumeration and metadata.
- `EguiSurfaceState::wl_surface()` remains public for input-region and click-through handling.
- `EguiSurfaceState::has_keyboard_focus()` remains available for overlay focus checks.

## Preserved fork features

- `SurfaceOptions`, `new_transparent`, and `new_with_options` are kept, but now feed into upstream's newer `EguiWgpuRenderer`. `SurfaceOptions::default()` matches upstream's transparent default.
- Clipboard support is feature-gated and enabled by default through the `clipboard` feature.
- Touchpad scrolling still prefers `value120`, then absolute deltas, then discrete wheel steps.

## Superseded fork commits

The previous repaint scheduling and frame-request deduplication commits are not carried forward directly. Upstream now provides the same behavior through `egui::Context::set_request_repaint_callback` and `FrameScheduler`, which coalesces repaint requests by keeping the earliest pending deadline.
