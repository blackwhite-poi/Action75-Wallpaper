use super::key_label;
use crate::calibration_geometry::CalibrationGeometry;
use action75_manager::{Calibration, Point};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Painter, Pos2, Rect, RichText, Sense, Shape, Stroke,
    StrokeKind, emath::RectTransform, pos2, vec2,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::OnceLock;

const PANEL_WIDTH: i32 = 1920;
const PANEL_HEIGHT: i32 = 720;
const DISPLAY: Rect = Rect::from_min_max(Pos2::ZERO, pos2(PANEL_WIDTH as f32, PANEL_HEIGHT as f32));

// The chassis and mask are estimated from Action75 product photos. Coordinates
// are relative to the native display; the black glass extends past its pixels.
const CHASSIS: Rect = Rect::from_min_max(pos2(-104., -250.), pos2(2012., 795.));
const BLACK_MASK: Rect = Rect::from_min_max(pos2(-61., -29.), pos2(1965., 748.));
const BEZEL: Color32 = Color32::from_rgb(19, 22, 24);

#[derive(Deserialize)]
struct KeycapLayout {
    keys: Vec<Keycap>,
}

#[derive(Deserialize)]
struct Keycap {
    code: String,
    #[serde(rename = "boundsPx")]
    bounds: KeycapBounds,
}

#[derive(Deserialize)]
struct KeycapBounds {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

fn action75_keycaps() -> &'static [Keycap] {
    static LAYOUT: OnceLock<KeycapLayout> = OnceLock::new();
    // Embed the existing hardware layout so it remains available when wallpaper
    // packs are moved or removed. Fn has a keycap and no calibration point.
    &LAYOUT
        .get_or_init(|| {
            serde_json::from_str(include_str!(
                "../../../../wallpapers-native/keycap-glow/layout.json"
            ))
            .expect("valid bundled Action75 keycap layout")
        })
        .keys
}

pub(super) fn fit_reference() -> BTreeMap<String, Point> {
    action75_keycaps()
        .iter()
        .filter(|key| key.code != "Fn")
        .map(|key| {
            let bounds = &key.bounds;
            let center = Point {
                x: (f64::from(bounds.left) + f64::from(bounds.right))
                    / (2. * f64::from(PANEL_WIDTH)),
                y: (f64::from(bounds.top) + f64::from(bounds.bottom))
                    / (2. * f64::from(PANEL_HEIGHT)),
            };
            (key.code.clone(), center)
        })
        .collect()
}

#[derive(Clone, Copy)]
struct DrawnKey<'a> {
    code: &'a str,
    rect: Rect,
}

struct Palette {
    chassis: Color32,
    chassis_edge: Color32,
    display: Color32,
    boundary: Color32,
    keycap: Color32,
    key_edge: Color32,
    selected_keycap: Color32,
    point: Color32,
    text: Color32,
}

impl Palette {
    fn new(visuals: &egui::Visuals) -> Self {
        let (boundary, point) = if visuals.dark_mode {
            (
                Color32::from_rgb(123, 207, 199),
                Color32::from_rgb(240, 191, 127),
            )
        } else {
            (
                Color32::from_rgb(27, 109, 103),
                Color32::from_rgb(154, 84, 33),
            )
        };
        Self {
            chassis: visuals.window_fill.lerp_to_gamma(visuals.panel_fill, 0.25),
            chassis_edge: visuals.widgets.noninteractive.bg_stroke.color,
            display: visuals
                .panel_fill
                .lerp_to_gamma(Color32::from_rgb(72, 128, 121), 0.10),
            boundary,
            keycap: visuals.window_fill.gamma_multiply(0.86),
            key_edge: visuals.widgets.noninteractive.bg_stroke.color,
            selected_keycap: visuals.selection.bg_fill,
            point,
            text: visuals.text_color(),
        }
    }
}

