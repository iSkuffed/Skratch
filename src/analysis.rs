//! Loudness of a track over time: the low end for the halo on the Now playing page, the
//! overall level for the waveform in the player bar, and the spectrum for the visualizer.
//! Decoding the whole file up front (a second or two) keeps this independent of mpv.
use eframe::egui;
use std::{
    f32::consts::TAU,
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        mpsc::{self, Receiver, Sender},
    },
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

/// Bars in the visualizer's spectrum, log-spaced from `LOW_HZ` to `HIGH_HZ`.
pub const BANDS: usize = 48;
const LOW_HZ: f32 = 40.0;
const HIGH_HZ: f32 = 16_000.0;
/// Samples per Fourier transform (a power of two).
const FFT: usize = 2048;

/// What one decoding pass learns about a track, one value (0 to 1) every `1 / RATE` seconds.
pub struct Analysis {
    pub bass: Vec<f32>,
    /// Overall loudness, for the waveform.
    pub peaks: Vec<f32>,
    /// `BANDS` values (0 to 255) per step, low to high pitch.
    pub bands: Vec<u8>,
}

static TWIDDLES: OnceLock<Vec<(f32, f32)>> = OnceLock::new();

/// In-place radix-2 Fourier transform; the length must be a power of two.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    // the rotation of every stage, computed once: stage `len` starts at `len / 2 - 1`
    let twiddles = TWIDDLES.get_or_init(|| {
        let mut table = Vec::with_capacity(FFT);
        let mut len = 2;
        while len <= FFT {
            let step = -TAU / len as f32;
            table.extend((0..len / 2).map(|k| (step * k as f32).sin_cos()));
            len <<= 1;
        }
        table
    });
    let mut len = 2;
    while len <= n {
        let stage = &twiddles[len / 2 - 1..len - 1];
        for start in (0..n).step_by(len) {
            for (k, &(sin, cos)) in stage.iter().enumerate() {
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cos - im[b] * sin, re[b] * sin + im[b] * cos);
                (re[b], im[b]) = (re[a] - tr, im[a] - ti);
                (re[a], im[a]) = (re[a] + tr, im[a] + ti);
            }
        }
        len <<= 1;
    }
}

/// Turns the latest `FFT` samples into `BANDS` levels in dB, rising ~4.5 dB per octave to make
/// up for the way music falls off towards the treble.
fn spectrum(history: &[f32], rate: f32, window: &[f32], out: &mut [f32; BANDS]) {
    let mut re = vec![0.0f32; FFT];
    let mut im = vec![0.0f32; FFT];
    let take = history.len().min(FFT);
    for (i, s) in history[history.len() - take..].iter().enumerate() {
        re[i] = s * window[i];
    }
    fft(&mut re, &mut im);
    let hz = rate / FFT as f32;
    let ratio = (HIGH_HZ.min(rate / 2.0) / LOW_HZ).powf(1.0 / BANDS as f32);
    for (b, level) in out.iter_mut().enumerate() {
        let lo = LOW_HZ * ratio.powi(b as i32);
        let hi = lo * ratio;
        let first = ((lo / hz) as usize).max(1);
        let last = ((hi / hz).ceil() as usize).clamp(first + 1, FFT / 2);
        let power = (first..last)
            .map(|k| re[k] * re[k] + im[k] * im[k])
            .sum::<f32>()
            / (last - first) as f32;
        let tilt = 4.5 * (lo / 1000.0).log2();
        *level = 10.0 * (power / (FFT * FFT) as f32 + 1e-12).log10() + tilt;
    }
}

/// Scales so the loud parts reach 1 whatever the mastering level.
fn normalise(values: &[f32]) -> Vec<f32> {
    let mut sorted = values.to_vec();
    let at = sorted.len() * 95 / 100;
    let top = (*sorted.select_nth_unstable_by(at, f32::total_cmp).1).max(1e-4);
    values.iter().map(|v| (v / top).min(1.0)).collect()
}

