//! Loudness of a track's low end over time, for the halo on the Now playing page.
//! Decoding the whole file up front (a second or two) keeps this independent of mpv.
use eframe::egui;
use std::{
    f32::consts::TAU,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    time::Instant,
};
use symphonia::core::{
    codecs::audio::AudioDecoderOptions,
    errors::Error,
    formats::{FormatOptions, TrackType, probe::Hint},
    io::MediaSourceStream,
    meta::MetadataOptions,
};

/// Samples of the envelope per second of audio.
pub const RATE: f32 = 30.0;

/// Cut-off of the low-pass filter that isolates kick and bass.
const BASS_HZ: f32 = 150.0;

/// The bass level (0 to 1) every `1 / RATE` seconds, or `None` if the file can't be decoded.
pub fn bass(path: &Path) -> Option<Vec<f32>> {
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let stream = MediaSourceStream::new(
        Box::new(std::fs::File::open(path).ok()?),
        Default::default(),
    );
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .ok()?;
    let track = format.default_track(TrackType::Audio)?;
    let track_id = track.id;
    let params = track.codec_params.as_ref()?.audio()?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .ok()?;

    let mut samples = Vec::new();
    let mut energy = Vec::new();
    let (mut low, mut sum, mut count) = (0.0f32, 0.0f32, 0usize);
    while let Ok(Some(packet)) = format.next_packet() {
        if packet.track_id != track_id {
            continue;
        }
        let buffer = match decoder.decode(&packet) {
            Ok(buffer) => buffer,
            Err(Error::IoError(_) | Error::DecodeError(_)) => continue,
            Err(_) => break,
        };
        let (rate, channels) = (
            buffer.spec().rate() as f32,
            buffer.spec().channels().count(),
        );
        let alpha = 1.0 - (-TAU * BASS_HZ / rate).exp();
        let window = (rate / RATE) as usize;
        buffer.copy_to_vec_interleaved(&mut samples);
        for frame in samples.chunks_exact(channels.max(1)) {
            let mono = frame.iter().sum::<f32>() / channels as f32;
            low += alpha * (mono - low);
            sum += low * low;
            count += 1;
            if count >= window {
                energy.push((sum / count as f32).sqrt());
                (sum, count) = (0.0, 0);
            }
        }
    }
    if energy.is_empty() {
        return None;
    }
    // scale so the loud parts reach 1 whatever the mastering level, then square for punch
    let mut sorted = energy.clone();
    sorted.sort_by(f32::total_cmp);
    let top = sorted[sorted.len() * 95 / 100].max(1e-4);
    Some(
        energy
            .into_iter()
            .map(|e| (e / top).min(1.0).powi(2))
            .collect(),
    )
}

/// Follows the bass of whatever is playing, smoothed so a beat snaps up and then falls away.
pub struct Reactive {
    tx: Sender<(PathBuf, Vec<f32>)>,
    rx: Receiver<(PathBuf, Vec<f32>)>,
    track: Option<PathBuf>,
    levels: Vec<f32>,
    /// The last position mpv reported and when it arrived, so time can run between reports.
    anchor: (f64, Instant),
    pulse: f32,
}

impl Default for Reactive {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            track: None,
            levels: Vec::new(),
            anchor: (0.0, Instant::now()),
            pulse: 0.0,
        }
    }
}

impl Reactive {
    /// The smoothed bass level (0 to 1) for this frame. Starts analysing a new track on demand;
    /// until that finishes, or for formats the decoder lacks, the level stays at zero.
    pub fn level(&mut self, ctx: &egui::Context, path: &Path, position: f64, playing: bool) -> f32 {
        if self.track.as_deref() != Some(path) {
            self.track = Some(path.to_path_buf());
            self.levels.clear();
            let (path, tx, ctx) = (path.to_path_buf(), self.tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                if let Some(levels) = bass(&path) {
                    let _ = tx.send((path, levels));
                    ctx.request_repaint();
                }
            });
        }
        while let Ok((done, levels)) = self.rx.try_recv() {
            if self.track.as_deref() == Some(done.as_path()) {
                self.levels = levels;
            }
        }
        if position != self.anchor.0 {
            self.anchor = (position, Instant::now());
        }
        let now = position + self.anchor.1.elapsed().as_secs_f64().min(0.5);
        let target = match self.levels.get((now * RATE as f64) as usize) {
            Some(level) if playing => *level,
            _ => 0.0,
        };
        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        let rate = if target > self.pulse { 40.0 } else { 5.0 };
        self.pulse += (target - self.pulse) * (1.0 - (-rate * dt).exp());
        if self.pulse > 0.01 || target > 0.0 {
            ctx.request_repaint();
        }
        self.pulse
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One second of a loud 60 Hz tone followed by one second of silence, as a 16-bit mono WAV.
    fn wav() -> Vec<u8> {
        let rate = 22050u32;
        let data: Vec<u8> = (0..rate * 2)
            .flat_map(|i| {
                let tone = (i as f32 / rate as f32 * 60.0 * TAU).sin() * 20000.0;
                (if i < rate { tone as i16 } else { 0 }).to_le_bytes()
            })
            .collect();
        let mut out = b"RIFF".to_vec();
        out.extend((36 + data.len() as u32).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes());
        out.extend([1, 0, 1, 0]);
        out.extend(rate.to_le_bytes());
        out.extend((rate * 2).to_le_bytes());
        out.extend([2, 0, 16, 0]);
        out.extend(b"data");
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(data);
        out
    }

    #[test]
    fn follows_the_bass() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        std::fs::write(&path, wav()).unwrap();
        let levels = bass(&path).expect("decodes");
        assert!((55..=65).contains(&levels.len()), "{}", levels.len());
        assert!(levels[10] > 0.8, "loud half: {}", levels[10]);
        assert!(levels[50] < 0.05, "silent half: {}", levels[50]);
    }
}
