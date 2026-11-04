//! Skratch's look: warm near-black, cream text, one accent that follows the playing cover.
//! Everything is painted with egui primitives; the only bundled assets are the logo and two
//! typefaces (DM Sans and DM Serif Display, SIL Open Font Licence in `assets/fonts`).
use eframe::egui::{
    self, Align, Align2, Color32, ColorImage, Context, CornerRadius, CursorIcon, FontId, Mesh,
    Painter, PointerButton, Pos2, Rect, Response, Sense, Shape, Stroke, TextureId, Ui, Vec2,
    ViewportCommand, ecolor::HsvaGamma as Hsva, pos2, vec2, viewport::ResizeDirection,
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

/// The heading typeface: DM Serif Display, or the normal one when custom fonts are off.
pub fn display(size: f32) -> FontId {
    FontId::new(size, egui::FontFamily::Name("display".into()))
}

/// A font on this system that covers Japanese, Korean and Chinese, which the bundled faces
/// and egui's own lack; titles in those scripts would show as empty boxes without it.
fn cjk_fallback() -> Option<egui::FontData> {
    let found = (!cfg!(windows))
        .then(|| {
            std::process::Command::new("fc-match")
                .args(["-f", "%{file}|%{index}", "sans-serif:lang=ja"])
                .output()
                .ok()
        })
        .flatten()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| {
            let (file, index) = text.split_once('|')?;
            // the high 16 bits pick a named instance of a variable font; the face is the low ones
            let index: u32 = index.trim().parse().unwrap_or(0);
            Some((file.to_string(), index & 0xffff))
        });
    let (file, index) = found.or_else(|| {
        [
            "/usr/share/fonts/google-noto-sans-cjk-fonts/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "C:\\Windows\\Fonts\\YuGothR.ttc",
            "C:\\Windows\\Fonts\\msgothic.ttc",
            "C:\\Windows\\Fonts\\msyh.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
        ]
        .iter()
        .find(|path| std::path::Path::new(path).exists())
        .map(|path| (path.to_string(), 0))
    })?;
    let bytes = std::fs::read(file).ok()?;
    // a font egui cannot read would stop the app at startup, so check it first
    ab_glyph::FontRef::try_from_slice_and_index(&bytes, index).ok()?;
    let mut data = egui::FontData::from_owned(bytes);
    data.index = index;
    Some(data)
}

/// Installs the typefaces. `custom` puts DM Sans (body) and DM Serif Display (headings) first;
/// either way a system CJK font is added as a last resort.
pub fn fonts(ctx: &Context, custom: bool) {
    use egui::FontFamily::{Monospace, Name, Proportional};
    let mut fonts = egui::FontDefinitions::default();
    let mut display = fonts.families[&Proportional].clone();
    if custom {
        fonts.font_data.insert(
            "dm-sans".into(),
            egui::FontData::from_static(include_bytes!("../assets/fonts/DMSans.ttf")).into(),
        );
        fonts.font_data.insert(
            "dm-serif".into(),
            egui::FontData::from_static(include_bytes!(
                "../assets/fonts/DMSerifDisplay-Regular.ttf"
            ))
            .into(),
        );
        if let Some(body) = fonts.families.get_mut(&Proportional) {
            body.insert(0, "dm-sans".into());
        }
        display = fonts.families[&Proportional].clone();
        display.insert(0, "dm-serif".into());
    }
    fonts.families.insert(Name("display".into()), display);
    if let Some(data) = cjk_fallback() {
        fonts.font_data.insert("cjk".into(), data.into());
        for family in [Proportional, Monospace, Name("display".into())] {
            if let Some(list) = fonts.families.get_mut(&family) {
                list.push("cjk".into());
            }
        }
    }
    ctx.set_fonts(fonts);
}

/// `base` with a little of `accent` mixed in, so panels drift towards the cover's colour.
pub fn tint(base: Color32, accent: Color32, amount: f32) -> Color32 {
    base.lerp_to_gamma(accent, amount)
}

/// A soft wash of `accent` fading out downwards from the top of `ui`'s area.
pub fn wash(ui: &Ui, accent: Color32) {
    let area = ui.max_rect();
    let bottom = area.top() + 280.0;
    let mut mesh = egui::Mesh::default();
    for (y, alpha) in [(area.top(), 0.10), (bottom, 0.0)] {
        for x in [area.left(), area.right()] {
            mesh.colored_vertex(Pos2::new(x, y), accent.gamma_multiply(alpha));
        }
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 2, 3);
    ui.painter().add(egui::Shape::mesh(mesh));
}

