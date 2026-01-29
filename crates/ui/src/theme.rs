use egui::Visuals;

pub fn setup_custom_style(ctx: &egui::Context, dark_mode: bool) {
    if dark_mode {
        let mut visuals = Visuals::dark();
        visuals.window_fill = egui::Color32::from_rgb(20, 20, 25);
        visuals.panel_fill = egui::Color32::from_rgb(20, 20, 25);
        ctx.set_visuals(visuals);
    } else {
        ctx.set_visuals(Visuals::light());
    }
}
