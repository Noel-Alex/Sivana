// In a new src/audio_loader.rs file, or within main.rs

// (If in a new file, remember to add `mod audio_loader;` in main.rs and `use audio_loader::load_audio_file;`)

use std::fs::File;
use std::path::Path;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::audio::{AudioBufferRef, SampleBuffer};

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

    let mut format = probed.format; // `format` is now mutable as we'll call next_packet() on it

    // Get the default track.
    // THIS IS WHERE `track` IS DEFINED. IT MUST BE IN SCOPE FOR THE DECODER.
    let track = format  // Make sure this 'format' is the mutable one from above
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL && t.codec_params.sample_rate.is_some()) // Ensure it's an audio track with a sample rate
        .ok_or_else(|| "No compatible audio track found".to_string())?;

    // Create a decoder for the track.
    // `track` (defined above) IS USED HERE.
    let dec_opts: DecoderOptions = Default::default();
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &dec_opts) // This line was giving the error
        .map_err(|e| format!("Failed to make decoder: {}", e))?;

    let track_id = track.id; // Store track_id for packet filtering
    let mut decoded_samples: Vec<f32> = Vec::new();

    // The audio decoding loop.
    loop {
        // Get the next packet from the format reader.
        let packet = match format.next_packet() { // `format` must be mutable here
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(ref err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(SymphoniaError::ResetRequired) => {
                // The track list has been changed. Re-probe for the track.
                println!("Info: ResetRequired encountered. Re-probing for track.");
                // Resetting and re-probing logic can be complex. For a simple loader,
                // if this happens mid-stream for a single file, it might indicate an unusual file.
                // A robust solution might re-evaluate format.tracks().
                // For now, let's try to find the track again using the current track_id
                // This is a simplified handling. A more robust solution would re-evaluate the tracks.
                let current_track_params = format.tracks().iter().find(|t| t.id == track_id).map(|t| &t.codec_params);
                if let Some(params) = current_track_params {
                    match symphonia::default::get_codecs().make(params, &dec_opts) {
                        Ok(new_decoder) => decoder = new_decoder,
                        Err(e) => return Err(format!("Failed to remake decoder after reset: {}", e)),
                    }
                } else {
                    return Err("Track disappeared after ResetRequired".to_string());
                }
                // After reset, the packet that caused it should be re-tried.
                // Symphonia's examples often have a loop structure that handles this.
                // For simplicity here, we might just skip the packet or error out.
                // Let's assume for now that ResetRequired is rare for simple file playback and break.
                // A better approach would be to re-fetch the packet or handle the reset more gracefully.
                // For now, let's just signal an error if we can't easily recover.
                return Err("Unhandled ResetRequired during packet reading.".to_string());

            }
            Err(err) => {
                return Err(format!("Error reading next packet: {}", err));
            }
        };

        // ... (rest of the loop as before, using track_id) ...
        if packet.track_id() != track_id {
            continue;
        }
        // ... (decoder.decode(&packet) etc.) ...
        // ... (rest of the function)
        match decoder.decode(&packet) {
            Ok(decoded_packet_ref) => {
                let spec = *decoded_packet_ref.spec();
                let mut sample_buf = SampleBuffer::<f32>::new(
                    decoded_packet_ref.capacity() as u64,
                    spec,
                );
                sample_buf.copy_interleaved_ref(decoded_packet_ref);

                let mut mono_samples_this_packet: Vec<f32> = Vec::new();
                if spec.rate != target_sample_rate {
                    eprintln!(
                        "Warning: Audio file sample rate ({}) does not match target ({}). Resampling not yet implemented. Results may be incorrect.",
                        spec.rate, target_sample_rate
                    );
                }

                let samples_this_packet = sample_buf.samples();
                match spec.channels.count() {
                    1 => {
                        mono_samples_this_packet.extend_from_slice(samples_this_packet);
                    }
                    2 => {
                        for i in (0..samples_this_packet.len()).step_by(2) {
                            mono_samples_this_packet.push((samples_this_packet[i] + samples_this_packet[i+1]) / 2.0);
                        }
                    }
                    _ => {
                        for i in (0..samples_this_packet.len()).step_by(spec.channels.count()) {
                            mono_samples_this_packet.push(samples_this_packet[i]);
                        }
                        eprintln!("Warning: Audio has {} channels. Taking first channel.", spec.channels.count());
                    }
                }
                decoded_samples.extend(mono_samples_this_packet);
            }
            Err(SymphoniaError::DecodeError(err)) => {
                eprintln!("Decode error: {}", err);
            }
            Err(err) => {
                return Err(format!("Fatal decoding error: {}", err));
            }
        }
    }
    Ok(decoded_samples)
}