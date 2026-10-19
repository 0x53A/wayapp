//! Egui 0.36 consumes wheel deltas only on movement events. End clears state.
fn delivered_scroll(phase: egui::TouchPhase) -> f32 {
    let context = egui::Context::default();
    let input = egui::RawInput {
        events: vec![egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -3.0),
            modifiers: Default::default(),
            phase,
        }],
        ..Default::default()
    };
    let mut scroll = 0.0;
    let output = context.run_ui(input, |ui| {
        scroll = ui.input(|input| input.smooth_scroll_delta.y);
    });
    output.drop_without_applying_deltas();
    scroll
}

#[test]
fn movement_phase_delivers_wheel_delta() {
    assert!(delivered_scroll(egui::TouchPhase::Move) < 0.0);
}

#[test]
fn end_phase_does_not_deliver_wheel_delta() {
    assert_eq!(delivered_scroll(egui::TouchPhase::End), 0.0);
}