/// A soft shadow under `rect`, drawn before whatever sits on it.
pub fn shadow(p: &Painter, rect: Rect, radius: f32) {
    for i in 1..=5 {
        let grow = i as f32 * 2.2;
        p.rect_filled(
            rect.expand(grow)
                .translate(Vec2::new(0.0, 2.0 + grow * 0.6)),
            radius + grow,
            Color32::from_black_alpha(14),
        );
    }
}

/// A round shadow under a record.
pub fn shadow_disc(p: &Painter, c: Pos2, r: f32) {
    for i in 1..=5 {
        let grow = i as f32 * 0.04 * r;
        p.circle_filled(
            c + Vec2::new(0.0, r * 0.05 + grow * 0.5),
            r + grow,
            Color32::from_black_alpha(16),
        );
    }
}

/// Film grain: a small noise tile that repeats. Drawn at about 3% it hides the banding that
/// dark gradients show on 8-bit displays.
pub fn grain_texture(ctx: &Context) -> egui::TextureHandle {
    const SIDE: usize = 128;
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let pixels: Vec<u8> = (0..SIDE * SIDE)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            if seed & 0x100 == 0 { 0 } else { 255 }
        })
        .collect();
    let image = ColorImage::from_gray([SIDE, SIDE], &pixels);
    let options = egui::TextureOptions {
        magnification: egui::TextureFilter::Nearest,
        minification: egui::TextureFilter::Nearest,
        wrap_mode: egui::TextureWrapMode::Repeat,
        mipmap_mode: None,
    };
    ctx.load_texture("grain", image, options)
}

pub fn grain(p: &Painter, rect: Rect, texture: &egui::TextureHandle) {
    let mut mesh = egui::Mesh::with_texture(texture.id());
    let uv = Rect::from_min_size(Pos2::ZERO, rect.size() / texture.size_vec2());
    mesh.add_rect_with_uv(rect, uv, Color32::from_white_alpha(7));
    p.add(egui::Shape::mesh(mesh));
}

pub fn apply(ctx: &Context, accent: Color32, panel: Color32) {
    let mut style = (*ctx.style()).clone();
    let v = &mut style.visuals;
    *v = egui::Visuals::dark();
    v.panel_fill = panel;
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
        .insert(egui::TextStyle::Heading, display(26.0));
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
    record_fading(p, c, r, art, None, label, angle);
}

