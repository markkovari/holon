use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

#[cfg(target_os = "macos")]
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
#[cfg(target_os = "macos")]
use cpal::{SampleFormat, Stream};

#[cfg(target_os = "macos")]
pub struct AudioStream(#[allow(dead_code)] Stream);

#[cfg(not(target_os = "macos"))]
pub struct AudioStream;

/// Calculates RMS energy of 16-bit PCM samples normalized to 0.0..1.0.
pub fn rms_energy(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    let mean_sq = sum_sq / (samples.len() as f64);
    let rms = mean_sq.sqrt();
    (rms / 32768.0) as f32
}

/// Simple linear resampler for single-channel 16-bit PCM.
pub fn resample_linear(input: &[i16], in_rate: u32, out_rate: u32) -> Vec<i16> {
    if input.is_empty() || in_rate == 0 || out_rate == 0 || in_rate == out_rate {
        return input.to_vec();
    }
    let ratio = in_rate as f64 / out_rate as f64;
    let out_len = ((input.len() as f64) / ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);

    for i in 0..out_len {
        let src_idx = (i as f64) * ratio;
        let idx = src_idx.floor() as usize;
        let frac = src_idx - (idx as f64);

        if idx + 1 < input.len() {
            let s0 = input[idx] as f64;
            let s1 = input[idx + 1] as f64;
            let val = s0 + frac * (s1 - s0);
            out.push(val.round().clamp(-32768.0, 32767.0) as i16);
        } else if idx < input.len() {
            out.push(input[idx]);
        }
    }
    out
}

/// Writes 16-bit mono PCM samples to a WAV file at the given sample rate.
pub fn write_wav(samples: &[i16], sample_rate: u32, path: &Path) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .map_err(|e| format!("failed to create wav at {}: {e}", path.display()))?;
    for &sample in samples {
        writer.write_sample(sample).map_err(|e| format!("failed to write sample to wav: {e}"))?;
    }
    writer.finalize().map_err(|e| format!("failed to finalize wav: {e}"))?;
    Ok(())
}

/// Reads 16-bit mono PCM samples from a WAV file.
pub fn read_wav(path: &Path) -> Result<(Vec<i16>, u32), String> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| format!("failed to open wav at {}: {e}", path.display()))?;
    let spec = reader.spec();
    let samples: Result<Vec<i16>, _> = reader.samples::<i16>().collect();
    let samples = samples.map_err(|e| format!("failed to read samples: {e}"))?;
    Ok((samples, spec.sample_rate))
}

/// Shared thread-safe audio accumulator.
#[derive(Clone, Default)]
pub struct AudioCollector {
    buffer: Arc<Mutex<Vec<i16>>>,
}

impl AudioCollector {
    pub fn new() -> Self {
        Self { buffer: Arc::new(Mutex::new(Vec::new())) }
    }

    pub fn push_samples(&self, samples: &[i16]) {
        self.buffer.lock().unwrap().extend_from_slice(samples);
    }

    pub fn take_samples(&self) -> Vec<i16> {
        let mut g = self.buffer.lock().unwrap();
        std::mem::take(&mut *g)
    }

    pub fn len(&self) -> usize {
        self.buffer.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.lock().unwrap().is_empty()
    }
}

/// Starts capturing microphone input and delivers 16kHz mono PCM frames.
#[cfg(target_os = "macos")]
pub fn start_mic_capture(
    target_sample_rate: u32,
    tx: Sender<Vec<i16>>,
) -> Result<AudioStream, String> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "no default audio input device found".to_string())?;

    let default_config = device
        .default_input_config()
        .map_err(|e| format!("failed to get default input config: {e}"))?;

    let channels = default_config.channels() as usize;
    let sample_rate = default_config.sample_rate().0;
    let format = default_config.sample_format();

    let err_fn = |err| eprintln!("audio input error: {err}");

    let stream = match format {
        SampleFormat::F32 => {
            let tx = tx.clone();
            device
                .build_input_stream(
                    &default_config.into(),
                    move |data: &[f32], _| {
                        let mono: Vec<i16> = data
                            .chunks(channels)
                            .map(|c| (c[0].clamp(-1.0, 1.0) * 32767.0) as i16)
                            .collect();
                        let resampled = resample_linear(&mono, sample_rate, target_sample_rate);
                        if !resampled.is_empty() {
                            let _ = tx.send(resampled);
                        }
                    },
                    err_fn,
                    None,
                )
                .map_err(|e| format!("failed to build f32 input stream: {e}"))?
        }
        SampleFormat::I16 => {
            let tx = tx.clone();
            device
                .build_input_stream(
                    &default_config.into(),
                    move |data: &[i16], _| {
                        let mono: Vec<i16> = data.chunks(channels).map(|c| c[0]).collect();
                        let resampled = resample_linear(&mono, sample_rate, target_sample_rate);
                        if !resampled.is_empty() {
                            let _ = tx.send(resampled);
                        }
                    },
                    err_fn,
                    None,
                )
                .map_err(|e| format!("failed to build i16 input stream: {e}"))?
        }
        other => return Err(format!("unsupported sample format: {:?}", other)),
    };

    stream.play().map_err(|e| format!("failed to start audio stream: {e}"))?;
    Ok(AudioStream(stream))
}

#[cfg(not(target_os = "macos"))]
pub fn start_mic_capture(
    _target_sample_rate: u32,
    _tx: Sender<Vec<i16>>,
) -> Result<AudioStream, String> {
    Err("microphone capture is supported on macOS CoreAudio".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_calculation_matches_known_signals() {
        let silent = vec![0i16; 1000];
        assert_eq!(rms_energy(&silent), 0.0);

        let loud = vec![32767i16; 1000];
        let loud_rms = rms_energy(&loud);
        assert!((loud_rms - 1.0).abs() < 0.001);

        let square: Vec<i16> = (0..1000).map(|i| if i % 2 == 0 { 16384 } else { -16384 }).collect();
        let square_rms = rms_energy(&square);
        assert!((square_rms - 0.5).abs() < 0.001);
    }

    #[test]
    fn wav_round_trip_preserves_audio() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.wav");
        let original: Vec<i16> = (0..1600).map(|i| ((i * 30) % 32000) as i16).collect();

        write_wav(&original, 16000, &file).unwrap();
        let (loaded, rate) = read_wav(&file).unwrap();

        assert_eq!(rate, 16000);
        assert_eq!(loaded, original);
    }

    #[test]
    fn resampler_scales_sample_rates() {
        let src = vec![1000i16, 2000, 3000, 4000];
        let resampled = resample_linear(&src, 32000, 16000);
        assert_eq!(resampled.len(), 2);
    }
}
