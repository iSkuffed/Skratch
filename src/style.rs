//! Skratch's look: warm near-black, cream text, one accent that follows the playing cover.
//! Everything is painted with egui primitives, so there are no bundled assets.
use eframe::egui::{
    self, Align2, Color32, ColorImage, Context, CornerRadius, CursorIcon, FontId, Mesh, Painter,
    PointerButton, Pos2, Rect, Response, Sense, Shape, Stroke, TextureId, Ui, Vec2,
    ViewportCommand, ecolor::HsvaGamma as Hsva, viewport::ResizeDirection,
};
use std::{
    f32::consts::TAU,
    hash::{DefaultHasher, Hash, Hasher},
};

pub const BG: Color32 = Color32::from_rgb(14, 12, 11);
pub const TITLE: Color32 = Color32::from_rgb(19, 16, 15);
pub const SIDEBAR: Color32 = Color32::from_rgb(17, 14, 13);
pub const PANEL: Color32 = Color32::from_rgb(25, 21, 19);
pub const BAR: Color32 = Color32::from_rgb(21, 18, 17);
pub const RAISED: Color32 = Color32::from_rgb(42, 36, 32);
pub const EDGE: Color32 = Color32::from_rgb(60, 51, 46);
pub const CREAM: Color32 = Color32::from_rgb(241, 231, 214);
pub const CREAM_DIM: Color32 = Color32::from_rgb(150, 139, 126);
pub const ACCENT: Color32 = Color32::from_rgb(240, 150, 78);
pub const RADIUS: u8 = 12;
pub const HOME_URL: &str = "https://github.com/iSkuffed";

fn stroke(width: f32, color: Color32) -> Stroke {
    Stroke::new(width, color)
}

/// The Skratch mark, white on transparent so it can be tinted with the accent.
pub fn load_logo(ctx: &Context) -> egui::TextureHandle {
    let image = image::load_from_memory(include_bytes!("../assets/logo.png"))
        .expect("bundled logo is a valid PNG")
        .to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let image = ColorImage::from_rgba_unmultiplied(size, image.as_raw());
    ctx.load_texture("logo", image, egui::TextureOptions::LINEAR)
}

pub fn apply(ctx: &Context, accent: Color32) {
    let mut style = (*ctx.style()).clone();
    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.window_stroke = stroke(1.0, EDGE);
    v.window_corner_radius = CornerRadius::same(10);
    v.extreme_bg_color = BG;
    v.faint_bg_color = TITLE;
    v.hyperlink_color = accent;
    v.selection.bg_fill = accent.gamma_multiply(0.45);
    v.selection.stroke = stroke(1.0, CREAM);
    let w = &mut v.widgets;
    w.noninteractive.fg_stroke = stroke(1.0, Color32::from_rgb(222, 212, 196));
    w.noninteractive.bg_stroke = stroke(1.0, Color32::from_rgb(46, 39, 35));
    for state in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        state.corner_radius = CornerRadius::same(7);
    }
    w.noninteractive.corner_radius = CornerRadius::same(7);
    w.inactive.weak_bg_fill = RAISED;
    w.inactive.bg_fill = RAISED;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = stroke(1.0, CREAM);
    w.hovered.weak_bg_fill = Color32::from_rgb(60, 51, 45);
    w.hovered.bg_fill = Color32::from_rgb(60, 51, 45);
    w.hovered.bg_stroke = stroke(1.0, accent.gamma_multiply(0.6));
    w.hovered.fg_stroke = stroke(1.5, Color32::WHITE);
    w.active.weak_bg_fill = accent.gamma_multiply(0.7);
    w.active.bg_fill = accent;
    w.active.bg_stroke = Stroke::NONE;
    w.active.fg_stroke = stroke(1.5, Color32::WHITE);
    w.open.weak_bg_fill = w.hovered.weak_bg_fill;
    style.spacing.item_spacing = egui::vec2(8.0, 7.0);
    style
        .text_styles
        .insert(egui::TextStyle::Heading, FontId::proportional(26.0));
    ctx.set_style(style);
}

fn hash(seed: &str) -> u64 {
    let mut h = DefaultHasher::new();
    seed.hash(&mut h);
    h.finish()
}

fn hsv(h: f32, s: f32, v: f32) -> Color32 {
    Hsva { h, s, v, a: 1.0 }.into()
}

pub fn seed_color(seed: &str) -> Color32 {
    hsv((hash(seed) % 360) as f32 / 360.0, 0.62, 0.92)
}