/// Like `record`, with the previous label still showing at `fading.1` opacity (0 to 1) on top,
/// for a crossfade when the track changes.
pub fn record_fading(
    p: &Painter,
    c: Pos2,
    r: f32,
    art: Option<TextureId>,
    fading: Option<(TextureId, f32)>,
    label: Color32,
    angle: f32,
) {
    p.circle_filled(c, r, Color32::from_rgb(9, 8, 7));
    p.circle_stroke(c, r, stroke(1.0, Color32::from_white_alpha(38)));
    for k in 0..14 {
        let ring = r * (0.44 + 0.55 * k as f32 / 13.0);
        let alpha = if k % 3 == 0 { 26 } else { 12 };
        p.circle_stroke(c, ring, stroke(0.7, Color32::from_white_alpha(alpha)));
    }
    let lr = r * 0.36;
    let label_art = |id: TextureId, alpha: f32| {
        const N: usize = 64;
        let mut mesh = Mesh::with_texture(id);
        let vertex = |pos, uv| egui::epaint::Vertex {
            pos,
            uv,
            color: Color32::WHITE.gamma_multiply(alpha),
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
    };
    match art {
        Some(id) => label_art(id, 1.0),
        None => {
            p.circle_filled(c, lr, label);
            p.circle_filled(c + lr * 0.68 * Vec2::angled(angle), lr * 0.09, BG);
        }
    }
    if let Some((id, alpha)) = fading {
        label_art(id, alpha);
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
/// `lowered` may overshoot a little while the arm settles. `progress` (0 to 1) is how far into the
/// song it is: the needle follows the groove inward as it plays. `scale` is 1.0 for the player
/// bar's 52 px record.
pub fn tonearm(p: &Painter, pivot: Pos2, lowered: f32, progress: f32, accent: Color32, scale: f32) {
    let angle = (95.0 + 30.0 * lowered + 8.0 * progress * lowered.clamp(0.0, 1.0)).to_radians();
    let head = pivot + 100.0 * scale * Vec2::angled(angle);
    p.line_segment(
        [pivot, head],
        stroke(3.0 * scale, Color32::from_rgb(120, 109, 98)),
    );
    p.circle_filled(pivot, 8.0 * scale, RAISED);
    p.circle_stroke(pivot, 8.0 * scale, stroke(1.0, EDGE));
    p.circle_filled(head, 4.5 * scale, accent);
}

#[derive(Clone, Copy, PartialEq)]
pub enum Icon {
    Prev,
    Next,
    Play,
    Pause,
    /// Play (0.0) turning into pause (1.0).
    Morph(f32),
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
        Icon::Morph(t) => {
            // the triangle cut in two, each half sliding into one of the pause bars
            let corners = |play: [(f32, f32); 4], pause: [(f32, f32); 4]| {
                let at = |i: usize| {
                    let (a, b) = (play[i], pause[i]);
                    Pos2::new(
                        c.x + u * (a.0 + (b.0 - a.0) * t),
                        c.y + u * (a.1 + (b.1 - a.1) * t),
                    )
                };
                Shape::convex_polygon(vec![at(0), at(1), at(2), at(3)], fg, Stroke::NONE)
            };
            p.add(corners(
                [(-0.7, -1.0), (0.25, -0.47), (0.25, 0.47), (-0.7, 1.0)],
                [(-0.775, -1.0), (-0.225, -1.0), (-0.225, 1.0), (-0.775, 1.0)],
            ));
            p.add(corners(
                [(0.2, -0.5), (1.1, 0.0), (1.1, 0.0), (0.2, 0.5)],
                [(0.225, -1.0), (0.775, -1.0), (0.775, 1.0), (0.225, 1.0)],
            ))
        }
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

#[derive(Clone, Copy, PartialEq)]
pub enum Toggle {
    Shuffle,
    Repeat,
    /// Repeat with a "1" in the loop.
    RepeatOne,
}

/// A small outline icon that lights up in `active` colour when switched on.
pub fn toggle_button(ui: &mut Ui, toggle: Toggle, size: f32, active: Option<Color32>) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let p = ui.painter();
    let (c, u) = (rect.center(), size * 0.24);
    if response.hovered() {
        p.circle_filled(c, size / 2.0, RAISED);
    }
    let fg = active.unwrap_or(CREAM_DIM);
    let stroke = Stroke::new(1.8_f32, fg);
    let at = |x: f32, y: f32| Pos2::new(c.x + x * u, c.y + y * u);
    let line = |points: &[(f32, f32)]| {
        p.add(Shape::line(
            points.iter().map(|&(x, y)| at(x, y)).collect(),
            stroke,
        ));
    };
    // arrowhead whose tip is at (x, y), pointing right (dir = 1) or left (dir = -1)
    let head = |x: f32, y: f32, dir: f32| {
        p.add(Shape::convex_polygon(
            vec![
                at(x, y),
                at(x - dir * 0.9, y - 0.6),
                at(x - dir * 0.9, y + 0.6),
            ],
            fg,
            Stroke::NONE,
        ));
    };
    match toggle {
        Toggle::Shuffle => {
            line(&[(-1.5, -0.9), (-0.5, -0.9), (0.5, 0.9), (1.0, 0.9)]);
            line(&[(-1.5, 0.9), (-0.5, 0.9), (0.5, -0.9), (1.0, -0.9)]);
            head(1.7, -0.9, 1.0);
            head(1.7, 0.9, 1.0);
        }
        Toggle::Repeat | Toggle::RepeatOne => {
            line(&[(-1.4, 0.0), (-1.4, -0.9), (0.6, -0.9)]);
            line(&[(1.4, 0.0), (1.4, 0.9), (-0.6, 0.9)]);
            head(1.6, -0.9, 1.0);
            head(-1.6, 0.9, -1.0);
            if toggle == Toggle::RepeatOne {
                p.text(
                    c,
                    Align2::CENTER_CENTER,
                    "1",
                    FontId::proportional(size * 0.34),
                    fg,
                );
            }
        }
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// What the seek bar did this frame.
pub struct Seek {
    /// The new position while the user clicks or drags.
    pub to: Option<f64>,
    pub dragging: bool,
    /// Horizontal drag since the last frame, in points.
    pub drag_dx: f32,
}

/// A seek bar drawn as record grooves. Bar heights are the track's loudness (`peaks`, evenly
/// spaced over the whole track); until it is known the bars sit flat. With `beat` (the bass level,
/// 0 to 1) the playhead glows, the bars around it follow the bass, and hovering shows a scrub line
/// with the time under the pointer.
pub fn groove_seek(
    ui: &mut Ui,
    position: f64,
    duration: f64,
    peaks: &[f32],
    accent: Color32,
    beat: Option<f32>,
) -> Seek {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().max(60.0), 30.0),
        Sense::click_and_drag(),
    );
    let fraction = (position / duration.max(1.0)).clamp(0.0, 1.0) as f32;
    let n = (rect.width() / 4.0) as usize;
    let hover = response
        .hover_pos()
        .or_else(|| {
            response
                .dragged()
                .then(|| response.interact_pointer_pos())
                .flatten()
        })
        .map(|pos| ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0));
    let head = fraction * n as f32;
    let p = ui.painter();
    for k in 0..n {
        let x = rect.left() + (k as f32 + 0.5) * rect.width() / n as f32;
        let (from, to) = (
            k * peaks.len() / n,
            ((k + 1) * peaks.len() / n).max(k * peaks.len() / n + 1),
        );
        let wave = peaks[from.min(peaks.len())..to.min(peaks.len())]
            .iter()
            .copied()
            .fold(0.0, f32::max);
        let mut half = (rect.height() * 0.5 * wave).max(2.0);
        if let Some(beat) = beat {
            // the grooves under the needle swell with the bass
            let near = (1.0 - (k as f32 + 0.5 - head).abs() / 7.0).max(0.0);
            half = (half * (1.0 + 0.7 * beat * near * near)).min(rect.height() * 0.5);
        }
        let at = (k as f32 + 0.5) / (n as f32);
        let color = if at <= fraction {
            accent
        } else if beat.is_some() && hover.is_some_and(|h| at <= h) {
            Color32::from_white_alpha(96)
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
    if beat.is_some() && duration > 0.0 {
        let x = rect.left() + fraction * rect.width();
        for (width, alpha) in [(16.0, 0.05), (9.0, 0.09), (4.0, 0.2)] {
            p.rect_filled(
                Rect::from_center_size(
                    Pos2::new(x, rect.center().y),
                    Vec2::new(width, rect.height()),
                ),
                width / 2.0,
                accent.gamma_multiply(alpha),
            );
        }
        p.line_segment(
            [
                Pos2::new(x, rect.top() + 2.0),
                Pos2::new(x, rect.bottom() - 2.0),
            ],
            stroke(2.0, Color32::WHITE),
        );
        if let Some(h) = hover.filter(|_| duration > 0.0) {
            let hx = rect.left() + h * rect.width();
            p.line_segment(
                [Pos2::new(hx, rect.top()), Pos2::new(hx, rect.bottom())],
                stroke(1.0, Color32::from_white_alpha(120)),
            );
            let text = crate::app::time(f64::from(h) * duration);
            let label = p.layout_no_wrap(text, FontId::proportional(12.0), CREAM);
            let size = label.size() + Vec2::new(12.0, 6.0);
            let lx = hx.clamp(rect.left() + size.x / 2.0, rect.right() - size.x / 2.0);
            let tag = Rect::from_center_size(Pos2::new(lx, rect.top() - size.y / 2.0 - 2.0), size);
            p.rect_filled(tag, 5.0, RAISED);
            p.galley(tag.center() - label.size() / 2.0, label, CREAM);
        }
    }
    let to = response.interact_pointer_pos().and_then(|pos| {
        (response.dragged() || response.clicked())
            .then(|| ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * duration)
    });
    Seek {
        to,
        dragging: response.dragged(),
        drag_dx: if response.dragged() {
            response.drag_delta().x
        } else {
            0.0
        },
    }
}

/// How long the logo's scratch show lasts.
pub const SCRATCH_SECS: f32 = 1.6;

/// The record being scrubbed back and forth, easing out: -1 to 1.
fn wicka(t: f32) -> f32 {
    (t * std::f32::consts::TAU * 3.5).sin() * (1.0 - t).powi(2)
}

fn wordmark(p: &Painter, left_center: Pos2, accent: Color32, scratch: Option<f32>) {
    let t = scratch.map(|t| t.clamp(0.0, 1.0));
    // the word shakes with the record and flashes white as the needle goes through
    let shake = t.map_or(0.0, |t| wicka(t) * 3.5);
    let flash = t.map_or(0.0, |t| (1.0 - t).powi(2) * 0.6);
    let colour = accent.lerp_to_gamma(Color32::WHITE, flash);
    let galley = p.layout_no_wrap("SKRATCH".into(), display(22.0), colour);
    let rect = Align2::LEFT_CENTER.anchor_size(left_center + Vec2::new(shake, 0.0), galley.size());
    for dx in [0.0, 0.6, 1.2] {
        p.galley(rect.min + Vec2::new(dx, 0.0), galley.clone(), colour);
    }
    // a needle sweeps through the word, leaving fresh scratches that fade
    if let Some(t) = t {
        let sweep = (t / 0.4).min(1.0);
        let fade = 1.0 - ((t - 0.4) / 0.6).clamp(0.0, 1.0);
        for from in [0.08, 0.2, 0.46, 0.55, 0.78, 0.9] {
            if sweep < from {
                continue;
            }
            let a = Pos2::new(rect.left() + rect.width() * from, rect.bottom() + 4.0);
            let b = a + Vec2::new(rect.width() * 0.07, -rect.height() - 8.0);
            p.line_segment([a, b], stroke(1.6, TITLE.gamma_multiply(fade)));
            p.line_segment(
                [a + Vec2::new(1.5, 0.0), b + Vec2::new(1.5, 0.0)],
                stroke(0.8, Color32::WHITE.gamma_multiply(fade * 0.8)),
            );
        }
        if sweep < 1.0 {
            let x = rect.left() + rect.width() * sweep;
            p.line_segment(
                [
                    Pos2::new(x, rect.bottom() + 5.0),
                    Pos2::new(x + 7.0, rect.top() - 5.0),
                ],
                stroke(2.4, Color32::WHITE),
            );
        }
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
///
/// `scratch` is how far (0 to 1) the logo's scratch show has got, if one is running.
pub fn title_bar(
    ui: &mut Ui,
    logo: &egui::TextureHandle,
    accent: Color32,
    now: &str,
    scratch: Option<f32>,
) {
    let ctx = ui.ctx().clone();
    let rect = ui.max_rect();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    // leave the outer edge to `resize_edges`
    let drag_area = rect
        .shrink2(Vec2::new(6.0, 0.0))
        .with_min_y(rect.top() + 6.0);
    let drag = ui.interact(drag_area, ui.id().with("drag"), Sense::click_and_drag());
    if drag.drag_started_by(PointerButton::Primary) {
        hand_to_compositor(&ctx, ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
    let p = ui.painter().clone();
    let mark = Rect::from_center_size(
        Pos2::new(rect.left() + 32.0, rect.center().y),
        logo.size_vec2() * (30.0 / logo.size_vec2().y),
    );
    // the mark rocks like a record under a hand
    let angle = scratch.map_or(0.0, |t| wicka(t.clamp(0.0, 1.0)) * 0.5);
    egui::Image::new(logo)
        .tint(accent)
        .rotate(angle, Vec2::splat(0.5))
        .paint_at(ui, mark);
    let link = ui
        .interact(mark.expand(4.0), ui.id().with("logo"), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(HOME_URL);
    if link.clicked() {
        // the desktop's own way of opening a link in the default browser
        let mut open = if cfg!(windows) {
            let mut open = std::process::Command::new("rundll32");
            open.args(["url.dll,FileProtocolHandler", HOME_URL]);
            open
        } else {
            let mut open = std::process::Command::new("xdg-open");
            open.arg(HOME_URL);
            open
        };
        skratch::tools::hide_window(&mut open);
        let _ = open.spawn();
    }
    wordmark(
        &p,
        Pos2::new(mark.right() + 12.0, rect.center().y),
        accent,
        scratch,
    );
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

const RELEASE: &str = "release_after_compositor_grab";

/// Starts a window-manager drag or resize. The compositor then owns the pointer and swallows
/// the button release, which would leave egui believing the button is still held.
fn hand_to_compositor(ctx: &Context, command: ViewportCommand) {
    ctx.send_viewport_cmd(command);
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(RELEASE), true));
    ctx.request_repaint();
}

/// Call from `App::raw_input_hook`: delivers the release the compositor ate.
pub fn release_after_grab(ctx: &Context, raw: &mut egui::RawInput) {
    if ctx.data_mut(|d| d.remove_temp::<bool>(egui::Id::new(RELEASE))) == Some(true) {
        raw.events.push(egui::Event::PointerButton {
            pos: ctx.input(|i| i.pointer.latest_pos()).unwrap_or(Pos2::ZERO),
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
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
        hand_to_compositor(ctx, ViewportCommand::BeginResize(direction));
    }
}

/// How dark the settings drawer is at `t`: 0 at its left edge, 1 at the panel's right edge.
/// Dark all the way across, so the options read over anything.
pub fn drawer_alpha(t: f32) -> f32 {
    // a little lighter at the edge the tab hangs off, settled well before the options start
    let t = (t / 0.2).clamp(0.0, 1.0);
    0.80 + 0.15 * t * t * (3.0 - 2.0 * t)
}

/// The settings drawer's backing: a dark plate, a little lighter at its left edge.
pub fn drawer_backing(p: &Painter, rect: Rect) {
    const STEPS: u32 = 12;
    let mut mesh = Mesh::default();
    for k in 0..=STEPS {
        let x = egui::lerp(rect.x_range(), k as f32 / STEPS as f32);
        let alpha = (drawer_alpha(k as f32 / STEPS as f32) * 255.0) as u8;
        let colour = Color32::from_rgba_unmultiplied(8, 7, 6, alpha);
        mesh.colored_vertex(pos2(x, rect.top()), colour);
        mesh.colored_vertex(pos2(x, rect.bottom()), colour);
    }
    for k in 0..STEPS {
        let a = 2 * k;
        mesh.add_triangle(a, a + 1, a + 2);
        mesh.add_triangle(a + 1, a + 2, a + 3);
    }
    p.add(Shape::mesh(mesh));
}

/// A soft shadow along the drawer's left edge, which lifts it off the page.
pub fn drawer_shadow(p: &Painter, backing: Rect) {
    const STEPS: u32 = 8;
    const WIDTH: f32 = 28.0;
    let mut mesh = Mesh::default();
    for k in 0..=STEPS {
        let t = k as f32 / STEPS as f32;
        let x = backing.left() - WIDTH * (1.0 - t);
        let alpha = (0.42 * t * t * 255.0) as u8;
        let colour = Color32::from_rgba_unmultiplied(0, 0, 0, alpha);
        mesh.colored_vertex(pos2(x, backing.top()), colour);
        mesh.colored_vertex(pos2(x, backing.bottom()), colour);
    }
    for k in 0..STEPS {
        let a = 2 * k;
        mesh.add_triangle(a, a + 1, a + 2);
        mesh.add_triangle(a + 1, a + 2, a + 3);
    }
    p.add(Shape::mesh(mesh));
}

/// The settings drawer's pull tab: a small plate for the gear as dark as the drawer's left edge,
/// so the two read as one piece. `round` leaves the corners that join the drawer square.
pub fn drawer_tab(p: &Painter, rect: Rect, round: CornerRadius) {
    let alpha = (drawer_alpha(0.0) * 255.0) as u8;
    p.rect_filled(rect, round, Color32::from_rgba_unmultiplied(8, 7, 6, alpha));
}

/// A hairline round the drawer and its tab, so the two read as one object: along the tab's top,
/// left and bottom, then down the drawer's left edge above and below the tab.
pub fn drawer_outline(p: &Painter, backing: Rect, tab: Rect, round: CornerRadius) {
    let r = f32::from(round.nw);
    let mut path = vec![pos2(tab.right(), tab.top() + 0.5)];
    // round the two left corners, angles in screen space (y down)
    for (centre, from) in [
        (pos2(tab.left() + r, tab.top() + r), -90.0_f32),
        (pos2(tab.left() + r, tab.bottom() - r), 180.0),
    ] {
        for k in 0..=6 {
            let a = (from - 90.0 * k as f32 / 6.0).to_radians();
            path.push(centre + Vec2::new(a.cos(), a.sin()) * r);
        }
    }
    path.push(pos2(tab.right(), tab.bottom() - 0.5));
    let stroke = Stroke::new(1.0_f32, Color32::from_white_alpha(48));
    p.add(Shape::line(path, stroke));
    let x = backing.left() + 0.5;
    p.line_segment([pos2(x, backing.top()), pos2(x, tab.top())], stroke);
    p.line_segment([pos2(x, tab.bottom()), pos2(x, backing.bottom())], stroke);
}

/// How much of a list's dark backing is left `t` of the way down it (0 top, 1 bottom): all of
/// it over the first quarter, then easing out to nothing at the end of the list.
pub fn scrim_alpha(t: f32) -> f32 {
    let s = ((t - 0.25) / 0.75).clamp(0.0, 1.0);
    1.0 - s * s * (3.0 - 2.0 * s)
}

/// A black wash over `rect`, `alpha` dark at the top and clear at the bottom, with the top
/// corners rounded by `radius`. Rows of vertices run down it, each as wide as the shape is there.
pub fn scrim_mesh(rect: Rect, radius: f32, alpha: u8) -> Mesh {
    const ARC: u32 = 5;
    const BODY: u32 = 14;
    let radius = radius.min(rect.height() / 2.0).max(0.0);
    let arc = (0..=ARC).map(|k| radius * k as f32 / ARC as f32);
    let body = (1..=BODY).map(|k| radius + (rect.height() - radius) * k as f32 / BODY as f32);
    let mut mesh = Mesh::default();
    for down in arc.chain(body) {
        let inset = if down < radius {
            radius - (radius * radius - (radius - down).powi(2)).max(0.0).sqrt()
        } else {
            0.0
        };
        let a = (f32::from(alpha) * scrim_alpha(down / rect.height().max(1.0))).round() as u8;
        let colour = Color32::from_rgba_unmultiplied(0, 0, 0, a);
        let y = rect.top() + down;
        mesh.colored_vertex(pos2(rect.left() + inset, y), colour);
        mesh.colored_vertex(pos2(rect.right() - inset, y), colour);
    }
    for row in 0..mesh.vertices.len() as u32 / 2 - 1 {
        let a = 2 * row;
        mesh.add_triangle(a, a + 1, a + 2);
        mesh.add_triangle(a + 1, a + 2, a + 3);
    }
    mesh
}

/// The wash behind a list: see [`scrim_mesh`].
pub fn scrim_down(p: &Painter, rect: Rect, radius: f32, alpha: u8) {
    p.add(Shape::mesh(scrim_mesh(rect, radius, alpha)));
}

/// A cog: a ring with eight teeth.
pub fn gear(p: &Painter, c: Pos2, r: f32, colour: Color32) {
    p.circle_stroke(c, r * 0.58, Stroke::new(r * 0.28, colour));
    for k in 0..8 {
        let d = Vec2::angled(k as f32 * TAU / 8.0);
        p.line_segment([c + d * r * 0.75, c + d * r], Stroke::new(r * 0.36, colour));
    }
}

/// A rounded search box: a magnifier, a clear button once something is typed, and an optional
/// small chip at the end (label, hover text). Escape empties it. Returns the text box's
/// response, marked changed when the text was cleared, and whether the chip was clicked.
pub fn search_field(
    ui: &mut Ui,
    text: &mut String,
    id: egui::Id,
    hint: &str,
    chip: Option<(&str, &str)>,
    accent: Color32,
) -> (Response, bool) {
    const HEIGHT: f32 = 32.0;
    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), HEIGHT), Sense::hover());
    let bar_hit = ui.interact(bar, id.with("bar"), Sense::click());
    let focused = ui.memory(|m| m.has_focus(id));
    ui.painter().rect(
        bar,
        CornerRadius::same((HEIGHT / 2.0) as u8),
        Color32::from_black_alpha(110),
        Stroke::new(1.0_f32, if focused { accent } else { EDGE }),
        egui::StrokeKind::Inside,
    );
    let ink = if focused { CREAM } else { CREAM_DIM };
    let lens = pos2(bar.left() + 17.0, bar.center().y - 1.0);
    ui.painter()
        .circle_stroke(lens, 5.0, Stroke::new(1.5_f32, ink));
    ui.painter().line_segment(
        [lens + vec2(3.6, 3.6), lens + vec2(7.5, 7.5)],
        Stroke::new(1.8_f32, ink),
    );
    // the right end, from the edge inwards: the chip, then the clear button
    let mut right = bar.right() - 12.0;
    let mut chip_clicked = false;
    if let Some((label, hover)) = chip {
        let galley =
            ui.painter()
                .layout_no_wrap(label.into(), egui::FontId::proportional(12.0), CREAM);
        let width = galley.size().x + 16.0;
        let rect = Rect::from_min_size(
            pos2(right - width, bar.center().y - 10.0),
            vec2(width, 20.0),
        );
        let hit = ui
            .interact(rect, id.with("chip"), Sense::click())
            .on_hover_text(hover)
            .on_hover_cursor(CursorIcon::PointingHand);
        let fill = if hit.hovered() {
            Color32::from_white_alpha(34)
        } else {
            Color32::from_white_alpha(16)
        };
        ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
        ui.painter()
            .galley(rect.center() - galley.size() / 2.0, galley, CREAM);
        chip_clicked = hit.clicked();
        right = rect.left() - 8.0;
    }
    let mut cleared = false;
    if !text.is_empty() {
        let c = pos2(right - 8.0, bar.center().y);
        let hit = ui
            .interact(
                Rect::from_center_size(c, Vec2::splat(20.0)),
                id.with("clear"),
                Sense::click(),
            )
            .on_hover_cursor(CursorIcon::PointingHand);
        let ink = if hit.hovered() { CREAM } else { CREAM_DIM };
        for side in [1.0, -1.0] {
            ui.painter().line_segment(
                [c + vec2(-3.5, -3.5 * side), c + vec2(3.5, 3.5 * side)],
                Stroke::new(1.5_f32, ink),
            );
        }
        if hit.clicked() {
            text.clear();
            cleared = true;
        }
        right = c.x - 12.0;
    }
    let edit = Rect::from_min_max(
        pos2(bar.left() + 34.0, bar.top() + 2.0),
        // a very narrow bar still gets a text box, however little of it shows
        pos2(right.max(bar.left() + 60.0), bar.bottom() - 2.0),
    );
    let mut response = ui.put(
        edit,
        egui::TextEdit::singleline(text)
            .id(id)
            .frame(false)
            .margin(egui::Margin::ZERO)
            .hint_text(hint)
            .vertical_align(Align::Center)
            .desired_width(edit.width()),
    );
    if bar_hit.clicked() {
        response.request_focus();
    }
    // Escape takes the focus away before the box is drawn, so look for the focus just lost
    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        text.clear();
        cleared = true;
    }
    if cleared {
        response.mark_changed();
    }
    (response, chip_clicked)
}

/// The one way to make a vertical scroll area. Its width is fixed to the space it is given, so
/// the scrollbar stays against the panel's edge instead of following the longest row. Plain
/// `egui::ScrollArea::vertical()` is banned by clippy.toml; see the "Layout rules" in README.
#[allow(clippy::disallowed_methods)]
pub fn scroll() -> egui::ScrollArea {
    egui::ScrollArea::vertical().auto_shrink([false, true])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs one frame with a search box in it and returns whether it reported a change.
    fn search_frame(ctx: &egui::Context, text: &mut String, events: Vec<egui::Event>) -> bool {
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0))),
            events,
            ..Default::default()
        };
        let mut changed = false;
        let _ = ctx.run(raw, |ctx| {
            egui::Area::new("test-area".into()).show(ctx, |ui| {
                ui.set_width(300.0);
                let (reply, _) = search_field(ui, text, "test-search".into(), "hint", None, ACCENT);
                changed = reply.changed();
            });
        });
        changed
    }

    #[test]
    fn the_search_box_takes_text_and_escape_empties_it() {
        let ctx = egui::Context::default();
        let mut text = String::new();
        // the box has to be on screen before it can be given the keyboard
        search_frame(&ctx, &mut text, vec![]);
        ctx.memory_mut(|m| m.request_focus("test-search".into()));
        search_frame(&ctx, &mut text, vec![]);
        assert!(search_frame(
            &ctx,
            &mut text,
            vec![egui::Event::Text("abc".into())]
        ));
        assert_eq!(text, "abc");
        let escape = egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        assert!(search_frame(&ctx, &mut text, vec![escape]));
        assert_eq!(text, "");
        assert!(!ctx.memory(|m| m.has_focus("test-search".into())));
    }

    #[test]
    fn the_list_backing_is_darkest_at_the_top_and_clear_at_the_end() {
        assert_eq!(scrim_alpha(0.0), 1.0);
        assert_eq!(scrim_alpha(0.25), 1.0);
        assert_eq!(scrim_alpha(1.0), 0.0);
        let steps: Vec<f32> = (0..=20).map(|k| scrim_alpha(k as f32 / 20.0)).collect();
        assert!(steps.windows(2).all(|pair| pair[0] >= pair[1]));

        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(300.0, 400.0));
        let mesh = scrim_mesh(rect, 10.0, 160);
        // rows of two vertices, left then right; the first row is pulled in by the corner radius
        assert_eq!(mesh.vertices[0].pos, pos2(20.0, 20.0));
        assert_eq!(mesh.vertices[1].pos, pos2(300.0, 20.0));
        let alphas: Vec<u8> = mesh.vertices.iter().map(|v| v.color.a()).collect();
        assert_eq!(alphas[0], 160);
        assert!(alphas.windows(2).all(|pair| pair[0] >= pair[1]));
        let last = mesh.vertices.len() - 2;
        assert_eq!(alphas[last], 0);
        assert_eq!(mesh.vertices[last].pos, pos2(10.0, 420.0));
        // every triangle uses vertices that exist
        assert!(
            mesh.indices
                .iter()
                .all(|i| (*i as usize) < mesh.vertices.len())
        );
    }

    #[test]
    fn the_drawer_is_dark_throughout_and_settles_past_its_left_edge() {
        assert!((drawer_alpha(0.0) - 0.80).abs() < 1e-6);
        assert!((drawer_alpha(1.0) - 0.95).abs() < 1e-6);
        let steps: Vec<f32> = (0..=20).map(|k| drawer_alpha(k as f32 / 20.0)).collect();
        assert!(steps.windows(2).all(|pair| pair[0] <= pair[1]) && steps[0] < steps[2]);
        // out-of-range input stays on the curve's ends
        assert_eq!(drawer_alpha(-3.0), drawer_alpha(0.0));
        assert_eq!(drawer_alpha(5.0), drawer_alpha(1.0));
    }
}