pub(super) fn show(
    ui: &mut egui::Ui,
    bounds: Option<[i32; 4]>,
    model: Option<&Calibration>,
    selected: Option<&str>,
    caption: &str,
) {
    let physical = bounds.unwrap_or([0, 0, PANEL_WIDTH, PANEL_HEIGHT]);
    let [_, _, width, height] = physical;
    let action75 =
        i64::from(width) * i64::from(PANEL_HEIGHT) == i64::from(height) * i64::from(PANEL_WIDTH);
    let resolution = format!("{width} × {height}");
    let resolution = if bounds.is_none() {
        format!("原生 {resolution}")
    } else {
        resolution
    };
    let mut details = if action75 && ui.available_width() >= 410. {
        format!("12.3″  ·  {resolution}")
    } else {
        resolution
    };
    if let Some(model) = model {
        details.push_str(&format!("  ·  {} 个校准点", model.points.len()));
    }
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(if action75 {
                "ViPi Action75"
            } else {
                "键盘屏预览"
            })
            .size(17.),
        );
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(details).size(12.).weak());
        });
    });

    let display = if action75 {
        DISPLAY
    } else {
        Rect::from_min_size(Pos2::ZERO, vec2(width as f32, height as f32))
    };
    let drawing = if action75 { CHASSIS } else { display };
    let available = ui.available_width().max(1.);
    let scale = (available / drawing.width()).min(456. / drawing.height());
    let (area, response) =
        ui.allocate_exact_size(vec2(available, drawing.height() * scale), Sense::hover());
    let projection = RectTransform::from_to(
        drawing,
        Rect::from_center_size(area.center(), drawing.size() * scale),
    );
    let display = projection.transform_rect(display);
    let colors = Palette::new(ui.visuals());
    let painter = ui.painter().with_clip_rect(area.expand(1.));

    if action75 {
        draw_chassis(&painter, projection, &colors);
        painter.rect_filled(projection.transform_rect(BLACK_MASK), 2, BEZEL);
    }
    painter.rect_filled(display, 0, colors.display);

    let geometry = CalibrationGeometry::new(
        physical,
        [
            f64::from(display.left()),
            f64::from(display.top()),
            f64::from(display.width()),
            f64::from(display.height()),
        ],
    )
    .expect("positive preview dimensions");
    let center_of = |point| point_position(geometry, point);
    let keys: Vec<DrawnKey<'_>> = if action75 {
        action75_keycaps()
            .iter()
            .map(|key| DrawnKey {
                code: &key.code,
                rect: projection.transform_rect(Rect::from_min_max(
                    pos2(key.bounds.left, key.bounds.top),
                    pos2(key.bounds.right, key.bounds.bottom),
                )),
            })
            .collect()
    } else {
        // A different output can still show its own screen and calibrated points.
        let size = vec2(28., 23.).min(display.size());
        let available = display.shrink2(size * 0.5);
        model
            .into_iter()
            .flat_map(|model| &model.points)
            .map(|(code, point)| DrawnKey {
                code,
                rect: Rect::from_center_size(available.clamp(center_of(*point)), size),
            })
            .collect()
    };
    let hovered = response.hover_pos().and_then(|pointer| {
        model
            .into_iter()
            .flat_map(|model| &model.points)
            .filter_map(|(code, point)| {
                let distance = pointer.distance_sq(center_of(*point));
                (distance <= 25.).then_some((code.as_str(), distance))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(code, _)| code)
            .or_else(|| {
                keys.iter()
                    .find(|key| key.rect.contains(pointer))
                    .map(|key| key.code)
            })
    });
    let highlighted = |code: &str| selected == Some(code) || hovered == Some(code);

    for key in &keys {
        let active = highlighted(key.code);
        painter.rect(
            key.rect,
            3,
            if active {
                colors.selected_keycap
            } else {
                colors.keycap
            },
            Stroke::new(
                0.85_f32,
                if active {
                    colors.point
                } else {
                    colors.key_edge
                },
            ),
            StrokeKind::Inside,
        );
        if action75 {
            // The clear caps have a footprint and a smaller, raised top face.
            let face_inset = key.rect.height() * 0.135;
            painter.rect_stroke(
                key.rect.shrink(face_inset),
                2,
                Stroke::new(0.75_f32, colors.key_edge.gamma_multiply(0.68)),
                StrokeKind::Inside,
            );
        }
    }
    painter.rect_stroke(
        display,
        0,
        Stroke::new(1.2_f32, colors.boundary),
        StrokeKind::Middle,
    );

    let cap_font = if action75 {
        FontId::proportional((scale * 29.).clamp(9., 12.))
    } else {
        FontId::proportional(9.)
    };
    for key in &keys {
        painter.text(
            pos2(
                key.rect.center().x,
                key.rect.center().y + key.rect.height() * 0.23,
            ),
            Align2::CENTER_CENTER,
            label(key.code),
            cap_font.clone(),
            colors.text,
        );
    }

    if let Some(model) = model {
        for (code, point) in &model.points {
            let anchor = center_of(*point);
            if highlighted(code) {
                if let Some(key) = keys.iter().find(|key| key.code == code)
                    && key.rect.center().distance(anchor) > 5.
                {
                    painter.line_segment(
                        [key.rect.center(), anchor],
                        Stroke::new(0.9_f32, colors.point.gamma_multiply(0.62)),
                    );
                }
                painter.circle_stroke(anchor, 3.8, Stroke::new(1_f32, colors.point));
            }
            painter.circle_filled(anchor, 1.55, colors.point);
        }
    }
    if let Some(code) = hovered {
        response.on_hover_ui_at_pointer(|ui| {
            ui.label(key_label(code));
            if let Some(point) = model.and_then(|model| model.points.get(code)) {
                ui.label(
                    RichText::new(format!(
                        "{}, {} px",
                        pixel_coordinate(point.x, width),
                        pixel_coordinate(point.y, height),
                    ))
                    .weak(),
                );
            }
        });
    }
    draw_legend(ui, &colors, action75, model.is_some(), caption);
}

