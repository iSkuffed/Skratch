//! The rows of the Tidy up lists: one card per song, with its cover, who it is, what is wrong
//! or what would change, and the buttons for it on the right.
use super::{Skratch, display, time};
use crate::style;
use eframe::egui::{self, Color32, CornerRadius, Rect, RichText, Sense, TextureId, pos2, vec2};
use skratch::{model::Track, suggest::Choice};

/// Height of a song's card.
pub(super) const ROW: f32 = 132.0;
/// Height of a song's card when it only has its cover and details (no list of changes).
pub(super) const SLIM: f32 = 92.0;
/// Height of a heading between groups of cards.
pub(super) const HEAD: f32 = 34.0;
/// Side of a cover picture.
const THUMB: f32 = 64.0;
/// Width the field name takes in a change line, so the values line up.
const FIELD_W: f32 = 48.0;

/// A card `height` tall across the page: `left` fills it up to the buttons, `right` is a column
/// `buttons_w` wide at its right edge.
pub(super) fn card(
    ui: &mut egui::Ui,
    height: f32,
    buttons_w: f32,
    left: impl FnOnce(&mut egui::Ui),
    right: impl FnOnce(&mut egui::Ui),
) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let body = rect.shrink2(vec2(0.0, 3.0));
    ui.painter().rect_filled(
        body,
        CornerRadius::same(10),
        style::RAISED.gamma_multiply(0.7),
    );
    let inner = body.shrink2(vec2(10.0, 8.0));
    let split = (inner.right() - buttons_w).max(inner.left() + 160.0);
    let text = Rect::from_min_max(inner.min, pos2(split - 12.0, inner.max.y));
    let buttons = Rect::from_min_max(pos2(split, inner.min.y), inner.max);
    // new_child, not scope_builder: a scope would move the page's cursor up to its own bottom
    let mut left_ui = ui.new_child(egui::UiBuilder::new().max_rect(text));
    left_ui.set_clip_rect(text.intersect(ui.clip_rect()));
    left_ui.spacing_mut().item_spacing = vec2(8.0, 2.0);
    left(&mut left_ui);
    let mut right_ui = ui.new_child(egui::UiBuilder::new().max_rect(buttons));
    right_ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
    right(&mut right_ui);
}

/// A square cover picture, or a plain box saying there is none.
pub(super) fn cover(ui: &mut egui::Ui, texture: Option<TextureId>, empty: &str) {
    match texture {
        Some(texture) => {
            ui.add(
                egui::Image::new((texture, vec2(THUMB, THUMB)))
                    .corner_radius(CornerRadius::same(6)),
            );
        }
        None => {
            let (rect, _) = ui.allocate_exact_size(vec2(THUMB, THUMB), Sense::hover());
            ui.painter()
                .rect_filled(rect, CornerRadius::same(6), style::PANEL);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                empty,
                egui::FontId::proportional(11.0),
                style::CREAM_DIM,
            );
        }
    }
}

/// A small arrow between two covers, drawn so no font has to have the glyph.
pub(super) fn arrow(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(16.0, THUMB), Sense::hover());
    let c = rect.center();
    let stroke = egui::Stroke::new(1.6_f32, style::CREAM_DIM);
    let painter = ui.painter();
    painter.line_segment([pos2(c.x - 5.0, c.y), pos2(c.x + 5.0, c.y)], stroke);
    painter.line_segment([pos2(c.x + 1.0, c.y - 4.5), pos2(c.x + 5.0, c.y)], stroke);
    painter.line_segment([pos2(c.x + 1.0, c.y + 4.5), pos2(c.x + 5.0, c.y)], stroke);
}

/// The first two lines of a card: the song's title, then who it is by and where it lives.
pub(super) fn identity(ui: &mut egui::Ui, track: &Track) {
    let title = display(&track.title, "Untitled");
    ui.add(
        egui::Label::new(RichText::new(title).size(15.0).strong().color(style::CREAM)).truncate(),
    );
    let name = track
        .path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let line = format!(
        "{}  ·  {}  ·  {}  ·  {}",
        display(&track.artist, "Unknown artist"),
        display(&track.album, "No album"),
        time(track.duration),
        name
    );
    ui.add(egui::Label::new(RichText::new(line).size(12.0).color(style::CREAM_DIM)).truncate())
        .on_hover_text(track.path.display().to_string());
}

/// "Year    none → 2021": what a field is now and what it would become.
pub(super) fn change_line(ui: &mut egui::Ui, field: &str, from: &str, to: &str, accent: Color32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.add_sized(
            [FIELD_W, 15.0],
            egui::Label::new(RichText::new(field).size(12.0).color(style::CREAM_DIM)),
        );
        let from = if from.is_empty() {
            RichText::new("none").size(12.0).italics()
        } else {
            RichText::new(from).size(12.0)
        };
        ui.label(from.color(style::CREAM_DIM));
        ui.label(RichText::new("→").size(12.0).color(style::CREAM_DIM));
        ui.add(egui::Label::new(RichText::new(to).size(12.0).color(accent)).truncate());
    });
}

/// Most lines of changes a card has room for.
const LINES: usize = 4;

/// Each change a choice makes, and its cover, one line apiece; what does not fit is counted.
pub(super) fn changes(ui: &mut egui::Ui, choice: &Choice, has_cover: bool, accent: Color32) {
    let total = choice.changes.len() + usize::from(choice.cover.is_some());
    let shown = if total > LINES { LINES - 1 } else { total };
    let mut left = shown;
    for change in choice.changes.iter().take(left) {
        change_line(ui, &change.field, &change.from, &change.to, accent);
    }
    left = left.saturating_sub(choice.changes.len());
    if left > 0
        && let Some(found) = &choice.cover
    {
        let from = if has_cover { "current" } else { "" };
        let to = format!("{} ({})", found.label, found.source);
        change_line(ui, "Cover", from, &to, accent);
    }
    if total > shown {
        let more = format!("+{} more", total - shown);
        ui.label(RichText::new(more).size(12.0).color(style::CREAM_DIM));
    }
}

impl Skratch {
    /// The cover a song has now, when it has one.
    pub(super) fn cover_of(&mut self, track: &Track) -> Option<TextureId> {
        let path = track.artwork.as_ref()?;
        self.art.texture(path)
    }
}
