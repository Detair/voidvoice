use egui_plot::{Line, Plot, PlotPoints};

pub fn render_spectrum(ui: &mut egui::Ui, input_data: &[f32], output_data: &[f32]) {
    if input_data.is_empty() {
        ui.label("Waiting for audio...");
        return;
    }

    let red_line = Line::new(PlotPoints::from_ys_f32(input_data))
        .color(egui::Color32::from_rgba_unmultiplied(220, 53, 69, 180)) // Clearer red
        .fill(0.0); // Fill input (noise)

    let green_line = Line::new(PlotPoints::from_ys_f32(output_data))
        .color(egui::Color32::GREEN)
        .width(2.0); // Clean output

    Plot::new("spectrum")
        .height(100.0)
        .show_axes([false, false])
        .show_grid([false, false])
        .allow_drag(false)
        .allow_zoom(false)
        .show(ui, |plot_ui| {
            plot_ui.line(red_line);
            plot_ui.line(green_line);
        });
}
