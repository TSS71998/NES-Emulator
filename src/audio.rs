use sdl2::audio::{AudioCallback, AudioDevice, AudioSpecDesired};
use sdl2::AudioSubsystem;
use std::collections::VecDeque;
use std::time::Instant;

const TARGET_LATENCY: f64 = 0.025;

const DEVICE_BUFFER_SAMPLES: u16 = 256;

const MAX_RATE_ADJUST: f64 = 0.005;

const KP: f64 = 0.02;
const KI: f64 = 0.00005;

struct SampleBuffer {
    samples: VecDeque<f32>,
    last_pull: Instant,
    last_pull_len: usize
}

impl AudioCallback for SampleBuffer {
    type Channel = f32;

    fn callback(&mut self, out: &mut [f32]) {
        for sample in out.iter_mut() {
            *sample = self.samples.pop_front().unwrap_or(0.0);
        }
        self.last_pull = Instant::now();
        self.last_pull_len = out.len();
    }
}

pub struct AudioOutput {
    device: AudioDevice<SampleBuffer>,
    sample_rate: u32,
    target_samples: f64,
    smoothed_fill: f64,
    rate_trim: f64
}

impl AudioOutput {
    pub fn new(audio: &AudioSubsystem) -> Self {
        let desired = AudioSpecDesired {
            freq: Some(44100),
            channels: Some(1),
            samples: Some(DEVICE_BUFFER_SAMPLES)
        };
        let device = audio
            .open_playback(None, &desired, |spec| SampleBuffer {
                samples: VecDeque::with_capacity(spec.freq as usize / 4),
                last_pull: Instant::now(),
                last_pull_len: 0
            })
            .unwrap();
        let sample_rate = device.spec().freq as u32;
        let target_samples = TARGET_LATENCY * sample_rate as f64;
        device.resume();

        AudioOutput {
            device,
            sample_rate,
            target_samples,
            smoothed_fill: target_samples,
            rate_trim: 0.0
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn push(&mut self, samples: &[f32]) -> f64 {
        let sample_rate = self.sample_rate as f64;
        let target = self.target_samples as usize;
        let max_samples = target * 2;

        let mut buffer = self.device.lock();

        let played = (buffer.last_pull.elapsed().as_secs_f64() * sample_rate) as usize;
        let in_device = buffer.last_pull_len.saturating_sub(played);
        let buffered = buffer.samples.len();

        if buffered == 0 || buffered + in_device > max_samples {
            // Underrun or runaway backlog: resync to the target fill level.
            buffer.samples.clear();
            let pad = target.saturating_sub(in_device + samples.len());
            buffer.samples.extend(std::iter::repeat(0.0).take(pad));
            self.smoothed_fill = self.target_samples;
            self.rate_trim = 0.0;
        }
        buffer.samples.extend(samples.iter().copied());

        let fill = (buffer.samples.len() + in_device) as f64;
        drop(buffer);

        self.smoothed_fill += 0.05 * (fill - self.smoothed_fill);

        let error = (self.target_samples - self.smoothed_fill) / self.target_samples;
        self.rate_trim = (self.rate_trim + KI * error).clamp(-MAX_RATE_ADJUST, MAX_RATE_ADJUST);
        1.0 + (self.rate_trim + KP * error).clamp(-MAX_RATE_ADJUST, MAX_RATE_ADJUST)
    }
}
