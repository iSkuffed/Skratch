use crate::style;
use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureId};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender},
};

type Loaded = (PathBuf, Option<(ColorImage, Color32)>);

pub struct Artwork {
    requests: SyncSender<PathBuf>,
    results: Receiver<Loaded>,
    pending: HashSet<PathBuf>,
    textures: HashMap<PathBuf, Option<TextureHandle>>,
    accents: HashMap<PathBuf, Color32>,
    order: VecDeque<PathBuf>,
}

impl Artwork {
    pub fn new(ctx: egui::Context) -> Self {
        let (requests, rx) = mpsc::sync_channel::<PathBuf>(32);
        let (tx, results) = mpsc::sync_channel(32);
        std::thread::spawn(move || {
            while let Ok(path) = rx.recv() {
                let image = image::ImageReader::open(&path)
                    .ok()
                    .and_then(|reader| reader.decode().ok())
                    .map(|image| {
                        let image = image.thumbnail(256, 256).to_rgba8();
                        let image = ColorImage::from_rgba_unmultiplied(
                            [image.width() as usize, image.height() as usize],
                            image.as_raw(),
                        );
                        let accent = style::dominant(&image);
                        (image, accent)
                    });
                if tx.send((path, image)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Self {
            requests,
            results,
            pending: HashSet::new(),
            textures: HashMap::new(),
            accents: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub fn invalidate(&mut self) {
        self.textures.clear();
        self.accents.clear();
        self.order.clear();
    }

    pub fn drain(&mut self, ctx: &egui::Context) {
        while let Ok((path, image)) = self.results.try_recv() {
            self.pending.remove(&path);
            let texture = image.map(|(image, accent)| {
                self.accents.insert(path.clone(), accent);
                ctx.load_texture(path.to_string_lossy(), image, egui::TextureOptions::LINEAR)
            });
            self.textures.insert(path.clone(), texture);
            self.order.push_back(path);
            while self.order.len() > 48 {
                if let Some(old) = self.order.pop_front() {
                    self.textures.remove(&old);
                }
            }
        }
    }

    /// The decoded cover, or `None` while it loads or when the file is not an image.
    pub fn texture(&mut self, path: &Path) -> Option<TextureId> {
        if let Some(texture) = self.textures.get(path) {
            return texture.as_ref().map(TextureHandle::id);
        }
        if !self.pending.contains(path) && self.requests.try_send(path.to_path_buf()).is_ok() {
            self.pending.insert(path.to_path_buf());
        }
        None
    }

    pub fn accent(&self, path: &Path) -> Option<Color32> {
        self.accents.get(path).copied()
    }

    /// Paints the cover into `rect`, or a generated sleeve keyed by `seed` when there is none.
    pub fn paint(&mut self, ui: &egui::Ui, rect: egui::Rect, path: Option<&Path>, seed: &str) {
        match path.and_then(|path| self.texture(path)) {
            Some(id) => {
                egui::Image::new((id, rect.size()))
                    .corner_radius(6.0)
                    .paint_at(ui, rect);
            }
            None => style::sleeve(ui.painter(), rect, seed),
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        path: Option<&Path>,
        size: f32,
        seed: &str,
    ) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
        self.paint(ui, rect, path, seed);
        response
    }
}
