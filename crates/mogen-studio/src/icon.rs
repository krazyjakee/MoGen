use eframe::egui;

const ICON_PNG: &[u8] = include_bytes!("../../../assets/icon.png");

pub fn load() -> egui::IconData {
    let img = image::load_from_memory(ICON_PNG)
        .expect("embedded icon.png decodes")
        .to_rgba8();
    let (width, height) = img.dimensions();
    egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    }
}
