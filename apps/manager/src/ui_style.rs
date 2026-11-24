use action75_manager::storage::InterfaceTheme;
use eframe::egui::{
    self, Color32, FontFamily, FontId, Id, Pos2, Rect, Stroke, Vec2, epaint::Mesh, pos2, vec2,
};
use std::{
    fs::{self, File, OpenOptions},
    os::windows::fs::OpenOptionsExt,
    sync::{Arc, OnceLock},
};

const DISPLAY: &str = "Action75 Display";
pub const FEEDBACK: f32 = 0.13;
pub const PAGE: f32 = 0.17;
pub const PANEL: f32 = 0.22;
pub const THEME: f32 = 0.20;

pub fn display_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(DISPLAY.into()))
}

fn chinese_font() -> Option<egui::FontData> {
    const PATH: &str = r"C:\Windows\Fonts\msyh.ttc";
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_DELETE: u32 = 4;
    static FONT: OnceLock<Option<(File, memmap2::Mmap)>> = OnceLock::new();

    if let Some((_, mapped)) = FONT
        .get_or_init(|| {
            let file = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
                .open(PATH)
                .ok()?;
            // Keep the file open without write sharing for the lifetime of the mapping.
            let mapped = unsafe { memmap2::MmapOptions::new().map(&file).ok()? };
            Some((file, mapped))
        })
        .as_ref()
    {
        return Some(egui::FontData::from_static(mapped));
    }
    fs::read(PATH).ok().map(egui::FontData::from_owned)
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if let Some(data) = chinese_font() {
        fonts.font_data.insert("chinese".into(), data.into());
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "chinese".into());
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .push("chinese".into());
    }
    fonts.font_data.insert(
        DISPLAY.into(),
        egui::FontData::from_static(include_bytes!("../../../assets/ui/action75-display.otf"))
            .into(),
    );
    let mut display = vec![DISPLAY.to_owned()];
    display.extend(fonts.families[&FontFamily::Proportional].iter().cloned());
    fonts
        .families
        .insert(FontFamily::Name(DISPLAY.into()), display);
    ctx.set_fonts(fonts);
}

pub fn motion_duration(ctx: &egui::Context, seconds: f32) -> f32 {
    if ctx.style().animation_time <= 0. {
        0.
    } else {
        seconds
    }
}

pub fn ease_out(t: f32) -> f32 {
    1. - (1. - t).powi(3)
}

pub fn animate(ctx: &egui::Context, id: Id, on: bool, seconds: f32) -> f32 {
    let duration = motion_duration(ctx, seconds);
    if duration == 0. {
        return u8::from(on) as f32;
    }
    ctx.animate_bool_with_time_and_easing(id, on, duration, ease_out)
}

/// Motion follows Windows' "Animation effects" setting.
fn system_animates() -> bool {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn SystemParametersInfoW(action: u32, param: u32, value: *mut i32, flags: u32) -> i32;
    }
    const SPI_GETCLIENTAREAANIMATION: u32 = 0x1042;
    let mut enabled = 1;
    // SPI_GETCLIENTAREAANIMATION writes a BOOL to the supplied pointer.
    unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, &mut enabled, 0) };
    enabled != 0
}

/// Bounded entrance feedback. No repaint is scheduled once the transition settles.
#[derive(Default)]
pub struct Reveal {
    started: Option<f64>,
}

impl Reveal {
    pub fn opacity(&mut self, ctx: &egui::Context, changed: bool) -> f32 {
        0.72 + 0.28 * self.progress(ctx, changed)
    }

    pub fn progress(&mut self, ctx: &egui::Context, changed: bool) -> f32 {
        let duration = motion_duration(ctx, PAGE);
        if duration == 0. {
            self.started = None;
            return 1.;
        }
        let now = ctx.input(|input| input.time);
        if changed {
            self.started = Some(now);
        }
        if let Some(started) = self.started {
            let elapsed = ((now - started) as f32 / duration).max(0.);
            if elapsed < 1. {
                ctx.request_repaint();
                return ease_out(elapsed);
            }
            self.started = None;
        }
        1.
    }
}

pub fn reveal_page_content(ui: &mut egui::Ui, progress: f32, show: impl FnOnce(&mut egui::Ui)) {
    let offset = vec2(0., 9. * (1. - progress));
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("page-content")
            .max_rect(ui.available_rect_before_wrap().translate(offset)),
    );
    content.multiply_opacity(egui::lerp(0.62..=1., progress));
    show(&mut content);
    // Report the resting bounds to the scroll area while the controls move.
    ui.advance_cursor_after_rect(content.min_rect().translate(-offset));
}

