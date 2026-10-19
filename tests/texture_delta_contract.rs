//! Tests the real egui delivery contract used by Wayapp's renderer.
use egui::Color32;
use egui::ColorImage;
use egui::Context;
use egui::RawInput;
use egui::TextureOptions;

fn frame(context: &Context) -> egui::FullOutput {
    context.run_ui(RawInput::default(), |ui| {
        ui.label("atlas delivery probe");
    })
}

#[test]
fn dropped_font_atlas_is_not_resent_on_next_frame() {
    let context = Context::default();
    let mut first = frame(&context);
    let atlas_ids: Vec<_> = first.textures_delta.set.keys().copied().collect();
    first.textures_delta.clear(); // Wayapp's surface-acquisition failure branch.
    assert!(
        !atlas_ids.is_empty(),
        "first frame must upload the font atlas"
    );
    let mut next = frame(&context);
    let resent = atlas_ids
        .iter()
        .any(|id| next.textures_delta.set.contains_key(id));
    next.textures_delta.clear();
    assert!(!resent, "egui does not retry an acknowledged atlas upload");
}

#[test]
fn dropped_image_upload_and_free_are_not_resent() {
    let context = Context::default();
    let texture = context.load_texture(
        "delivery-probe",
        ColorImage::filled([2, 2], Color32::RED),
        TextureOptions::default(),
    );
    let id = texture.id();
    let mut first = frame(&context);
    let uploaded = first.textures_delta.set.contains_key(&id);
    first.textures_delta.clear();
    assert!(uploaded);
    let mut next = frame(&context);
    let retried_upload = next.textures_delta.set.contains_key(&id);
    next.textures_delta.clear();
    assert!(!retried_upload);

    drop(texture);
    let mut freeing = frame(&context);
    let freed = freeing.textures_delta.free.contains(&id);
    freeing.textures_delta.clear();
    assert!(freed);
    let mut next = frame(&context);
    let retried_free = next.textures_delta.free.contains(&id);
    next.textures_delta.clear();
    assert!(
        !retried_free,
        "a skipped free can leak renderer texture resources"
    );
}

#[test]
fn retaining_output_preserves_uploads_until_successful_frame() {
    let context = Context::default();
    let mut pending = frame(&context);
    let atlas_ids: Vec<_> = pending.textures_delta.set.keys().copied().collect();
    // Control case: retain failed-frame output and append newer output as
    // EguiSurfaceState already does while suspended.
    pending.append(frame(&context));
    let preserved = atlas_ids
        .iter()
        .all(|id| pending.textures_delta.set.contains_key(id));
    pending.textures_delta.clear();
    assert!(!atlas_ids.is_empty());
    assert!(preserved);
}
