// src/audio_loader.rs

use std::fs::File;
use std::path::Path;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::audio::SampleBuffer; // Keep this for Symphonia's internal buffering

// --- Add rubato imports ---
use rubato::{Resampler, SincFixedIn, SincInterpolationType, SincInterpolationParameters, WindowFunction};

/// Loads an audio file, decodes it, converts to mono, and resamples to target_sample_rate.
/// Returns a Vec<f32> of audio samples or an error string.
pub fn load_audio_file(
    file_path: &Path,
    target_sample_rate: u32,
) -> Result<Vec<f32>, String> {
    let src = File::open(file_path).map_err(|e| format!("Failed to open file: {}", e))?;
    let mss = MediaSourceStream::new(Box::new(src), Default::default());

    let mut hint = Hint::new();
    if let Some(extension) = file_path.extension().and_then(|s| s.to_str()) {
        hint.with_extension(extension);
    }

    let meta_opts: MetadataOptions = Default::default();
    let fmt_opts: FormatOptions = Default::default();

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &fmt_opts, &meta_opts)
        .map_err(|e| format!("Unsupported format or error probing file: {}", e))?;

    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL && t.codec_params.sample_rate.is_some())
        .ok_or_else(|| "No compatible audio track found".to_string())?;

    let dec_opts: DecoderOptions = Default::default();
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &dec_opts)
        .map_err(|e| format!("Failed to make decoder: {}", e))?;

    let track_id = track.id;
    let mut collected_mono_samples: Vec<f32> = Vec::new(); // Will hold all mono samples before resampling
    let mut input_file_sample_rate: Option<u32> = None; // To store the original sample rate

    // The audio decoding loop.
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(ref err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                break; // End of file
            }
            Err(SymphoniaError::ResetRequired) => {
                // Simplified handling for ResetRequired. A more robust solution might re-probe.
                return Err("Unhandled ResetRequired during packet reading. Stream parameters might have changed.".to_string());
            }
            Err(err) => {
                return Err(format!("Error reading next packet: {}", err));
            }
        };

        if packet.track_id() != track_id {
            continue; // Skip packets not for our selected track
        }

        match decoder.decode(&packet) {
            Ok(decoded_packet_ref) => {
                let spec = *decoded_packet_ref.spec();
                // Store the original sample rate from the first valid decoded packet
                if input_file_sample_rate.is_none() {
                    input_file_sample_rate = Some(spec.rate);
                } else if input_file_sample_rate != Some(spec.rate) {
                    // This case (sample rate changing mid-stream) is rare for files but possible.
                    // For simplicity, we'll error out. Robust handling would be complex.
                    return Err(format!(
                        "Sample rate changed mid-stream from {:?} to {}. This is not supported by the simple loader.",
                        input_file_sample_rate, spec.rate
                    ));
                }


                let mut sample_buf = SampleBuffer::<f32>::new(
                    decoded_packet_ref.capacity() as u64,
                    spec,
                );
                sample_buf.copy_interleaved_ref(decoded_packet_ref);

                let samples_this_packet = sample_buf.samples();
                match spec.channels.count() {
                    1 => { // Mono
                        collected_mono_samples.extend_from_slice(samples_this_packet);
                    }
                    2 => { // Stereo -> Mono by averaging
                        for i in (0..samples_this_packet.len()).step_by(2) {
                            collected_mono_samples.push((samples_this_packet[i] + samples_this_packet[i+1]) / 2.0);
                        }
                    }
                    _ => { // More than 2 channels -> Mono by taking the first channel
                        for i in (0..samples_this_packet.len()).step_by(spec.channels.count()) {
                            collected_mono_samples.push(samples_this_packet[i]);
                        }
                        eprintln!("Warning: Audio has {} channels. Taking first channel only.", spec.channels.count());
                    }
                }
            }
            Err(SymphoniaError::DecodeError(err)) => {
                // Non-fatal decode errors can be logged.
                eprintln!("Decode error: {}", err);
            }
            Err(err) => {
                // Other errors during decode are treated as fatal.
                return Err(format!("Fatal decoding error: {}", err));
            }
        }
    }

    if collected_mono_samples.is_empty() {
        return Err("No audio samples were decoded from the file.".to_string());
    }

    // Ensure we got a sample rate from the file.
    let original_sample_rate = match input_file_sample_rate {
        Some(rate) => rate,
        None => return Err("Could not determine the original sample rate from the audio file.".to_string()),
    };

    // --- RESAMPLING STEP using Rubato ---
    if original_sample_rate != target_sample_rate {
        println!(
            "Resampling audio from {} Hz to {} Hz...",
            original_sample_rate, target_sample_rate
        );

        // Prepare input for Rubato: Vec<Vec<f32>> (outer Vec for channels, inner for samples)
        let waves_in = vec![collected_mono_samples]; // Our mono samples as the first (and only) channel

        // Choose resampler parameters
        let sinc_len = 256; // Length of the sinc interpolation filter, larger is generally better quality
        let window_type = WindowFunction::BlackmanHarris2; // A good general-purpose window

        // Parameters for SincFixedIn. Oversampling factor can greatly affect quality/speed.
        let params = SincInterpolationParameters {
            sinc_len,
            f_cutoff: 0.95, // Cutoff frequency, relative to Nyquist frequency of the lower sample rate
            interpolation: SincInterpolationType::Linear, // Or Cubic for better quality
            oversampling_factor: 128, // Lower for faster, higher for better quality (e.g., 256)
            window: window_type,
        };

        // Create the resampler
        // The first argument is the ratio: f_out / f_in
        // The second argument `max_resample_ratio_relative` can be used if you provide `f_out_custom` to `process`.
        // We provide a fixed ratio, so it's less critical but should be >= 1.0.
        // The `input_frames_next_call` is a hint for buffer allocation.
        let mut resampler = SincFixedIn::<f32>::new(
            target_sample_rate as f64 / original_sample_rate as f64, // Resampling ratio
            2.0, // max_resample_ratio_relative, recommend >= 1.0
            params,
            waves_in[0].len(), // Initial hint for input buffer length
            1,                 // Number of channels (mono)
        ).map_err(|e| format!("Failed to create resampler: {:?}", e))?;

        // Process the audio waves.
        // `process` can take an optional pre-allocated output buffer, or it will allocate one.
        let waves_out = resampler.process(&waves_in, None)
            .map_err(|e| format!("Error during resampling: {:?}", e))?;

        // `waves_out` is Vec<Vec<f32>>. Since we resampled mono, it contains one Vec<f32>.
        if let Some(resampled_mono_samples) = waves_out.into_iter().next() {
            println!(
                "Resampling complete. Original samples: {}, Resampled samples: {}",
                waves_in[0].len(), resampled_mono_samples.len()
            );
            Ok(resampled_mono_samples)
        } else {
            // Should not happen if resampling was successful and input was not empty
            Err("Resampling produced no output, though it should have.".to_string())
        }
    } else {
        // No resampling needed, sample rates already match.
        println!(
            "No resampling needed. Audio already at target sample rate: {} Hz.",
            target_sample_rate
        );
        Ok(collected_mono_samples)
    }
}