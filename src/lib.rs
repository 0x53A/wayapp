mod application;
// mod egui;
mod egui;
mod kind;
mod single_color;

pub use application::*;
// pub use egui::*;
pub use egui::*;
pub use kind::*;
pub use single_color::*;

// Re-export wgpu types needed for SurfaceOptions configuration
pub use wgpu::{Color as WgpuColor, CompositeAlphaMode, PresentMode};