#[derive(Clone, Copy)]
struct Colors {
    bright: Color32,
    ground: Color32,
    cast: Color32,
    ink: Color32,
    muted: Color32,
    face: Color32,
    edge: Color32,
    selected: Color32,
    trace: Color32,
    error: Color32,
}

impl Colors {
    fn at(t: f32) -> Self {
        let gray = |light, dark| mix(Color32::from_gray(light), Color32::from_gray(dark), t);
        // Interpolating dark and light ink would become unreadable halfway
        // through the transition. Keep it legible over the moving surface.
        let ink = if t < 0.46 {
            Color32::from_gray(32)
        } else {
            Color32::from_gray(240)
        };
        let muted = if (0.18..0.8).contains(&t) {
            ink
        } else {
            gray(79, 192)
        };
        Self {
            bright: gray(245, 51),
            ground: gray(230, 28),
            cast: gray(201, 16),
            ink,
            muted,
            face: gray(249, 57),
            edge: gray(181, 104),
            selected: gray(211, 78),
            trace: gray(41, 229),
            error: mix(
                Color32::from_rgb(153, 49, 42),
                Color32::from_rgb(248, 164, 155),
                t,
            ),
        }
    }
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let channel = |a: u8, b: u8| egui::lerp(a as f32..=b as f32, t).round() as u8;
    Color32::from_rgb(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
    )
}

fn gray_alpha(color: Color32, alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), (255. * alpha) as u8)
}

#[derive(Default)]
pub struct Shell {
    animations: Option<bool>,
    tone: Option<u8>,
    surface: Option<(Rect, u8, bool, Arc<Mesh>)>,
}

impl Shell {
    pub fn clear_color(&self, theme: InterfaceTheme) -> [f32; 4] {
        let tone = self.tone.unwrap_or(if theme == InterfaceTheme::Graphite {
            255
        } else {
            0
        });
        Colors::at(tone as f32 / 255.)
            .ground
            .to_normalized_gamma_f32()
    }

    pub fn paint(&mut self, ctx: &egui::Context, theme: InterfaceTheme, acrylic: bool) {
        let enabled = *self.animations.get_or_insert_with(system_animates);
        if self.tone.is_none() {
            ctx.style_mut(|style| style.animation_time = if enabled { PAGE } else { 0. });
        }
        let t = animate(
            ctx,
            Id::new("interface-theme"),
            theme == InterfaceTheme::Graphite,
            THEME,
        );
        let tone = (255. * t).round() as u8;
        let colors = Colors::at(tone as f32 / 255.);
        if self.tone != Some(tone) {
            self.tone = Some(tone);
            apply_visuals(ctx, colors, tone >= 118);
        }
        let rect = ctx.content_rect();
        if rect.is_positive() {
            if !self.surface.as_ref().is_some_and(|(bounds, step, glass, _)| {
                *bounds == rect && *step == tone && *glass == acrylic
            }) {
                self.surface = Some((
                    rect,
                    tone,
                    acrylic,
                    Arc::new(case_surface(rect, colors, acrylic)),
                ));
            }
            if let Some((_, _, _, mesh)) = &self.surface {
                ctx.layer_painter(egui::LayerId::background())
                    .add(egui::Shape::Mesh(mesh.clone()));
            }
        }
    }
}

fn apply_visuals(ctx: &egui::Context, c: Colors, dark: bool) {
    let mut v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.panel_fill = c.ground;
    v.window_fill = c.face;
    v.extreme_bg_color = c.face;
    v.faint_bg_color = gray_alpha(c.face, 0.42);
    v.override_text_color = Some(c.ink);
    v.weak_text_color = Some(c.muted);
    v.error_fg_color = c.error;
    v.selection.bg_fill = c.selected;
    v.selection.stroke = Stroke::new(1_f32, c.ink);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1_f32, c.edge);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1_f32, c.muted);
    for (widget, fill) in [
        (&mut v.widgets.inactive, c.face),
        (&mut v.widgets.hovered, mix(c.face, c.selected, 0.43)),
        (&mut v.widgets.active, c.selected),
        (&mut v.widgets.open, c.selected),
    ] {
        widget.bg_fill = fill;
        widget.weak_bg_fill = fill;
        widget.bg_stroke = Stroke::new(1_f32, c.edge);
        widget.fg_stroke = Stroke::new(1_f32, c.ink);
        widget.corner_radius = 5.into();
    }
    v.handle_shape = egui::style::HandleShape::Circle;
    v.slider_trailing_fill = true;
    ctx.style_mut(|s| {
        s.visuals = v;
        s.spacing.item_spacing = vec2(9., 8.);
        s.spacing.button_padding = vec2(12., 7.);
        s.spacing.slider_rail_height = 2.;
        s.spacing.scroll.bar_width = 5.;
        s.spacing.scroll.dormant_handle_opacity = 0.13;
        s.spacing.scroll.active_handle_opacity = 0.34;
        s.spacing.scroll.interact_handle_opacity = 0.70;
        s.spacing.scroll.active_background_opacity = 0.;
        s.spacing.scroll.interact_background_opacity = 0.12;
        s.text_styles
            .insert(egui::TextStyle::Heading, display_font(31.));
        s.text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(14.));
        s.text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(14.));
        s.text_styles
            .insert(egui::TextStyle::Small, FontId::proportional(12.));
    });
}