/// Decodes the whole file, or `None` if it can't be decoded.
pub fn analyse(path: &Path) -> Option<Analysis> {
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
    let window: Vec<f32> = (0..FFT)
        .map(|i| 0.5 - 0.5 * (TAU * i as f32 / FFT as f32).cos())
        .collect();
    let (mut history, mut decibels) = (Vec::<f32>::new(), Vec::<f32>::new());
    let (mut energy, mut loudness) = (Vec::new(), Vec::new());
    let (mut low, mut sum, mut full, mut count) = (0.0f32, 0.0f32, 0.0f32, 0usize);
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
        let step = (rate / RATE) as usize;
        buffer.copy_to_vec_interleaved(&mut samples);
        for frame in samples.chunks_exact(channels.max(1)) {
            let mono = frame.iter().sum::<f32>() / channels as f32;
            low += alpha * (mono - low);
            sum += low * low;
            full += mono * mono;
            count += 1;
            history.push(mono);
            if count >= step {
                energy.push((sum / count as f32).sqrt());
                loudness.push((full / count as f32).sqrt());
                (sum, count, full) = (0.0, 0, 0.0);
                if history.len() > 2 * FFT {
                    history.drain(..history.len() - FFT);
                }
                let mut bands = [0.0; BANDS];
                spectrum(&history, rate, &window, &mut bands);
                decibels.extend(bands);
            }
        }
    }
    if energy.is_empty() {
        return None;
    }
    // the loud parts reach the top of the bars; 38 dB below them is the floor
    let mut sorted = decibels.clone();
    let at = sorted.len() * 97 / 100;
    let top = *sorted.select_nth_unstable_by(at, f32::total_cmp).1;
    let bands = decibels
        .iter()
        .map(|db| (((db - top) / 38.0 + 1.0).clamp(0.0, 1.0) * 255.0) as u8)
        .collect();
    // square the bass for punch; the waveform gets a square root so quiet passages still show
    Some(Analysis {
        bands,
        bass: normalise(&energy).into_iter().map(|e| e * e).collect(),
        peaks: normalise(&loudness).into_iter().map(f32::sqrt).collect(),
    })
}

/// Follows the bass of whatever is playing, smoothed so a beat snaps up and then falls away.
pub struct Reactive {
    tx: Sender<(PathBuf, Analysis)>,
    rx: Receiver<(PathBuf, Analysis)>,
    track: Option<PathBuf>,
    levels: Vec<f32>,
    peaks: Vec<f32>,
    bands: Vec<u8>,
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
            peaks: Vec::new(),
            bands: Vec::new(),
            anchor: (0.0, Instant::now()),
            pulse: 0.0,
        }
    }
}

impl Reactive {
    /// Starts analysing `path` if it is not the current track, and collects a finished analysis.
    pub fn track(&mut self, ctx: &egui::Context, path: &Path) {
        if self.track.as_deref() != Some(path) {
            self.track = Some(path.to_path_buf());
            self.levels.clear();
            self.peaks.clear();
            self.bands.clear();
            let (path, tx, ctx) = (path.to_path_buf(), self.tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                if let Some(analysis) = analyse(&path) {
                    let _ = tx.send((path, analysis));
                    ctx.request_repaint();
                }
            });
        }
        while let Ok((done, analysis)) = self.rx.try_recv() {
            if self.track.as_deref() == Some(done.as_path()) {
                self.levels = analysis.bass;
                self.peaks = analysis.peaks;
                self.bands = analysis.bands;
            }
        }
    }

    /// The whole track's loudness for the waveform; empty until the analysis finishes, or for
    /// formats the decoder lacks.
    pub fn peaks(&self) -> &[f32] {
        &self.peaks
    }

    /// The spectrum (0 to 1 per band) at the current moment, or `None` before the analysis is
    /// done. Blends the two nearest steps so motion stays smooth at any frame rate.
    pub fn spectrum(&self) -> Option<[f32; BANDS]> {
        let at = self.now() * RATE as f64;
        let (i, mix) = (at as usize, at.fract() as f32);
        let frame = |i: usize| self.bands.get(i * BANDS..(i + 1) * BANDS);
        let (a, b) = (frame(i)?, frame(i + 1).unwrap_or(frame(i)?));
        let mut out = [0.0; BANDS];
        for (k, v) in out.iter_mut().enumerate() {
            *v = (f32::from(a[k]) * (1.0 - mix) + f32::from(b[k]) * mix) / 255.0;
        }
        Some(out)
    }

    /// The playback position, running on between the player's reports.
    pub fn now(&self) -> f64 {
        self.anchor.0 + self.anchor.1.elapsed().as_secs_f64().min(0.5)
    }

    /// The smoothed bass level (0 to 1) for this frame. Until the analysis finishes, or for
    /// formats the decoder lacks, the level stays at zero.
    pub fn level(&mut self, ctx: &egui::Context, path: &Path, position: f64, playing: bool) -> f32 {
        self.track(ctx, path);
        if position != self.anchor.0 {
            self.anchor = (position, Instant::now());
        }
        let now = self.now();
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
        let analysis = analyse(&path).expect("decodes");
        let (levels, peaks) = (analysis.bass, analysis.peaks);
        // the 60 Hz tone sits in the lowest bars, the silent half in none
        let band = |t: usize, b: usize| analysis.bands[t * BANDS + b];
        let tone = (0..6).map(|b| band(10, b)).max().unwrap();
        assert!(tone > 200, "tone: {tone}");
        assert!(band(10, BANDS - 1) < 60, "treble: {}", band(10, BANDS - 1));
        assert!(band(50, 0) < 5, "silence: {}", band(50, 0));
        assert!((55..=65).contains(&levels.len()), "{}", levels.len());
        assert!(levels[10] > 0.8, "loud half: {}", levels[10]);
        assert!(levels[50] < 0.05, "silent half: {}", levels[50]);
        assert!(
            peaks[10] > 0.8 && peaks[50] < 0.05,
            "{} {}",
            peaks[10],
            peaks[50]
        );
    }
}

