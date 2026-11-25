use super::{BOARD_FRAME_FILL, BOARD_FRAME_SELECTED};
use eframe::egui::{
    self, Align, Align2, FontId, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2,
};

pub(super) fn show(ui: &mut egui::Ui, hmd: bool) {
    let width = ui.available_width().min(768.);
    ui.add_space(16.);
    ui.vertical_centered(|ui| {
        ui.allocate_ui_with_layout(vec2(width, 0.), egui::Layout::top_down(Align::Min), |ui| {
            ui.set_width(width);
            let visuals = ui.visuals();
            egui::Frame::new()
                .fill(visuals.faint_bg_color)
                .stroke(Stroke::new(
                    1_f32,
                    visuals
                        .widgets
                        .noninteractive
                        .bg_stroke
                        .color
                        .gamma_multiply(0.58),
                ))
                .corner_radius(6)
                .inner_margin(egui::Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    if ui.available_width() >= 515. {
                        ui.horizontal_top(|ui| {
                            diagram(ui);
                            ui.add_space(9.);
                            ui.vertical(|ui| instructions(ui, hmd));
                        });
                    } else {
                        instructions(ui, hmd);
                        ui.add_space(4.);
                        diagram(ui);
                    }
                });
        });
    });
}

fn instructions(ui: &mut egui::Ui, hmd: bool) {
    ui.spacing_mut().item_spacing.y = 4.;
    ui.label(RichText::new("校准教程").size(17.));
    let align = if hmd {
        "在桌面拖动校准框，让键盘副屏上的十字对齐对应键帽的中心。"
    } else {
        "看向键盘副屏，把校准框拖到对应实体键帽正下方，对齐中心。"
    };
    ui.label(RichText::new(align).size(13.));
    ui.label(
        RichText::new("选中后按方向键微调 1 像素，Shift + 方向键移动 10 像素。")
            .size(13.)
            .weak(),
    );
    ui.label(RichText::new("对齐后点击「保存并结束」。").size(13.).weak());
}

fn diagram(ui: &mut egui::Ui) {
    let (sample, _) = ui.allocate_exact_size(vec2(154., 76.), Sense::hover());
    let painter = ui.painter();
    let colors = ui.visuals();
    let edge = Stroke::new(1_f32, colors.widgets.noninteractive.bg_stroke.color);
    let face_edge = Stroke::new(0.75_f32, edge.color.gamma_multiply(0.62));

    for (x, offset, caption) in [
        (26., vec2(13., 10.), "拖动前"),
        (128., vec2(0., 0.), "中心对齐"),
    ] {
        let center = sample.min + vec2(x, 28.);
        let keycap = Rect::from_center_size(center, vec2(43., 43.));
        painter.rect(keycap, 4, colors.window_fill, edge, StrokeKind::Inside);
        painter.rect_stroke(keycap.shrink(5.), 3, face_edge, StrokeKind::Inside);
        painter.hline(center.x - 3.5..=center.x + 3.5, center.y, edge);
        painter.vline(center.x, center.y - 3.5..=center.y + 3.5, edge);

        let frame = Rect::from_center_size(center + offset, vec2(26., 17.));
        painter.rect(
            frame,
            3,
            BOARD_FRAME_FILL,
            Stroke::new(1_f32, BOARD_FRAME_SELECTED),
            StrokeKind::Inside,
        );
        painter.text(
            frame.center(),
            Align2::CENTER_CENTER,
            "A",
            FontId::proportional(11.),
            BOARD_FRAME_SELECTED,
        );
        painter.text(
            pos2(center.x, sample.bottom() - 12.),
            Align2::CENTER_CENTER,
            caption,
            FontId::proportional(11.),
            colors.weak_text_color(),
        );
    }

    let tail = sample.min + vec2(67., 28.);
    let tip = sample.min + vec2(91., 28.);
    painter.line_segment([tail, tip], edge);
    painter.add(egui::Shape::line(
        vec![tip - vec2(5., 4.), tip, tip - vec2(5., -4.)],
        edge,
    ));
}