fn case_surface(rect: Rect, colors: Colors, acrylic: bool) -> Mesh {
    let mut mesh = Mesh::default();
    const COLS: u32 = 16;
    const ROWS: u32 = 12;
    for row in 0..=ROWS {
        let y = row as f32 / ROWS as f32;
        for col in 0..=COLS {
            let x = col as f32 / COLS as f32;
            let base = mix(colors.bright, colors.ground, ((x + y) / 1.25).min(1.));
            let radius = vec2((x - 1.04) / 1.28, (y - 1.12) / 0.68).length();
            let shaded = mix(base, colors.cast, ((0.75 - radius) / 0.66).clamp(0., 1.));
            let fill = if acrylic {
                shaded.gamma_multiply(0.64)
            } else {
                shaded
            };
            mesh.colored_vertex(rect.min + rect.size() * vec2(x, y), fill);
            if col < COLS && row < ROWS {
                let i = row * (COLS + 1) + col;
                let j = i + COLS + 1;
                mesh.add_triangle(i, i + 1, j);
                mesh.add_triangle(i + 1, j + 1, j);
            }
        }
    }
    add_case_traces(&mut mesh, rect, colors.trace);
    mesh
}

/// Geometry of the approved case lip. Points are in the 1600 × 1000 design space.
fn add_case_traces(mesh: &mut Mesh, rect: Rect, color: Color32) {
    let point = |xy: [f32; 2]| pos2(xy[0], xy[1]);
    let scale = rect.size() / vec2(1600., 1000.);
    let map = |p: Pos2| rect.min + p.to_vec2() * scale;
    let mut line = Vec::with_capacity(64);
    let mut draw =
        |start: [f32; 2], curves: &[[[f32; 2]; 3]], end: [f32; 2], offset: f32, width: f32| {
            line.clear();
            line.push(point(start));
            for &[a, b, c] in curves {
                let from = *line.last().unwrap();
                let [a, b, c] = [point(a), point(b), point(c)];
                for index in 1..=16 {
                    let t = index as f32 / 16.;
                    let s = 1. - t;
                    line.push(pos2(
                        s.powi(3) * from.x
                            + 3. * s * s * t * a.x
                            + 3. * s * t * t * b.x
                            + t.powi(3) * c.x,
                        s.powi(3) * from.y
                            + 3. * s * s * t * a.y
                            + 3. * s * t * t * b.y
                            + t.powi(3) * c.y,
                    ));
                }
            }
            line.push(point(end));
            for p in &mut line {
                *p = map(*p + vec2(0., offset));
            }
            trace(mesh, &line, width * scale.min_elem(), color, rect);
        };
    for (offset, width) in [(0., 1.65), (10., 1.1), (19., 0.9)] {
        draw(
            [-36., 74.],
            &[
                [[302., 74.], [679., 74.], [1018., 74.]],
                [[1099., 74.], [1112., 29.], [1163., 29.]],
            ],
            [1644., 29.],
            offset,
            width,
        );
        draw(
            [-40., 891.],
            &[
                [[17., 891.], [65., 891.], [119., 891.]],
                [[203., 891.], [236., 929.], [306., 929.]],
                [[630., 929.], [1010., 929.], [1239., 929.]],
                [[1302., 929.], [1368., 875.], [1424., 820.]],
                [[1453., 794.], [1470., 794.], [1496., 794.]],
            ],
            [1640., 794.],
            offset * 1.2,
            width,
        );
    }
    for (offset, width) in [(0., 1.35), (10., 0.86)] {
        draw(
            [-22., 463.],
            &[
                [[24., 463.], [42., 463.], [61., 463.]],
                [[116., 463.], [128., 522.], [174., 522.]],
            ],
            [253., 522.],
            offset,
            width,
        );
        draw(
            [1626., 548.],
            &[
                [[1590., 548.], [1550., 548.], [1529., 548.]],
                [[1470., 548.], [1463., 620.], [1402., 620.]],
            ],
            [1255., 620.],
            offset,
            width,
        );
    }
    for (x, y, count, spacing) in [
        (765., 929., 6, 14.),
        (826., 74., 6, 14.),
        (44., 463., 3, 11.),
        (1551., 548., 3, 11.),
    ] {
        for index in 0..count {
            let x = x + index as f32 * spacing;
            trace(
                mesh,
                &[map(pos2(x, y - 9.)), map(pos2(x, y))],
                scale.min_elem(),
                color,
                rect,
            );
        }
    }
}

