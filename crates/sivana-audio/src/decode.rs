//! Native audio file decoding (PLAN §32).
//!
//! Decodes any Symphonia-supported container/codec (mp3, flac, wav, aac,
//! ogg/vorbis, ...) to mono f32 at the file's native sample rate.
//! Callers resample explicitly — never silently here.

/// Decode an in-memory file to mono f32 + native sample rate.
pub fn decode_mono(bytes: &[u8]) -> Result<(Vec<f32>, u32), String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let src = std::io::Cursor::new(bytes.to_vec());
    let mss = MediaSourceStream::new(Box::new(src), Default::default());
    let hint = Hint::new();
    // Container probing relies on the stream itself; extension is optional.

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probing failed: {e}"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("no decodable audio track")?
        .clone();
    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or("track has no sample rate")?;
    let channels = track
        .codec_params
        .channels
        .map(|c| c.count())
        .unwrap_or(1)
        .max(1);

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("decoder init: {e}"))?;

    let mut mono: Vec<f32> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(symphonia::core::errors::Error::ResetRequired) => {
                return Err("stream reset mid-file not supported".into());
            }
            Err(_) => break, // decode error: keep what we have
        };
        match decoder.decode(&packet) {
            Ok(decoded) => {
                if sample_buf.is_none() {
                    sample_buf = Some(SampleBuffer::<f32>::new(
                        decoded.capacity() as u64,
                        *decoded.spec(),
                    ));
                }
                let buf = sample_buf.as_mut().unwrap();
                buf.copy_interleaved_ref(decoded);
                let n = buf.samples().len() / channels.max(1);
                for i in 0..n {
                    let mut s = 0.0f32;
                    for c in 0..channels {
                        s += buf.samples()[i * channels + c];
                    }
                    mono.push(s / channels as f32);
                }
            }
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decode: {e}")),
        }
    }

    if mono.is_empty() {
        return Err("no audio decoded".into());
    }
    Ok((mono, sample_rate))
}
