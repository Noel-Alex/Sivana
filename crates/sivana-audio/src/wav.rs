//! Minimal WAV reader/writer (RIFF/WAVE, PCM16 + float32).
//!
//! Enough for benchmark fixtures and degraded-query corpora; full codec
//! decoding stays in `sivana-audio`'s future symphonia-based sibling or the
//! legacy loader. Deterministic output: `write_wav` always emits the same
//! bytes for the same samples.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

/// Mono audio plus its sample rate. Channels >1 are averaged to mono.
#[derive(Debug, Clone, PartialEq)]
pub struct WavData {
    pub sample_rate: u32,
    pub samples: Vec<f32>,
}

#[derive(Debug)]
pub enum WavError {
    Io(std::io::Error),
    NotRiff,
    NotWave,
    MissingChunk(&'static str),
    UnsupportedFormat(u16),
    UnsupportedBitsPerSample(u16),
    UnsupportedChannelCount(u16),
    Truncated(&'static str),
}

impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::NotRiff => write!(f, "not a RIFF file"),
            Self::NotWave => write!(f, "not a WAVE file"),
            Self::MissingChunk(c) => write!(f, "missing {c} chunk"),
            Self::UnsupportedFormat(v) => write!(f, "unsupported format tag {v}"),
            Self::UnsupportedBitsPerSample(v) => write!(f, "unsupported bits per sample {v}"),
            Self::UnsupportedChannelCount(v) => write!(f, "unsupported channel count {v}"),
            Self::Truncated(what) => write!(f, "truncated file: {what}"),
        }
    }
}

impl std::error::Error for WavError {}

impl From<std::io::Error> for WavError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

fn read_u16(r: &mut impl Read) -> Result<u16, WavError> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}

fn read_u32(r: &mut impl Read) -> Result<u32, WavError> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

/// Read a WAV file, converting to mono f32 in `[-1, 1]`.
pub fn read_wav(path: &Path) -> Result<WavData, WavError> {
    let file = File::open(path)?;
    let mut r = BufReader::new(file);

    let mut riff = [0u8; 4];
    r.read_exact(&mut riff)?;
    if &riff != b"RIFF" {
        return Err(WavError::NotRiff);
    }
    let _riff_size = read_u32(&mut r)?;
    let mut wave = [0u8; 4];
    r.read_exact(&mut wave)?;
    if &wave != b"WAVE" {
        return Err(WavError::NotWave);
    }

    let mut format_tag: Option<u16> = None;
    let mut channels: Option<u16> = None;
    let mut sample_rate: Option<u32> = None;
    let mut bits: Option<u16> = None;
    let mut data: Option<Vec<u8>> = None;

    // Walk chunks; tolerate unknown ones.
    loop {
        let mut chunk_id = [0u8; 4];
        match r.read_exact(&mut chunk_id) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        let size = read_u32(&mut r)? as usize;

        match &chunk_id {
            b"fmt " => {
                format_tag = Some(read_u16(&mut r)?);
                channels = Some(read_u16(&mut r)?);
                sample_rate = Some(read_u32(&mut r)?);
                let _byte_rate = read_u32(&mut r)?;
                let _block_align = read_u16(&mut r)?;
                bits = Some(read_u16(&mut r)?);
                // Skip any extension bytes.
                if size > 16 {
                    let ext = size - 16;
                    let mut sink = vec![0u8; ext];
                    r.read_exact(&mut sink)?;
                }
            }
            b"data" => {
                let mut buf = vec![0u8; size];
                r.read_exact(&mut buf)?;
                data = Some(buf);
            }
            _ => {
                let mut sink = vec![0u8; size];
                r.read_exact(&mut sink)?;
            }
        }
    }

    let format_tag = format_tag.ok_or(WavError::MissingChunk("fmt "))?;
    let channels = channels.ok_or(WavError::MissingChunk("fmt "))?;
    let sample_rate = sample_rate.ok_or(WavError::MissingChunk("fmt "))?;
    let bits = bits.ok_or(WavError::MissingChunk("fmt "))?;
    let data = data.ok_or(WavError::MissingChunk("data"))?;

    if !matches!(format_tag, 1 | 3) {
        return Err(WavError::UnsupportedFormat(format_tag));
    }
    let samples_i = match (format_tag, bits) {
        (1, 16) => {
            if data.len() % 2 != 0 {
                return Err(WavError::Truncated("odd PCM16 byte count"));
            }
            decode_pcm16(&data)
        }
        (3, 32) => {
            if data.len() % 4 != 0 {
                return Err(WavError::Truncated("odd f32 byte count"));
            }
            decode_f32(&data)
        }
        (_, b) => return Err(WavError::UnsupportedBitsPerSample(b)),
    };

    match channels {
        1 => Ok(WavData { sample_rate, samples: samples_i }),
        2 => {
            let mono: Vec<f32> = samples_i
                .chunks_exact(2)
                .map(|c| (c[0] + c[1]) / 2.0)
                .collect();
            Ok(WavData { sample_rate, samples: mono })
        }
        n => Err(WavError::UnsupportedChannelCount(n)),
    }
}