fn trace(mesh: &mut Mesh, points: &[Pos2], width: f32, color: Color32, rect: Rect) {
    let start = mesh.vertices.len() as u32;
    let outer = (width * 0.5 + 0.62).max(0.65);
    let inner = (width * 0.5 - 0.28).max(0.);
    for (i, &point) in points.iter().enumerate() {
        let before = points[i.saturating_sub(1)];
        let after = points[(i + 1).min(points.len() - 1)];
        let tangent: Vec2 = (after - before).normalized();
        let n = vec2(-tangent.y, tangent.x);
        let x = ((point.x - rect.min.x) / rect.width()).clamp(0., 1.);
        let edge_strength = ((x - 0.4).abs() / 0.6).min(1.);
        let ink = gray_alpha(color, 0.037 + 0.12 * edge_strength);
        for (distance, c) in [
            (-outer, Color32::TRANSPARENT),
            (-inner, ink),
            (inner, ink),
            (outer, Color32::TRANSPARENT),
        ] {
            mesh.colored_vertex(point + n * distance, c);
        }
        if i + 1 < points.len() {
            let base = start + 4 * i as u32;
            for stripe in 0..3 {
                let vertex = base + stripe;
                mesh.add_triangle(vertex, vertex + 1, vertex + 4);
                mesh.add_triangle(vertex + 1, vertex + 5, vertex + 4);
            }
        }
    }
}

pub fn nav_wash(ui: &egui::Ui, rect: Rect, opacity: f32) {
    if opacity <= 0. {
        return;
    }
    let mut mesh = Mesh::default();
    let radius = 13_f32;
    let ink = ui.visuals().text_color();
    let offsets = [
        0.,
        0.7,
        2.5,
        5.8,
        9.,
        radius,
        rect.width() * 0.45,
        rect.width() * 0.74,
        rect.width(),
    ];
    for (index, x) in offsets.into_iter().enumerate() {
        let rounding = radius - (radius * radius - (radius - x).max(0.).powi(2)).sqrt();
        let fade = (1. - x / rect.width()).powf(1.75);
        let color = gray_alpha(ink, 0.22 * fade * opacity);
        mesh.colored_vertex(pos2(rect.left() + x, rect.top() + rounding), color);
        mesh.colored_vertex(pos2(rect.left() + x, rect.bottom() - rounding), color);
        if index > 0 {
            let i = 2 * index as u32;
            mesh.add_triangle(i - 2, i - 1, i);
            mesh.add_triangle(i - 1, i + 1, i);
        }
    }
    ui.painter().add(egui::Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_reveal_settles_the_controls_without_changing_scroll_extent() {
        let ctx = egui::Context::default();
        let mut reveal = Reveal::default();
        let mut sample = |time, changed| {
            let mut bounds = Rect::NOTHING;
            let mut extent = Vec2::ZERO;
            let _ = ctx.run(
                egui::RawInput {
                    time: Some(time),
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(510., 360.))),
                    ..Default::default()
                },
                |ctx| {
                    let progress = reveal.progress(ctx, changed);
                    egui::CentralPanel::default().show(ctx, |ui| {
                        extent = egui::ScrollArea::vertical()
                            .show(ui, |ui| {
                                reveal_page_content(ui, progress, |ui| {
                                    bounds = ui.button("Apply").rect;
                                    ui.add_space(480.);
                                });
                            })
                            .content_size;
                    });
                },
            );
            (bounds, extent)
        };

        let (start, start_extent) = sample(1., true);
        let (middle, middle_extent) = sample(1.04, false);
        let (settled, settled_extent) = sample(1.2, false);
        assert!(start.top() > middle.top() && middle.top() > settled.top());
        assert_eq!(start.left(), settled.left());
        assert_eq!(start.size(), settled.size());
        assert!((start_extent - settled_extent).length() < 0.02);
        assert!((middle_extent - settled_extent).length() < 0.02);

        let (next_page, _) = sample(1.21, true);
        assert_eq!(next_page, start);

        ctx.style_mut(|style| style.animation_time = 0.);
        let (reduced_motion, _) = sample(1.22, true);
        assert_eq!(reduced_motion, settled);
    }
}
