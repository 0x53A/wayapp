//! Convert native surface-logical coordinates into egui points.
//! Kept independent of Wayland/WGPU so coordinate contracts can be reproduced
//! with the exact production conversion and egui alone.

pub(crate) fn logical_input_to_egui_points(input: &mut egui::RawInput, zoom: f32) {
    if let Some(rect) = &mut input.screen_rect {
        rect.min = rect.min / zoom;
        rect.max = rect.max / zoom;
    }
    for event in &mut input.events {
        match event {
            egui::Event::PointerMoved(pos) | egui::Event::PointerButton { pos, .. } => {
                *pos = *pos / zoom
            }
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta,
                ..
            } => *delta /= zoom,
            _ => {}
        }
    }
}