/// A bright colour representing a cover: saturation-weighted average, pulled into a readable range.
pub fn dominant(image: &ColorImage) -> Color32 {
    let (mut sum, mut weight) = ([0.0_f32; 3], 0.0_f32);
    for px in image.pixels.iter().step_by(7) {
        let hsv = Hsva::from(*px);
        let w = hsv.s * hsv.v * hsv.v + 0.001;
        for (total, channel) in sum.iter_mut().zip([px.r(), px.g(), px.b()]) {
            *total += channel as f32 * w;
        }
        weight += w;
    }
    if weight <= 0.0 {
        return ACCENT;
    }
    let [r, g, b] = sum.map(|c| (c / weight) as u8);
    let mut hsv = Hsva::from(Color32::from_rgb(r, g, b));
    if hsv.s < 0.15 {
        return ACCENT; // a grey cover has no hue worth following
    }
    hsv.s = hsv.s.clamp(0.45, 0.85);
    hsv.v = hsv.v.clamp(0.78, 0.95);
    hsv.into()
}

/// A stand-in cover: rings, a coloured label and a few scratches, unique per `seed`.
pub fn sleeve(painter: &Painter, rect: Rect, seed: &str) {
    let h = hash(seed);
    let hue = (h % 360) as f32 / 360.0;
    let col = |s, v| hsv(hue, s, v);
    let (c, s) = (rect.center(), rect.width());
    painter.rect_filled(rect, 6.0, col(0.55, 0.17));
    let clip = painter.with_clip_rect(rect);
    for r in [0.46, 0.38, 0.30, 0.22] {
        clip.circle_stroke(c, s * r, stroke(1.0, col(0.6, 0.55).gamma_multiply(0.45)));
    }
    clip.circle_filled(c, s * 0.15, col(0.75, 0.9));
    clip.circle_filled(c, s * 0.025, col(0.55, 0.12));
    for i in 0..3 {
        let f = |n: u32| ((h >> (n * 8)) & 0xff) as f32 / 255.0;
        let (y0, y1) = (rect.top() + f(i * 2) * s, rect.top() + f(i * 2 + 1) * s);
        clip.line_segment(
            [Pos2::new(rect.left(), y0), Pos2::new(rect.right(), y1)],
            stroke(1.0, Color32::from_white_alpha(28)),
        );
    }
}

/// A vinyl record. `angle` spins the label and its art; the sheen stays put like a real reflection.
pub fn record(p: &Painter, c: Pos2, r: f32, art: Option<TextureId>, label: Color32, angle: f32) {
    p.circle_filled(c, r, Color32::from_rgb(9, 8, 7));
    p.circle_stroke(c, r, stroke(1.0, Color32::from_white_alpha(38)));
    for k in 0..14 {
        let ring = r * (0.44 + 0.55 * k as f32 / 13.0);
        let alpha = if k % 3 == 0 { 26 } else { 12 };
        p.circle_stroke(c, ring, stroke(0.7, Color32::from_white_alpha(alpha)));
    }
    let lr = r * 0.36;
    match art {
        Some(id) => {
            const N: usize = 64;
            let mut mesh = Mesh::with_texture(id);
            let vertex = |pos, uv| egui::epaint::Vertex {
                pos,
                uv,
                color: Color32::WHITE,
            };
            mesh.vertices.push(vertex(c, Pos2::new(0.5, 0.5)));
            for i in 0..=N {
                let t = i as f32 / N as f32 * TAU;
                let uv = Pos2::new(0.5 + 0.5 * t.cos(), 0.5 + 0.5 * t.sin());
                mesh.vertices
                    .push(vertex(c + lr * Vec2::angled(t + angle), uv));
            }
            for i in 0..N as u32 {
                mesh.add_triangle(0, 1 + i, 2 + i);
            }
            p.add(Shape::mesh(mesh));
        }
        None => {
            p.circle_filled(c, lr, label);
            p.circle_filled(c + lr * 0.68 * Vec2::angled(angle), lr * 0.09, BG);
        }
    }
    p.circle_stroke(c, lr, stroke(1.0, Color32::from_black_alpha(160)));
    p.circle_filled(c, r * 0.045, BG);
    for start in [-0.9_f32, 2.25] {
        let mut pts = vec![c];
        pts.extend((0..=8).map(|i| c + r * Vec2::angled(start + 0.55 * i as f32 / 8.0)));
        p.add(Shape::convex_polygon(
            pts,
            Color32::from_white_alpha(11),
            Stroke::NONE,
        ));
    }
}