#[cfg(test)]
mod equivalence {
    use super::*;

    /// The transform as it was before the rotations were cached.
    fn direct_fft(re: &mut [f32], im: &mut [f32]) {
        let n = re.len();
        let mut j = 0;
        for i in 1..n {
            let mut bit = n >> 1;
            while j & bit != 0 {
                j ^= bit;
                bit >>= 1;
            }
            j |= bit;
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let step = -TAU / len as f32;
            for start in (0..n).step_by(len) {
                for k in 0..len / 2 {
                    let (sin, cos) = (step * k as f32).sin_cos();
                    let (a, b) = (start + k, start + k + len / 2);
                    let (tr, ti) = (re[b] * cos - im[b] * sin, re[b] * sin + im[b] * cos);
                    (re[b], im[b]) = (re[a] - tr, im[a] - ti);
                    (re[a], im[a]) = (re[a] + tr, im[a] + ti);
                }
            }
            len <<= 1;
        }
    }

    fn noise(count: usize, mut seed: u32) -> Vec<f32> {
        (0..count)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn cached_rotations_give_the_same_transform_to_rounding() {
        for n in [2, 8, 256, FFT] {
            let (mut re, mut im) = (noise(n, 7), noise(n, 99));
            let (mut direct_re, mut direct_im) = (re.clone(), im.clone());
            fft(&mut re, &mut im);
            direct_fft(&mut direct_re, &mut direct_im);
            let scale = direct_re
                .iter()
                .chain(&direct_im)
                .fold(0.0_f32, |m, v| m.max(v.abs()));
            for (a, b) in re.iter().chain(&im).zip(direct_re.iter().chain(&direct_im)) {
                // a few units in the last place; the bars store whole steps of 1/255
                assert!((a - b).abs() <= scale * 1e-5, "{a} against {b} at size {n}");
            }
        }
    }

    #[test]
    fn selecting_the_percentile_matches_sorting() {
        for count in [1, 2, 7, 100, 4097] {
            let values: Vec<f32> = noise(count, count as u32)
                .iter()
                .map(|v| v.abs() * 3.0)
                .collect();
            let mut sorted = values.clone();
            sorted.sort_by(f32::total_cmp);
            let top = sorted[sorted.len() * 95 / 100].max(1e-4);
            let sorted_way: Vec<f32> = values.iter().map(|v| (v / top).min(1.0)).collect();
            let selected_way = normalise(&values);
            assert!(
                sorted_way
                    .iter()
                    .zip(&selected_way)
                    .all(|(a, b)| a.to_bits() == b.to_bits())
            );
        }
    }
}
