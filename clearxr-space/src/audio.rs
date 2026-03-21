/// Background audio playback: loads a PCM WAV file and loops it on the default
/// output device via WASAPI (cpal).

use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use log::{info, warn};
use std::path::Path;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};

/// Spawn a background thread that plays the given WAV file in a loop until
/// `keep_running` is set to false.  Returns immediately.
pub fn start_looped(wav_path: &Path, keep_running: Arc<AtomicBool>) -> Result<()> {
    // Load the entire WAV into memory up front (on the calling thread).
    let reader = hound::WavReader::open(wav_path)?;
    let spec = reader.spec();
    info!(
        "Audio: {} Hz, {} ch, {}-bit, {} samples",
        spec.sample_rate, spec.channels, spec.bits_per_sample, reader.len(),
    );

    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .into_samples::<i16>()
            .filter_map(|s| s.ok())
            .map(|s| s as f32 / i16::MAX as f32)
            .collect(),
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .filter_map(|s| s.ok())
            .collect(),
    };

    if samples.is_empty() {
        anyhow::bail!("WAV file is empty");
    }

    let wav_channels = spec.channels as usize;
    let wav_sample_rate = spec.sample_rate;

    // The cpal Stream is !Send, so we must create and hold it on the audio
    // thread.  Pass the sample data in via Arc.
    let samples = Arc::new(samples);

    std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || {
            if let Err(e) = audio_thread(samples, wav_channels, wav_sample_rate, keep_running) {
                warn!("Audio thread error: {}", e);
            }
        })?;

    Ok(())
}

fn audio_thread(
    samples: Arc<Vec<f32>>,
    wav_channels: usize,
    wav_sample_rate: u32,
    keep_running: Arc<AtomicBool>,
) -> Result<()> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("No audio output device found"))?;

    info!("Audio device: {}", device.name().unwrap_or_default());

    let config = device.default_output_config()?;
    info!("Audio output config: {:?}", config);

    let out_channels = config.channels() as usize;
    let out_sample_rate = config.sample_rate().0;

    if wav_sample_rate != out_sample_rate {
        info!(
            "Audio resampling: {} Hz → {} Hz",
            wav_sample_rate, out_sample_rate
        );
    }

    // Fractional cursor for sample-rate conversion (linear interpolation).
    // Using AtomicU64 to store the cursor as fixed-point: upper 32 bits = integer
    // part (frame index), but that limits us. Instead, use a Mutex<f64> for simplicity.
    let cursor = Arc::new(std::sync::Mutex::new(0.0_f64));
    let rate_ratio = wav_sample_rate as f64 / out_sample_rate as f64;

    let stream = {
        let samples = samples.clone();
        let cursor = cursor.clone();
        let keep_running = keep_running.clone();

        device.build_output_stream(
            &config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                if !keep_running.load(Ordering::Relaxed) {
                    data.fill(0.0);
                    return;
                }

                let total_wav_frames = samples.len() / wav_channels;
                let mut pos = *cursor.lock().unwrap();

                for frame in data.chunks_exact_mut(out_channels) {
                    // Wrap for looping
                    if pos >= total_wav_frames as f64 {
                        pos -= total_wav_frames as f64;
                    }

                    let idx = pos as usize;
                    let frac = (pos - idx as f64) as f32;
                    let next = if idx + 1 >= total_wav_frames { 0 } else { idx + 1 };

                    for (ch, sample) in frame.iter_mut().enumerate() {
                        let wav_ch = ch % wav_channels;
                        let s0 = samples[idx * wav_channels + wav_ch];
                        let s1 = samples[next * wav_channels + wav_ch];
                        *sample = s0 + (s1 - s0) * frac; // linear interpolation
                    }

                    pos += rate_ratio;
                }

                *cursor.lock().unwrap() = pos;
            },
            move |err| {
                warn!("Audio stream error: {}", err);
            },
            None,
        )?
    };

    stream.play()?;
    info!("Audio playback started (looping).");

    // Keep the stream alive until the app exits.
    while keep_running.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Stream drops here → playback stops.
    info!("Audio thread exiting.");
    Ok(())
}