/// Tonearm resting off the record at `lowered == 0.0` and sitting in the grooves at `1.0`.
pub fn tonearm(p: &Painter, pivot: Pos2, lowered: f32, accent: Color32) {
    let angle = (95.0 + 30.0 * lowered).to_radians();
    let head = pivot + 100.0 * Vec2::angled(angle);
    p.line_segment([pivot, head], stroke(3.0, Color32::from_rgb(120, 109, 98)));
    p.circle_filled(pivot, 8.0, RAISED);
    p.circle_stroke(pivot, 8.0, stroke(1.0, EDGE));
    p.circle_filled(head, 4.5, accent);
}

#[derive(Clone, Copy, PartialEq)]
pub enum Icon {
    Prev,
    Next,
    Play,
    Pause,
    Stop,
}

pub fn icon_button(
    ui: &mut Ui,
    icon: Icon,
    size: f32,
    fill: Option<Color32>,
    on: bool,
) -> Response {
    let sense = if on { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), sense);
    let hot = on && response.hovered();
    let p = ui.painter();
    let (c, u) = (rect.center(), size * 0.24);
    let fg = match (fill, on) {
        (Some(_), _) => BG,
        (None, true) => CREAM,
        (None, false) => EDGE,
    };
    match fill {
        Some(a) => {
            p.circle_filled(c, size / 2.0, if hot { a.gamma_multiply(1.12) } else { a });
        }
        None if hot => {
            p.circle_filled(c, size / 2.0, RAISED);
        }
        None => {}
    }
    let tri = |tip: f32, base: f32| {
        Shape::convex_polygon(
            vec![
                Pos2::new(c.x + tip, c.y),
                Pos2::new(c.x + base, c.y - u),
                Pos2::new(c.x + base, c.y + u),
            ],
            fg,
            Stroke::NONE,
        )
    };
    let bar = |x: f32, w: f32| {
        Shape::rect_filled(
            Rect::from_center_size(Pos2::new(c.x + x, c.y), Vec2::new(w, u * 2.0)),
            1.0,
            fg,
        )
    };
    match icon {
        Icon::Play => p.add(tri(u * 1.1, -u * 0.7)),
        Icon::Pause => {
            p.add(bar(-u * 0.5, u * 0.55));
            p.add(bar(u * 0.5, u * 0.55))
        }
        Icon::Stop => p.add(bar(0.0, u * 1.7)),
        Icon::Prev => {
            p.add(bar(-u * 0.9, u * 0.4));
            p.add(tri(-u * 0.7, u * 0.9))
        }
        Icon::Next => {
            p.add(bar(u * 0.9, u * 0.4));
            p.add(tri(u * 0.7, -u * 0.9))
        }
    };
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A seek bar drawn as record grooves. Bar heights are a stable pattern per `seed`, not real audio.
/// Returns the new position while the user clicks or drags.
pub fn groove_seek(
    ui: &mut Ui,
    position: f64,
    duration: f64,
    seed: u64,
    accent: Color32,
) -> Option<f64> {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().max(60.0), 30.0),
        Sense::click_and_drag(),
    );
    let fraction = (position / duration.max(1.0)).clamp(0.0, 1.0) as f32;
    let (a, b) = ((seed % 97) as f32, (seed / 97 % 89) as f32);
    let n = (rect.width() / 4.0) as usize;
    let p = ui.painter();
    for k in 0..n {
        let x = rect.left() + (k as f32 + 0.5) * rect.width() / n as f32;
        let t = k as f32;
        let wave = 0.5
            + 0.28 * (t * 0.31 + a).sin()
            + 0.16 * (t * 0.83 + b).sin()
            + 0.06 * (t * 2.1 + a * b).sin();
        let half = (rect.height() * 0.5 * wave).max(2.0);
        let color = if (k as f32 + 0.5) / (n as f32) <= fraction {
            accent
        } else if response.hovered() {
            Color32::from_white_alpha(70)
        } else {
            Color32::from_white_alpha(44)
        };
        p.line_segment(
            [
                Pos2::new(x, rect.center().y - half),
                Pos2::new(x, rect.center().y + half),
            ],
            stroke(2.0, color),
        );
    }
    let pos = response.interact_pointer_pos()?;
    (response.dragged() || response.clicked())
        .then(|| ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * duration)
}

fn wordmark(p: &Painter, left_center: Pos2, accent: Color32) {
    let galley = p.layout_no_wrap("SKRATCH".into(), FontId::proportional(21.0), accent);
    let rect = Align2::LEFT_CENTER.anchor_size(left_center, galley.size());
    for dx in [0.0, 0.6, 1.2] {
        p.galley(rect.min + Vec2::new(dx, 0.0), galley.clone(), accent);
    }
    // two scratches cut across the letters, the way a needle would
    for (from, to) in [(0.26, 0.36), (0.63, 0.7)] {
        let (a, b) = (
            Pos2::new(rect.left() + rect.width() * from, rect.bottom() + 3.0),
            Pos2::new(rect.left() + rect.width() * to, rect.top() - 3.0),
        );
        p.line_segment([a, b], stroke(2.0, TITLE));
        p.line_segment(
            [a + Vec2::new(2.0, 0.0), b + Vec2::new(2.0, 0.0)],
            stroke(0.8, CREAM.gamma_multiply(0.5)),
        );
    }
}

