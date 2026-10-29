//! Draws one lyric line: effects, dimmed ad-libs and rhyme colours, wrapped to the panel.
use eframe::egui::{
    self, Color32, FontId, Pos2, Response, Sense, TextFormat, Vec2, ecolor::Hsva,
    text::LayoutJob,
};
use skratch::{
    lyricfx::{self, ITALIC, LOUD, MOVING, RAINBOW, SHAKE, WAVE},
    rhyme::{self, Mark},
};
use std::{sync::Arc, time::Duration};

/// What every line shares for one frame.
pub struct Look<'a> {
    pub style: &'a crate::state::LyricStyle,
    pub time: f64,
}

/// One colour per rhyme group.
pub fn rhyme_color(group: usize) -> Color32 {
    Hsva::new(
        (group % rhyme::GROUPS) as f32 / rhyme::GROUPS as f32 + 0.02,
        0.55,
        0.98,
        1.0,
    )
    .into()
}

/// Rhyme marks for every line of a song, found verse by verse (blank lines divide verses).
pub fn rhyme_marks(lines: &[String]) -> Vec<Vec<Mark>> {
    let plain: Vec<String> = lines
        .iter()
        .map(|line| lyricfx::parse(line).plain())
        .collect();
    let mut marks = vec![Vec::new(); lines.len()];
    let mut start = 0;
    while start < plain.len() {
        if plain[start].trim().is_empty() {
            start += 1;
            continue;
        }
        let end = plain[start..]
            .iter()
            .position(|l| l.trim().is_empty())
            .map_or(plain.len(), |n| start + n);
        let verse: Vec<&str> = plain[start..end].iter().map(String::as_str).collect();
        for (offset, found) in rhyme::mark_verse(&verse).into_iter().enumerate() {
            marks[start + offset] = found;
        }
        start = end;
    }
    marks
}

struct Piece {
    galley: Arc<egui::Galley>,
    at: Vec2,
    /// Effects and the char's position in the line, for the moving ones.
    fx: u8,
    index: usize,
    size: f32,
}

#[derive(Clone, Copy, PartialEq)]
struct Look1 {
    size: f32,
    color: Color32,
    italic: bool,
}

