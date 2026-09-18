//! Microphone capture and speaker playback via CoreAudio (cpal), resampled
//! to and from the 24 kHz mono PCM16 that GPT-Live speaks.

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::mpsc;

pub const LIVE_RATE: u32 = 24_000;
/// Mic frames are batched to this many 24 kHz samples (50 ms) per WebSocket event.
pub const MIC_CHUNK: usize = 1_200;
/// Playback queue high-water mark: anything beyond this is dropped so barge-in
/// never has to wait for stale audio. GPT-Live paces output in real time so
/// the queue normally stays far below this.
const MAX_QUEUED_SAMPLES: usize = LIVE_RATE as usize * 6;

/// Stateful linear-interpolation resampler. Good enough for speech, no
/// dependencies, and deterministic.
pub struct Resampler {
    ratio: f64, // input samples per output sample
    pos: f64,
    last: f32,
    primed: bool,
}

impl Resampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        Self {
            ratio: from_rate as f64 / to_rate as f64,
            pos: 0.0,
            last: 0.0,
            primed: false,
        }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        if (self.ratio - 1.0).abs() < 1e-9 {
            out.extend_from_slice(input);
            return;
        }
        // Virtual buffer = [last] ++ input, index 0 is `last`.
        let mut buf: Vec<f32> = Vec::with_capacity(input.len() + 1);
        buf.push(if self.primed { self.last } else { input[0] });
        buf.extend_from_slice(input);
        let n = buf.len();
        let mut pos = self.pos;
        while pos + 1.0 < n as f64 {
            let i = pos.floor() as usize;
            let frac = (pos - i as f64) as f32;
            let a = buf[i];
            let b = buf[i + 1];
            out.push(a + (b - a) * frac);
            pos += self.ratio;
        }
        self.pos = pos - (n as f64 - 1.0);
        self.last = buf[n - 1];
        self.primed = true;
    }
}

pub fn rms_i16(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|s| (*s as f64 / 32768.0).powi(2)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}

pub fn f32_to_i16(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * 32767.0) as i16
}

pub fn i16_to_le_bytes(samples: &[i16]) -> Vec<u8> {
    let mut v = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

pub fn le_bytes_to_i16(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Atomic f32 helper for energy meters.
#[derive(Default)]
pub struct Meter(AtomicU32);
impl Meter {
    pub fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// Shared playback state written by the network task and drained by CoreAudio.
pub struct Playback {
    queue: Mutex<VecDeque<i16>>, // 24 kHz mono
    pub out_level: Meter,
}

impl Playback {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::with_capacity(LIVE_RATE as usize * 2)),
            out_level: Meter::default(),
        })
    }
    pub fn push(&self, samples: &[i16]) {
        let mut q = self.queue.lock();
        q.extend(samples.iter().copied());
        if q.len() > MAX_QUEUED_SAMPLES {
            let drop = q.len() - MAX_QUEUED_SAMPLES;
            q.drain(..drop);
        }
    }
    pub fn queued_ms(&self) -> u64 {
        (self.queue.lock().len() as u64) * 1000 / LIVE_RATE as u64
    }
    fn pop_into(&self, out: &mut [f32]) {
        let mut q = self.queue.lock();
        let mut acc = 0.0f64;
        for slot in out.iter_mut() {
            let s = q.pop_front().unwrap_or(0);
            let f = s as f32 / 32768.0;
            acc += (f as f64) * (f as f64);
            *slot = f;
        }
        let rms = (acc / out.len().max(1) as f64).sqrt() as f32;
        self.out_level.set(rms);
    }
}

pub struct AudioHandles {
    /// 24 kHz PCM16 mic chunks of `MIC_CHUNK` samples.
    pub mic_rx: mpsc::Receiver<Vec<i16>>,
    pub in_level: Arc<Meter>,
    pub playback: Arc<Playback>,
    pub shutdown: std::sync::mpsc::Sender<()>,
}

