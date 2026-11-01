//! "Fetch metadata": looks the selected tracks up on MusicBrainz in the background, then shows
//! every proposed change in one list to tick and apply.
use super::{Skratch, display};
use crate::service::Command;
use eframe::egui::{self, RichText};
use skratch::{
    model::MetadataEdit,
    online::{self, TagMatch},
    tagparse,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

type Lookup = Result<Option<TagMatch>, String>;

struct Row {
    id: i64,
    /// `None` while the lookup is still waiting its turn.
    found: Option<Lookup>,
    checked: bool,
}

pub struct TagFetch {
    rows: Vec<Row>,
    rx: mpsc::Receiver<(i64, Lookup)>,
    cancel: Arc<AtomicBool>,
    replace: bool,
    running: bool,
    /// The user stopped the lookups, as opposed to them giving up on their own.
    stopped: bool,
}

impl Drop for TagFetch {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

use skratch::suggest::fill as proposed;

impl Skratch {
    pub(super) fn open_fetch(&mut self) {
        if self.fetch.is_some() {
            return;
        }
        let pattern = self.state.tag_pattern.clone();
        let jobs: Vec<_> = self
            .tracks
            .iter()
            .filter(|t| self.selected.contains(&t.id))
            .map(|t| {
                let (mut artist, mut title) = (t.artist.clone(), t.title.clone());
                // no artist tag: try the filename, the way "Fix tags from filenames" would
                if artist.trim().is_empty() {
                    let stem = t.path.file_stem().unwrap_or_default().to_string_lossy();
                    if let Some(parsed) = tagparse::parse(&pattern, &stem) {
                        artist = parsed.artist.unwrap_or_default();
                        title = parsed.title.unwrap_or(title);
                    }
                }
                (t.id, artist, title, t.duration)
            })
            .collect();
        let rows = jobs
            .iter()
            .map(|job| Row {
                id: job.0,
                found: None,
                checked: false,
            })
            .collect();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let (stop, wake) = (cancel.clone(), self.wake.clone());
        std::thread::spawn(move || {
            const ATTEMPTS: u32 = 4;
            let mut queue: VecDeque<_> = jobs.into_iter().map(|job| (job, 1)).collect();
            let (mut failures, mut asked) = (0, false);
            while let Some(((id, artist, title, seconds), attempt)) = queue.pop_front() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let result = if title.trim().is_empty() {
                    Ok(None)
                } else {
                    // MusicBrainz asks for at most one request a second
                    if asked {
                        std::thread::sleep(Duration::from_millis(1100));
                    }
                    asked = true;
                    online::lookup_tags(&artist, &title, seconds)
                };
                // a busy answer (503) puts the track at the back of the line for another go,
                // and after the last attempt counts as "no match"
                let result = match result {
                    Err(e) if online::is_busy(&e) => {
                        if attempt < ATTEMPTS {
                            queue.push_back(((id, artist, title, seconds), attempt + 1));
                            std::thread::sleep(Duration::from_secs(3));
                            continue;
                        }
                        Ok(None)
                    }
                    other => other,
                };
                let result = result.map_err(|e| format!("{e:#}"));
                failures = if result.is_err() { failures + 1 } else { 0 };
                if tx.send((id, result)).is_err() {
                    break;
                }
                wake();
                // offline or blocked: don't grind through the whole list
                if failures >= 3 {
                    break;
                }
            }
        });
        self.fetch = Some(TagFetch {
            rows,
            rx,
            cancel,
            replace: false,
            running: true,
            stopped: false,
        });
    }

    pub(super) fn fetch_window(&mut self, ctx: &egui::Context) {
        let Some(fetch) = self.fetch.as_mut() else {
            return;
        };
        loop {
            match fetch.rx.try_recv() {
                Ok((id, result)) => {
                    if let Some(row) = fetch.rows.iter_mut().find(|r| r.id == id) {
                        row.checked = matches!(&result, Ok(Some(m)) if m.strong);
                        row.found = Some(result);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    fetch.running = false;
                    break;
                }
            }
        }
        let done = fetch.rows.iter().filter(|r| r.found.is_some()).count();
        let errors = fetch
            .rows
            .iter()
            .filter(|r| matches!(r.found, Some(Err(_))))
            .count();
        let (replace, accent) = (fetch.replace, self.accent);
        let (tracks, by_id) = (&self.tracks, &self.by_id);
        let (mut open, mut apply, mut close, mut stop) = (true, false, false, false);
        egui::Window::new("Fetch metadata")
            .open(&mut open)
            .default_pos(ctx.content_rect().center() - egui::vec2(380.0, 250.0))
            .collapsible(false)
            .default_width(760.0)
            .show(ctx, |ui| {
                let total = fetch.rows.len();
                if fetch.running {
                    ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).text(format!("Looked up {done} of {total}")));
                } else if done < total {
                    let why = if fetch.stopped { "stopped" } else { "MusicBrainz did not respond (try again later)" };
                    ui.label(RichText::new(format!("Looked up {done} of {total}: {why}.")).color(accent));
                }
                ui.checkbox(&mut fetch.replace, "Replace existing values too (otherwise only empty fields are filled)");
                ui.separator();
                crate::style::scroll().max_height(380.0).show_rows(ui, 22.0, fetch.rows.len(), |ui, range| {
                    for row in &mut fetch.rows[range] {
                        let Some(track) = by_id.get(&row.id).map(|i| &tracks[*i]) else {
                            continue;
                        };
                        let edit = match &row.found {
                            Some(Ok(Some(m))) => Some(proposed(track, m, replace)),
                            _ => None,
                        };
                        let changes = edit.as_ref().is_some_and(|e| *e != MetadataEdit::from(track));
                        ui.horizontal(|ui| {
                            ui.add_enabled(changes, egui::Checkbox::without_text(&mut row.checked));
                            let now = format!("{} - {}", display(&track.artist, "Unknown artist"), track.title);
                            ui.allocate_ui_with_layout(egui::vec2(300.0, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.set_min_size(egui::vec2(300.0, 18.0));
                                ui.add(egui::Label::new(RichText::new(now).weak()).truncate());
                            });
                            match (&row.found, &edit) {
                                (None, _) => { ui.label(RichText::new("waiting...").weak()); }
                                (Some(Err(e)), _) => { ui.label(RichText::new(format!("failed: {e}")).weak()); }
                                (Some(Ok(None)), _) => { ui.label(RichText::new("no match").weak()); }
                                (_, Some(e)) if !changes => { let _ = e; ui.label(RichText::new("nothing to add").weak()); }
                                (_, Some(e)) => {
                                    let mut text = format!("{} - {}", display(&e.artist, "Unknown artist"), e.title);
                                    if !e.album.is_empty() { text.push_str(&format!("   [{}", e.album)); if let Some(y) = e.year { text.push_str(&format!(", {y}")); } text.push(']'); }
                                    if let Some(n) = e.track_number { text.push_str(&format!("   #{n}")); }
                                    ui.add(egui::Label::new(RichText::new(text).color(accent)).truncate());
                                }
                                _ => {}
                            }
                        });
                    }
                });
                ui.separator();
                let ticked = fetch.rows.iter().filter(|r| r.checked).count();
                let matched = fetch.rows.iter().filter(|r| matches!(r.found, Some(Ok(Some(_))))).count();
                ui.label(format!("{matched} matched, {ticked} ticked{}. Tags are written into the files themselves; filenames stay the same.", if errors > 0 { format!(", {errors} failed") } else { String::new() }));
                ui.horizontal(|ui| {
                    let verb = if fetch.running { "Stop and apply to" } else { "Apply to" };
                    if ui.add_enabled(ticked > 0, egui::Button::new(format!("{verb} {ticked} tracks"))).clicked() {
                        apply = true;
                    }
                    if fetch.running && ui.button("Stop").on_hover_text("Stop looking up, keep the list to review").clicked() {
                        stop = true;
                    }
                    if ui.button(if fetch.running { "Stop and cancel" } else { "Close" }).clicked() {
                        close = true;
                    }
                });
            });
        if stop {
            fetch.cancel.store(true, Ordering::Relaxed);
            fetch.running = false;
            fetch.stopped = true;
        }
        if apply {
            let edits: Vec<_> = fetch
                .rows
                .iter()
                .filter(|r| r.checked)
                .filter_map(|r| {
                    let track = &tracks[*by_id.get(&r.id)?];
                    let Some(Ok(Some(m))) = &r.found else {
                        return None;
                    };
                    Some((r.id, proposed(track, m, replace)))
                })
                .collect();
            self.fetch = None;
            self.command(Command::EditMany(edits));
            self.select_mode = false;
            self.selected.clear();
        } else if close || !open {
            self.fetch = None;
        }
    }
}