/// Draws `text` (markup included) in `width`, `base` sized and coloured and faded by `fade`
/// (1 is solid). `marks` are the rhyme groups to colour, as byte ranges of the plain text.
pub fn draw_line(
    ui: &mut egui::Ui,
    text: &str,
    marks: &[Mark],
    (base, color, fade): (f32, Color32, f32),
    look: &Look,
) -> Response {
    let style = look.style;
    let mut parsed = lyricfx::parse(text);
    if !style.effects {
        parsed.fx.iter_mut().for_each(|fx| *fx = 0);
    }
    let count = parsed.chars.len();
    // rhyme group and ad-lib state of every char
    let mut group: Vec<Option<usize>> = vec![None; count];
    let mut byte = 0;
    let starts: Vec<usize> = parsed
        .chars
        .iter()
        .map(|c| {
            let at = byte;
            byte += c.len_utf8();
            at
        })
        .collect();
    for mark in marks {
        for (i, start) in starts.iter().enumerate() {
            if (mark.start..mark.end).contains(start) {
                group[i] = Some(mark.group);
            }
        }
    }
    let mut adlib = vec![false; count];
    let mut depth = 0;
    for (i, c) in parsed.chars.iter().enumerate() {
        if *c == '(' {
            depth += 1;
        }
        adlib[i] = style.ad_libs && depth > 0;
        if *c == ')' && depth > 0 {
            depth -= 1;
        }
    }
    let animate = style.animate;
    let char_look = |i: usize| -> Look1 {
        let fx = parsed.fx[i];
        let mut size = base;
        let mut color = color;
        if fx & LOUD != 0 {
            size *= 1.25;
        }
        if adlib[i] {
            size *= 0.82;
            color = color.gamma_multiply(0.7);
        }
        if let Some(g) = group[i] {
            color = rhyme_color(g);
        }
        if fx & RAINBOW != 0 {
            let phase = if animate { look.time as f32 * 0.5 } else { 0.0 };
            color = Hsva::new((i as f32 * 0.06 + phase).fract(), 0.6, 1.0, 1.0).into();
        }
        Look1 {
            size,
            color: color.gamma_multiply(fade),
            italic: fx & ITALIC != 0 || adlib[i],
        }
    };

    // words, each with the space after it
    let mut words: Vec<std::ops::Range<usize>> = Vec::new();
    let mut from = 0;
    for i in 0..count {
        let next_starts_word = i + 1 == count
            || (parsed.chars[i].is_whitespace() && !parsed.chars[i + 1].is_whitespace());
        if next_starts_word {
            words.push(from..i + 1);
            from = i + 1;
        }
    }

    let width = ui.available_width();
    let painter = ui.painter().clone();
    let layout = |text: String, look: Look1| {
        let mut job = LayoutJob::default();
        job.append(
            &text,
            0.0,
            TextFormat {
                font_id: FontId::proportional(look.size),
                color: look.color,
                italics: look.italic,
                ..TextFormat::default()
            },
        );
        painter.layout_job(job)
    };
    let mut pieces: Vec<Piece> = Vec::new();
    let (mut x, mut y, mut row_h) = (0.0_f32, 0.0_f32, 0.0_f32);
    let mut row_start = 0;
    let mut any_moving = false;
    for word in words {
        let looks: Vec<Look1> = word.clone().map(char_look).collect();
        let fxs: Vec<u8> = word.clone().map(|i| parsed.fx[i]).collect();
        let moving = fxs.iter().any(|fx| fx & (WAVE | SHAKE) != 0);
        any_moving |= fxs.iter().any(|fx| fx & MOVING != 0);
        let uniform = looks.windows(2).all(|w| w[0] == w[1]);
        let mut parts: Vec<(Arc<egui::Galley>, u8, usize)> = Vec::new();
        if uniform && !moving {
            let text: String = parsed.chars[word.clone()].iter().collect();
            parts.push((layout(text, looks[0]), fxs[0], word.start));
        } else {
            // one glyph at a time, so each can have its own colour and movement
            for (n, i) in word.clone().enumerate() {
                parts.push((layout(parsed.chars[i].to_string(), looks[n]), fxs[n], i));
            }
        }
        let word_w: f32 = parts.iter().map(|(g, ..)| g.size().x).sum();
        let visible_w = word_w
            - parts
                .iter()
                .rev()
                .take_while(|(g, ..)| g.text().trim().is_empty())
                .map(|(g, ..)| g.size().x)
                .sum::<f32>();
        if x > 0.0 && x + visible_w > width {
            for piece in &mut pieces[row_start..] {
                piece.at.y += (row_h - piece.galley.size().y) / 2.0;
            }
            row_start = pieces.len();
            x = 0.0;
            y += row_h;
            row_h = 0.0;
        }
        for (galley, fx, index) in parts {
            row_h = row_h.max(galley.size().y);
            let size = galley.size().y;
            pieces.push(Piece {
                at: Vec2::new(x, y),
                fx,
                index,
                size,
                galley: galley.clone(),
            });
            x += galley.size().x;
        }
    }
    for piece in &mut pieces[row_start..] {
        piece.at.y += (row_h - piece.galley.size().y) / 2.0;
    }
    let height = (y + row_h).max(base * 1.3);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let time = look.time as f32;
    for piece in &pieces {
        let mut at: Pos2 = rect.min + piece.at;
        if animate {
            if piece.fx & WAVE != 0 {
                at.y += (time * 3.0 + piece.index as f32 * 0.6).sin() * piece.size * 0.12;
            }
            if piece.fx & SHAKE != 0 {
                let tick = (look.time * 18.0) as u32;
                let n = (piece.index as u32).wrapping_mul(2654435761) ^ tick.wrapping_mul(40503);
                let jitter = |s: u32| (((n >> s) & 0xff) as f32 / 255.0 - 0.5) * piece.size * 0.12;
                at += Vec2::new(jitter(0), jitter(8));
            }
        }
        ui.painter().galley(at, piece.galley.clone(), Color32::WHITE);
    }
    if animate && any_moving && ui.is_rect_visible(rect) {
        ui.ctx().request_repaint_after(Duration::from_millis(33));
    }
    response
}