/// The window's own title bar: drag to move, double-click to maximise, and the three buttons.
pub fn title_bar(ui: &mut Ui, logo: &egui::TextureHandle, accent: Color32, now: &str) {
    let ctx = ui.ctx().clone();
    let rect = ui.max_rect();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    // leave the outer edge to `resize_edges`
    let drag_area = rect
        .shrink2(Vec2::new(6.0, 0.0))
        .with_min_y(rect.top() + 6.0);
    let drag = ui.interact(drag_area, ui.id().with("drag"), Sense::click_and_drag());
    if drag.drag_started_by(PointerButton::Primary) {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
    let p = ui.painter().clone();
    let mark = Rect::from_center_size(
        Pos2::new(rect.left() + 32.0, rect.center().y),
        logo.size_vec2() * (30.0 / logo.size_vec2().y),
    );
    egui::Image::new(logo).tint(accent).paint_at(ui, mark);
    let link = ui
        .interact(mark.expand(4.0), ui.id().with("logo"), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(HOME_URL);
    if link.clicked() {
        // Linux-only app; xdg-open hands the link to the default browser
        let _ = std::process::Command::new("xdg-open").arg(HOME_URL).spawn();
    }
    wordmark(&p, Pos2::new(mark.right() + 12.0, rect.center().y), accent);
    if !now.is_empty() {
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            now,
            FontId::proportional(13.0),
            CREAM_DIM,
        );
    }
    let mut x = rect.right() - 16.0;
    for kind in 0..3 {
        let c = Pos2::new(x - 12.0, rect.center().y);
        x -= 34.0;
        let response = ui
            .interact(
                Rect::from_center_size(c, Vec2::splat(28.0)),
                ui.id().with(("window", kind)),
                Sense::click(),
            )
            .on_hover_cursor(CursorIcon::PointingHand);
        if response.hovered() {
            let hot = if kind == 0 {
                Color32::from_rgb(190, 66, 56)
            } else {
                RAISED
            };
            p.circle_filled(c, 14.0, hot);
        }
        let stroke = stroke(1.6, CREAM);
        let d = 5.0;
        match kind {
            0 => {
                p.line_segment([c + Vec2::new(-d, -d), c + Vec2::new(d, d)], stroke);
                p.line_segment([c + Vec2::new(-d, d), c + Vec2::new(d, -d)], stroke);
                if response.clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
            1 => {
                p.rect_stroke(
                    Rect::from_center_size(c, Vec2::splat(d * 2.0)),
                    2.0,
                    stroke,
                    egui::StrokeKind::Middle,
                );
                if response.clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                }
            }
            _ => {
                p.line_segment(
                    [c + Vec2::new(-d, d - 1.0), c + Vec2::new(d, d - 1.0)],
                    stroke,
                );
                if response.clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                }
            }
        }
    }
}

/// Without a native frame the window needs its own invisible resize borders.
pub fn resize_edges(ctx: &Context) {
    const M: f32 = 6.0;
    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    let screen = ctx.content_rect();
    let (w, e) = (pos.x - screen.left() < M, screen.right() - pos.x < M);
    let (n, s) = (pos.y - screen.top() < M, screen.bottom() - pos.y < M);
    let (direction, cursor) = match (w, e, n, s) {
        (true, _, true, _) => (ResizeDirection::NorthWest, CursorIcon::ResizeNorthWest),
        (_, true, true, _) => (ResizeDirection::NorthEast, CursorIcon::ResizeNorthEast),
        (true, _, _, true) => (ResizeDirection::SouthWest, CursorIcon::ResizeSouthWest),
        (_, true, _, true) => (ResizeDirection::SouthEast, CursorIcon::ResizeSouthEast),
        (true, ..) => (ResizeDirection::West, CursorIcon::ResizeWest),
        (_, true, ..) => (ResizeDirection::East, CursorIcon::ResizeEast),
        (_, _, true, _) => (ResizeDirection::North, CursorIcon::ResizeNorth),
        (_, _, _, true) => (ResizeDirection::South, CursorIcon::ResizeSouth),
        _ => return,
    };
    ctx.set_cursor_icon(cursor);
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
    }
}
