use egui::{Color32, CornerRadius, Stroke, Visuals};

pub fn setup_custom_style(ctx: &egui::Context, dark_mode: bool) {
    if dark_mode {
        let mut visuals = Visuals::dark();

        // Premium Dark Palette (Deep Void Blue)
        let bg_color = Color32::from_rgb(13, 17, 23); // Extremely dark blue-grey
        let panel_color = Color32::from_rgb(22, 27, 34); // Slightly lighter
        let text_color = Color32::from_rgb(240, 246, 252);

        visuals.window_fill = bg_color;
        visuals.panel_fill = panel_color;
        visuals.override_text_color = Some(text_color);

        // Widgets
        visuals.widgets.noninteractive.bg_fill = panel_color;
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, text_color);

        visuals.widgets.inactive.bg_fill = Color32::from_rgb(33, 38, 45);
        visuals.widgets.inactive.corner_radius = CornerRadius::same(6);

        visuals.widgets.hovered.bg_fill = Color32::from_rgb(48, 54, 61);
        visuals.widgets.hovered.corner_radius = CornerRadius::same(6);

        visuals.widgets.active.bg_fill = Color32::from_rgb(88, 166, 255);
        visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::BLACK);
        visuals.widgets.active.corner_radius = CornerRadius::same(6);

        visuals.selection.bg_fill = Color32::from_rgb(56, 139, 253);

        ctx.set_visuals(visuals);
    } else {
        // Clean Light Mode
        let mut visuals = Visuals::light();
        visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
        visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
        visuals.widgets.active.corner_radius = CornerRadius::same(6);
        ctx.set_visuals(visuals);
    }
}