/// Start capture and playback on a dedicated OS thread (cpal streams are not
/// `Send` on macOS). Returns once both streams are running or an error occurred.
pub fn start() -> Result<AudioHandles> {
    let (mic_tx, mic_rx) = mpsc::channel::<Vec<i16>>(64);
    let in_level = Arc::new(Meter::default());
    let playback = Playback::new();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel::<()>();

    let in_level_t = in_level.clone();
    let playback_t = playback.clone();
    std::thread::Builder::new()
        .name("thursday-agent-audio".into())
        .spawn(move || {
            let result = (|| -> Result<(cpal::Stream, cpal::Stream)> {
                let host = cpal::default_host();
                let input = host
                    .default_input_device()
                    .ok_or_else(|| anyhow!("no default input device (microphone)"))?;
                let output = host
                    .default_output_device()
                    .ok_or_else(|| anyhow!("no default output device (speakers)"))?;

                let in_cfg = input.default_input_config().context("input config")?;
                let out_cfg = output.default_output_config().context("output config")?;
                tracing::info!(
                    input = ?input.description().ok().map(|d| d.name().to_string()),
                    in_rate = in_cfg.sample_rate(),
                    in_channels = in_cfg.channels(),
                    output = ?output.description().ok().map(|d| d.name().to_string()),
                    out_rate = out_cfg.sample_rate(),
                    out_channels = out_cfg.channels(),
                    "audio devices"
                );

                // ---- input ----
                let in_channels = in_cfg.channels() as usize;
                let mut in_resampler = Resampler::new(in_cfg.sample_rate(), LIVE_RATE);
                let mut mono = Vec::<f32>::new();
                let mut resampled = Vec::<f32>::new();
                let mut pending = Vec::<i16>::with_capacity(MIC_CHUNK * 2);
                let in_stream_cfg: cpal::StreamConfig = in_cfg.clone().into();
                let in_stream = input.build_input_stream(
                    in_stream_cfg,
                    move |data: &[f32], _| {
                        mono.clear();
                        for frame in data.chunks(in_channels) {
                            let s = frame.iter().sum::<f32>() / in_channels as f32;
                            mono.push(s);
                        }
                        resampled.clear();
                        in_resampler.process(&mono, &mut resampled);
                        pending.extend(resampled.iter().map(|s| f32_to_i16(*s)));
                        while pending.len() >= MIC_CHUNK {
                            let chunk: Vec<i16> = pending.drain(..MIC_CHUNK).collect();
                            in_level_t.set(rms_i16(&chunk));
                            // Drop on backpressure rather than block the audio thread.
                            let _ = mic_tx.try_send(chunk);
                        }
                    },
                    |e| tracing::error!("input stream error: {e}"),
                    None,
                )?;

                // ---- output ----
                let out_channels = out_cfg.channels() as usize;
                let mut out_resampler = Resampler::new(LIVE_RATE, out_cfg.sample_rate());
                let mut src = Vec::<f32>::new();
                let mut dst = Vec::<f32>::new();
                let mut carry = VecDeque::<f32>::new();
                let out_stream_cfg: cpal::StreamConfig = out_cfg.clone().into();
                let out_ratio = LIVE_RATE as f64 / out_cfg.sample_rate() as f64;
                let out_stream = output.build_output_stream(
                    out_stream_cfg,
                    move |data: &mut [f32], _| {
                        let frames = data.len() / out_channels;
                        // How many 24 kHz samples do we need to fill `frames` output frames?
                        while carry.len() < frames {
                            let need = ((frames - carry.len()) as f64 * out_ratio).ceil() as usize + 2;
                            src.resize(need, 0.0);
                            playback_t.pop_into(&mut src);
                            dst.clear();
                            out_resampler.process(&src, &mut dst);
                            carry.extend(dst.iter().copied());
                        }
                        for frame in data.chunks_mut(out_channels) {
                            let s = carry.pop_front().unwrap_or(0.0);
                            for ch in frame.iter_mut() {
                                *ch = s;
                            }
                        }
                    },
                    |e| tracing::error!("output stream error: {e}"),
                    None,
                )?;

                in_stream.play()?;
                out_stream.play()?;
                Ok((in_stream, out_stream))
            })();

            match result {
                Ok((_in_stream, _out_stream)) => {
                    let _ = ready_tx.send(Ok(()));
                    // Keep streams alive until shutdown.
                    let _ = shutdown_rx.recv();
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })
        .context("spawning audio thread")?;

    ready_rx
        .recv()
        .map_err(|_| anyhow!("audio thread died before reporting readiness"))??;

    Ok(AudioHandles {
        mic_rx,
        in_level,
        playback,
        shutdown: shutdown_tx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_halves_48k_to_24k() {
        let mut r = Resampler::new(48_000, 24_000);
        let input: Vec<f32> = (0..960).map(|i| (i as f32 / 960.0)).collect();
        let mut out = Vec::new();
        r.process(&input, &mut out);
        assert!((out.len() as i64 - 480).abs() <= 1, "got {}", out.len());
        // Monotonic ramp stays monotonic.
        assert!(out.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn resampler_is_stateful_across_calls() {
        let mut r = Resampler::new(44_100, 24_000);
        let input: Vec<f32> = vec![0.5; 4410];
        let mut a = Vec::new();
        r.process(&input[..2000], &mut a);
        r.process(&input[2000..], &mut a);
        assert!((a.len() as i64 - 2400).abs() <= 2, "got {}", a.len());
        assert!(a.iter().all(|s| (s - 0.5).abs() < 1e-5));
    }

    #[test]
    fn pcm_roundtrip() {
        let s = vec![0i16, 1, -1, 32767, -32768];
        assert_eq!(le_bytes_to_i16(&i16_to_le_bytes(&s)), s);
    }

    #[test]
    fn playback_queue_drops_oldest_when_overfull() {
        let p = Playback::new();
        p.push(&vec![1i16; MAX_QUEUED_SAMPLES]);
        p.push(&vec![2i16; 10]);
        assert_eq!(p.queue.lock().len(), MAX_QUEUED_SAMPLES);
        assert_eq!(*p.queue.lock().back().unwrap(), 2);
    }
}