fn draw_chassis(painter: &Painter, projection: RectTransform, colors: &Palette) {
    let project = |(x, y)| projection.transform_pos(pos2(x, y));
    let outline = [
        (-104., -232.),
        (-87., -250.),
        (245., -250.),
        (284., -210.),
        (1624., -210.),
        (1663., -250.),
        (1995., -250.),
        (2012., -232.),
        (2012., 778.),
        (1995., 795.),
        (-87., 795.),
        (-104., 778.),
    ];
    let rear_left = (-104., -210.);
    let rear_right = (2012., -210.);
    let body = [
        rear_left,
        rear_right,
        outline[8],
        outline[9],
        outline[10],
        outline[11],
    ];
    let left_shoulder = [rear_left, outline[0], outline[1], outline[2], outline[3]];
    let right_shoulder = [outline[4], outline[5], outline[6], outline[7], rear_right];
    for surface in [
        body.as_slice(),
        left_shoulder.as_slice(),
        right_shoulder.as_slice(),
    ] {
        painter.add(Shape::convex_polygon(
            surface.iter().copied().map(project).collect(),
            colors.chassis,
            Stroke::NONE,
        ));
    }

    let edge = Stroke::new(1_f32, colors.chassis_edge);
    painter.add(Shape::closed_line(outline.map(project).to_vec(), edge));
    let deck = projection.transform_rect(Rect::from_min_max(pos2(-92., -162.), pos2(2000., -40.)));
    painter.rect(deck, 2, colors.chassis, edge, StrokeKind::Inside);
    let seam = Stroke::new(0.8_f32, colors.chassis_edge.gamma_multiply(0.68));
    painter.vline(deck.center().x, deck.y_range(), seam);
    painter.line_segment([project((-70., -188.)), project((1978., -188.))], seam);
    painter.line_segment([project((-78., 777.)), project((1986., 777.))], seam);
    painter.line_segment([project((-78., 755.)), project((1986., 755.))], seam);
}

