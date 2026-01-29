use egui::{Pos2, Sense, Stroke};

/// Renders a horizontal volume meter with a threshold indicator.
pub fn render_volume_meter(ui: &mut egui::Ui, volume: f32, gate_threshold: f32) {
    // Calculate DB for current volume
    let volume_db = if volume > 0.0001 {
        20.0 * volume.log10()
    } else {
        -60.0
    };
    let bar_len = ((volume_db + 60.0) / 60.0).clamp(0.0, 1.0);

    // Calculate DB for threshold
    let threshold_db = if gate_threshold > 0.0001 {
        20.0 * gate_threshold.log10()
    } else {
        -60.0
    };
    let threshold_pos = ((threshold_db + 60.0) / 60.0).clamp(0.0, 1.0);

    let color = if volume > gate_threshold {
        egui::Color32::GREEN
    } else {
        egui::Color32::DARK_GRAY
    };

    // Custom painting
    let (rect, _response) = ui.allocate_at_least(egui::vec2(ui.available_width(), 20.0), Sense::hover());
    
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        
        // Background
        painter.rect_filled(rect, 2.0, egui::Color32::from_gray(40));

        // Fill (Volume Bar)
        if bar_len > 0.0 {
            let mut fill_rect = rect;
            fill_rect.set_width(rect.width() * bar_len);
            painter.rect_filled(fill_rect, 2.0, color);
        }

        // Threshold Marker
        let marker_x = rect.min.x + rect.width() * threshold_pos;
        painter.line_segment(
            [Pos2::new(marker_x, rect.min.y), Pos2::new(marker_x, rect.max.y)],
            Stroke::new(2.0, egui::Color32::WHITE),
        );
        
        // Text overlay
        let text = format!("{:.1} dB", volume_db);
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(12.0),
            egui::Color32::WHITE,
        );
    }

    ui.label(egui::RichText::new("White Line = Gate Threshold. Keep noise to the left, voice to the right.").size(10.0));
}