fn decode_pcm16(data: &[u8]) -> Vec<f32> {
    data.chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect()
}

fn decode_f32(data: &[u8]) -> Vec<f32> {
    data.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Write mono f32 samples as 16-bit PCM WAV. Values are clamped to [-1, 1].
pub fn write_wav(path: &Path, sample_rate: u32, samples: &[f32]) -> Result<(), WavError> {
    let file = File::create(path)?;
    let mut w = BufWriter::new(file);

    let data_len = samples.len() * 2;
    let riff_len = 36 + data_len as u32;

    w.write_all(b"RIFF")?;
    w.write_all(&riff_len.to_le_bytes())?;
    w.write_all(b"WAVE")?;

    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes())?; // fmt chunk size
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&1u16.to_le_bytes())?; // mono
    w.write_all(&sample_rate.to_le_bytes())?;
    let byte_rate = sample_rate * 2;
    w.write_all(&byte_rate.to_le_bytes())?;
    w.write_all(&2u16.to_le_bytes())?; // block align
    w.write_all(&16u16.to_le_bytes())?; // bits

    w.write_all(b"data")?;
    w.write_all(&(data_len as u32).to_le_bytes())?;
    for chunk in encode_pcm16(samples).chunks(8192) {
        w.write_all(chunk)?;
    }
    w.flush()?;
    Ok(())
}

fn encode_pcm16(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        let q = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        out.extend_from_slice(&q.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_pcm16() {
        let dir = std::env::temp_dir().join("sivana-wav-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rt.wav");

        let sr = 16_000u32;
        let samples: Vec<f32> = (0..4000)
            .map(|i| (i as f32 / sr as f32 * 440.0 * std::f32::consts::TAU).sin() * 0.5)
            .collect();
        write_wav(&path, sr, &samples).unwrap();

        let back = read_wav(&path).unwrap();
        assert_eq!(back.sample_rate, sr);
        assert_eq!(back.samples.len(), samples.len());
        // Quantization to 16-bit bounds reconstruction error.
        for (a, b) in samples.iter().zip(back.samples.iter()) {
            assert!((a - b).abs() < 1e-3);
        }
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn written_bytes_are_deterministic() {
        let samples: Vec<f32> = (0..1000).map(|i| ((i * 7) % 101) as f32 / 101.0 - 0.5).collect();
        let a = encode_pcm16(&samples);
        let b = encode_pcm16(&samples);
        assert_eq!(a, b);
    }

    #[test]
    fn stereo_is_averaged_to_mono() {
        // Hand-build a tiny stereo PCM16 wav.
        let left: i16 = 16384; // 0.5
        let right: i16 = -16384; // -0.5
        let mut data = Vec::new();
        for v in [left, right] {
            data.extend_from_slice(&v.to_le_bytes());
        }
        let mut wav: Vec<u8> = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&2u16.to_le_bytes()); // stereo
        wav.extend_from_slice(&8000u32.to_le_bytes());
        wav.extend_from_slice(&32000u32.to_le_bytes()); // byte rate
        wav.extend_from_slice(&4u16.to_le_bytes()); // block align
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);

        let parsed = parse_memory(&wav);
        assert_eq!(parsed.sample_rate, 8000);
        assert_eq!(parsed.samples.len(), 1);
        assert!(parsed.samples[0].abs() < 1e-6); // (+0.5 + -0.5)/2
    }

    fn parse_memory(bytes: &[u8]) -> WavData {
        let tmp = std::env::temp_dir().join("sivana-mem.wav");
        std::fs::write(&tmp, bytes).unwrap();
        read_wav(&tmp).unwrap()
    }

    #[test]
    fn rejects_non_riff() {
        let tmp = std::env::temp_dir().join("sivana-bad.wav");
        std::fs::write(&tmp, b"NOPE1234").unwrap();
        assert!(matches!(read_wav(&tmp), Err(WavError::NotRiff)));
        std::fs::remove_file(&tmp).ok();
    }
}
