mod application;
// mod egui;
mod egui;
mod frame_scheduler;
mod kind;
mod single_color;

pub use application::*;
// pub use egui::*;
pub use egui::*;
pub(crate) use frame_scheduler::*;
pub use kind::*;
pub use single_color::*;
// Re-export wayland types needed for multi-output support
pub use smithay_client_toolkit::output::OutputInfo;
pub use wayland_client::Proxy;
pub use wayland_client::protocol::wl_output::WlOutput;
// Re-export wgpu types needed for SurfaceOptions configuration
pub use wgpu::{Color as WgpuColor, CompositeAlphaMode, PresentMode};
