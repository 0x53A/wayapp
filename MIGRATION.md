# Migration Notes

This fork includes upstream `Ciantic/wayapp` `main` at `57fc26b` and the
input, rendering, and lifecycle fixes from `test/upstream-regressions`.
The dependency baseline is egui/egui-wgpu 0.36.2, wgpu 30, and
smithay-client-toolkit 0.21. Consumers using egui or native GPU callbacks
need compatible dependency versions.

## Upstream-aligned API changes

- Monitor hot-plug events now use upstream names:
  - `WaylandEvent::OutputAdded(output)` -> `WaylandEvent::OutputCreated(output)`
  - `WaylandEvent::OutputRemoved(output)` -> `WaylandEvent::OutputDestroyed(output)`
  - `WaylandEvent::OutputUpdated(output)` is unchanged
- `Application::outputs()` and `Application::output_info(&WlOutput)` remain available for monitor enumeration and metadata.
- `EguiSurfaceState::wl_surface()` remains public for input-region and click-through handling.
- `EguiSurfaceState::has_keyboard_focus()` remains available for overlay focus checks.
- `WaylandEvent::PointerRemoved` reports pointer-device loss. Exhaustive event
  matches must handle it; direct input-adapter users can call
  `WaylandToEguiInput::handle_pointer_removed` to cancel held pointer state.

## Preserved fork features

- `SurfaceOptions`, `new_transparent`, and `new_with_options` are kept, but now feed into upstream's newer `EguiWgpuRenderer`. `SurfaceOptions::default()` matches upstream's transparent default.
- Requested presentation/alpha modes are retained when supported, including
  WGPU's automatic modes. Unsupported explicit modes fall back to supported
  surface modes.
- Clipboard support is feature-gated and enabled by default through the `clipboard` feature.
- Touchpad and wheel scrolling now preserve each axis's units and fractional
  motion, using the reviewed input conversion instead of the old scaling.
- The `egui_wgpu` re-export and `CallbackTargetFormat` remain available for
  native paint callbacks.

## Superseded fork commits

The previous repaint scheduling and frame-request deduplication commits are not carried forward directly. Upstream now provides the same behavior through `egui::Context::set_request_repaint_callback` and `FrameScheduler`, which coalesces repaint requests by keeping the earliest pending deadline.

Layer-shell configure now renders immediately, superseding the fork's synthetic
first-frame workaround.
