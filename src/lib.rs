mod application;
// mod egui;
mod egui;
mod frame_scheduler;
mod kind;
mod single_color;

pub use application::*;
// pub use egui::*;
pub use egui::*;
/// Re-export the exact egui-wgpu version used by the renderer so applications
/// can provide native paint callbacks without accidentally depending on an
/// ABI-incompatible duplicate.
pub use egui_wgpu;
pub(crate) use frame_scheduler::*;
pub use kind::*;
pub use single_color::*;
pub use smithay_client_toolkit::output::OutputInfo;
pub use wayland_client::Proxy;
pub use wayland_client::protocol::wl_output::WlOutput;
pub use wgpu::Color as WgpuColor;
pub use wgpu::CompositeAlphaMode;
pub use wgpu::PresentMode;