fn draw_legend(
    ui: &mut egui::Ui,
    colors: &Palette,
    action75: bool,
    has_points: bool,
    caption: &str,
) {
    ui.add_space(2.);
    let width = ui.available_width();
    ui.horizontal_wrapped(|ui| {
        let swatch = |ui: &mut egui::Ui, label: &str, fill: Color32, stroke: Stroke, dot: bool| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.;
                let (rect, _) = ui.allocate_exact_size(vec2(12., 12.), Sense::hover());
                if dot {
                    ui.painter().circle_filled(rect.center(), 2.2, fill);
                } else {
                    ui.painter()
                        .rect(rect.shrink(1.), 1, fill, stroke, StrokeKind::Inside);
                }
                ui.label(RichText::new(label).size(12.).weak());
            });
        };
        swatch(
            ui,
            "有效显示区",
            Color32::TRANSPARENT,
            Stroke::new(1.2_f32, colors.boundary),
            false,
        );
        if action75 {
            swatch(
                ui,
                "屏幕黑边",
                BEZEL,
                Stroke::new(0.8_f32, colors.chassis_edge),
                false,
            );
        }
        if has_points {
            swatch(ui, "校准点", colors.point, Stroke::NONE, true);
        }
        if width >= 430. {
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(caption).size(12.).weak());
            });
        }
    });
    if width < 430. {
        ui.label(RichText::new(caption).size(12.).weak());
    }
}

fn point_position(geometry: CalibrationGeometry, point: Point) -> Pos2 {
    let [x, y] = geometry.normalized_to_canvas([point.x, point.y]);
    pos2(x as f32, y as f32)
}

fn pixel_coordinate(normalized: f64, length: i32) -> i32 {
    ((normalized * f64::from(length)).round() as i32).clamp(0, length - 1)
}

fn label(code: &str) -> &str {
    match code {
        "Escape" => "Esc",
        "PrintScreen" => "PrtSc",
        "Delete" => "Del",
        "Backspace" => "Bksp",
        "CapsLock" => "Caps",
        "PageUp" => "PgUp",
        "PageDown" => "PgDn",
        _ => key_label(code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Context, RawInput};
    use std::collections::BTreeMap;

    #[test]
    fn dragging_a_calibration_point_uses_the_lit_screen_and_keeps_the_keycaps_fixed() {
        let ctx = Context::default();
        let colors = Palette::new(&ctx.style().visuals);
        let frame = |point| {
            let model = Calibration::from_points(
                serde_json::Value::Null,
                BTreeMap::from([("KeyA".into(), point)]),
                vec!["KeyA".into()],
            );
            let output = ctx.run(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(700., 500.))),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        show(ui, Some([-1920, 0, 1920, 720]), Some(&model), None, "");
                    });
                },
            );
            let mut display = None;
            let mut marker = None;
            let mut key = None;
            for shape in output.shapes {
                match shape.shape {
                    Shape::Rect(rect) if rect.stroke.color == colors.boundary => {
                        if rect.rect.width() > 100. {
                            display = Some(rect.rect);
                        }
                    }
                    Shape::Circle(circle) if circle.fill == colors.point && circle.radius < 2. => {
                        marker = Some(circle.center);
                    }
                    Shape::Text(text) if text.galley.job.text == "F1" => key = Some(text.pos),
                    _ => {}
                }
            }
            (display.unwrap(), marker.unwrap(), key.unwrap())
        };

        let (display, first, first_key) = frame(Point { x: 0., y: 0. });
        let (updated_display, last, last_key) = frame(Point { x: 1., y: 1. });
        assert!((display.aspect_ratio() - 8. / 3.).abs() < 0.001);
        assert_eq!(display, updated_display);
        assert_eq!(first_key, last_key);
        assert!(first.distance(display.min) < 0.01);
        assert!(last.distance(display.max) < 0.01);
    }
}
