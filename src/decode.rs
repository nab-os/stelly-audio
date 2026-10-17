//! Audio in: decoding to mono or stereo and resampling, before either
//! analysis reads it.
//!
//! MP3 is always there, it is what Stelly analyses. Other formats come with
//! the `all-codecs` feature, for local files, Opus among them.
//!
//! symphonia demuxes Ogg Opus but has no Opus decoder, and Opus is what most
//! audio downloaded from the web arrives as. So with the `opus` feature the
//! packets symphonia reads go through `rusty-opus`, pure Rust like the rest.
//! The encoder's pre-skip comes as the track's delay and the final padding as
//! the last packet's end trim; dropping both matters to anything beat-aligned,
//! where a beat 6.5ms late is a grid 6.5ms off.

use anyhow::{bail, Context, Result};
use std::path::Path;
use symphonia::core::io::MediaSource;


/// Mono samples in [-1, 1] and their rate.
#[derive(Debug, Clone)]
pub struct Pcm {
    pub samples: Vec<f32>,
    pub rate: u32,
}

impl Pcm {
    pub fn seconds(&self) -> f64 {
        self.samples.len() as f64 / self.rate as f64
    }
}

/// Two channels, for listening; the analysis only ever needs one.
#[derive(Debug, Clone)]
pub struct Stereo {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub rate: u32,
}

impl Stereo {
    pub fn len(&self) -> usize {
        self.left.len()
    }

    pub fn is_empty(&self) -> bool {
        self.left.is_empty()
    }

    pub fn seconds(&self) -> f64 {
        self.left.len() as f64 / self.rate as f64
    }

    /// Mono doubled, more channels than two cut to the first two.
    fn push(&mut self, interleaved: &[f32], channels: usize) {
        for frame in interleaved.chunks_exact(channels) {
            self.left.push(frame[0]);
            self.right.push(frame[if channels > 1 { 1 } else { 0 }]);
        }
    }
}

// ----------------------------------------------------------------- decoding

/// Decode MP3 bytes to mono. Frames that fail, the first one usually does,
/// since its bit reservoir points into audio we did not fetch, are skipped.
pub fn decode_mp3(bytes: Vec<u8>) -> Result<Pcm> {
    mono(Box::new(std::io::Cursor::new(bytes)), Some("mp3")).context("not an MP3 stream")
}

/// Decode a whole file to mono, in any format the enabled codecs cover.
pub fn decode_file(path: &Path) -> Result<Pcm> {
    #[cfg(feature = "opus")]
    if is_opus(path) {
        return opus::decode(path, 1).map(|samples| Pcm { samples, rate: opus::RATE });
    }
    let decoded = open(path).and_then(|file| mono(file, extension(path)));
    // An .ogg can hold Opus too.
    #[cfg(feature = "opus")]
    let decoded = decoded.or_else(|err| {
        opus::decode(path, 1)
            .map(|samples| Pcm { samples, rate: opus::RATE })
            .map_err(|_| err)
    });
    decoded.with_context(|| format!("decoding {}", path.display()))
}

/// Decode a whole file to stereo: mono doubled, more channels than two cut
/// to the first two.
pub fn decode_file_stereo(path: &Path) -> Result<Stereo> {
    #[cfg(feature = "opus")]
    if is_opus(path) {
        return opus::decode(path, 2).map(opus::stereo);
    }
    let decoded = open(path).and_then(|file| stereo(file, extension(path)));
    #[cfg(feature = "opus")]
    let decoded = decoded.or_else(|err| opus::decode(path, 2).map(opus::stereo).map_err(|_| err));
    decoded.with_context(|| format!("decoding {}", path.display()))
}

fn open(path: &Path) -> Result<Box<dyn MediaSource>> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    Ok(Box::new(file))
}

fn extension(path: &Path) -> Option<&str> {
    path.extension().and_then(|e| e.to_str())
}

#[cfg(feature = "opus")]
fn is_opus(path: &Path) -> bool {
    extension(path).is_some_and(|e| e.eq_ignore_ascii_case("opus"))
}

fn mono(source: Box<dyn MediaSource>, extension: Option<&str>) -> Result<Pcm> {
    let mut samples = Vec::new();
    let rate = decode(source, extension, |interleaved, channels| {
        let scale = 1.0 / channels as f32;
        samples.extend(
            interleaved
                .chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() * scale),
        );
    })?;
    Ok(Pcm { samples, rate })
}

fn stereo(source: Box<dyn MediaSource>, extension: Option<&str>) -> Result<Stereo> {
    let mut stereo = Stereo { left: Vec::new(), right: Vec::new(), rate: 0 };
    stereo.rate = decode(source, extension, |interleaved, channels| stereo.push(interleaved, channels))?;
    Ok(stereo)
}

/// Every decoded buffer of the default audio track to `each`, interleaved
/// with its channel count; the rate, once something decoded.
fn decode(
    source: Box<dyn MediaSource>,
    extension: Option<&str>,
    mut each: impl FnMut(&[f32], usize),
) -> Result<u32> {
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let stream = MediaSourceStream::new(source, Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = extension {
        hint.with_extension(extension);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, stream, FormatOptions::default(), MetadataOptions::default())
        .context("unrecognised audio format")?;

    let track = format
        .default_track(TrackType::Audio)
        .context("no audio track")?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .context("no audio codec parameters")?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .context("no decoder for this stream")?;

    let mut interleaved: Vec<f32> = Vec::new();
    let mut rate = 0u32;
    let mut decoded = false;

    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            // A truncated final frame is the normal end of a byte range.
            Err(Error::IoError(_)) => break,
            Err(err) => return Err(err).context("demuxing"),
        };
        if packet.track_id != track_id {
            continue;
        }
        let buffer = match decoder.decode(&packet) {
            Ok(buffer) => buffer,
            Err(Error::DecodeError(_)) | Err(Error::IoError(_)) => continue,
            Err(err) => return Err(err).context("decoding"),
        };

        rate = buffer.spec().rate();
        let channels = buffer.spec().channels().count().max(1);
        interleaved.resize(buffer.samples_interleaved(), 0.0);
        buffer.copy_to_slice_interleaved(&mut interleaved);
        decoded |= interleaved.len() >= channels;
        each(&interleaved, channels);
    }

    if !decoded || rate == 0 {
        bail!("decoded no audio");
    }
    Ok(rate)
}

#[cfg(feature = "opus")]
mod opus {
    use super::{open, Stereo};
    use anyhow::{bail, Context, Result};
    use std::path::Path;

    /// Opus always decodes at 48kHz; its timestamps count at that rate too.
    pub const RATE: u32 = 48_000;
    /// The longest Opus packet, 120ms, in samples.
    const MAX_PACKET: usize = 5_760;

    pub fn stereo(interleaved: Vec<f32>) -> Stereo {
        let mut stereo = Stereo { left: Vec::new(), right: Vec::new(), rate: RATE };
        stereo.push(&interleaved, 2);
        stereo
    }

    /// Interleaved samples at 48kHz in `channels`, 1 or 2; the decoder mixes
    /// down or up itself, a plain mean for mono.
    pub fn decode(path: &Path, channels: usize) -> Result<Vec<f32>> {
        use symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS;
        use symphonia::core::errors::Error;
        use symphonia::core::formats::probe::Hint;
        use symphonia::core::formats::{FormatOptions, TrackType};
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;

        let stream = MediaSourceStream::new(open(path)?, Default::default());
        let mut hint = Hint::new();
        hint.with_extension("ogg");
        let mut format = symphonia::default::get_probe()
            .probe(&hint, stream, FormatOptions::default(), MetadataOptions::default())
            .with_context(|| format!("{} is not an Ogg stream", path.display()))?;

        let track = format.default_track(TrackType::Audio).context("no audio track")?;
        let is_opus = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .is_some_and(|p| p.codec == CODEC_ID_OPUS);
        if !is_opus {
            bail!("{} holds no Opus", path.display());
        }
        let track_id = track.id;
        let mut pre_skip = track.delay.unwrap_or(0) as usize;

        let mut decoder =
            rusty_opus::OpusDecoder::new(RATE as i32, channels).map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let mut frame = vec![0f32; MAX_PACKET * channels];
        let mut samples = Vec::new();
        loop {
            let packet = match format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => break,
                Err(Error::IoError(_)) => break,
                Err(err) => return Err(err).context("demuxing"),
            };
            if packet.track_id != track_id {
                continue;
            }
            let decoded = match decoder.decode(&packet.data, MAX_PACKET, &mut frame) {
                Ok(n) => n,
                // A corrupt packet: conceal it rather than shift everything after.
                Err(_) => decoder.decode(&[], MAX_PACKET, &mut frame).unwrap_or(0),
            };
            let end = decoded.saturating_sub(packet.trim_end.get() as usize);
            let trim = (packet.trim_start.get() as usize).min(end);
            // The pre-skip can outlast the first packet.
            let skip = pre_skip.min(end - trim);
            pre_skip -= skip;
            samples.extend_from_slice(&frame[(trim + skip) * channels..end * channels]);
        }

        if samples.is_empty() {
            bail!("decoded no audio from {}", path.display());
        }
        Ok(samples)
    }
}

// --------------------------------------------------------------- resampling

/// Windowed-sinc resampling, polyphase when the two rates share a small
/// period, 44.1k to 48k is 147 in, 160 out.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    const HALF_TAPS: i64 = 32;

    let divisor = gcd(from as u64, to as u64);
    let (up, down) = (to as u64 / divisor, from as u64 / divisor);
    // Low-pass at the lower of the two Nyquists, a little under it.
    let cutoff = (to as f64 / from as f64).min(1.0) * 0.95;
    let half = (HALF_TAPS as f64 / cutoff.min(1.0)).ceil() as i64;

    let kernel = |x: f64| -> f64 {
        if x.abs() >= half as f64 {
            return 0.0;
        }
        let sinc = if x == 0.0 {
            1.0
        } else {
            let px = std::f64::consts::PI * cutoff * x;
            px.sin() / px
        };
        // Blackman over the kernel's support.
        let n = (x / half as f64 + 1.0) / 2.0;
        let window = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * n).cos()
            + 0.08 * (4.0 * std::f64::consts::PI * n).cos();
        cutoff * sinc * window
    };

    let out_len = (input.len() as u64 * up / down) as usize;
    let taps = (2 * half) as usize;

    // One row of taps per phase, when there are few enough phases to tabulate.
    let table: Option<Vec<f32>> = (up <= 4096).then(|| {
        let mut table = vec![0f32; up as usize * taps];
        for phase in 0..up as usize {
            let frac = phase as f64 / up as f64;
            for (j, slot) in table[phase * taps..(phase + 1) * taps].iter_mut().enumerate() {
                let k = j as i64 - half + 1;
                *slot = kernel(k as f64 - frac) as f32;
            }
        }
        table
    });

    let mut out = Vec::with_capacity(out_len);
    for n in 0..out_len as u64 {
        let position = n * down;
        let base = (position / up) as i64;
        let phase = (position % up) as usize;
        let mut acc = 0f32;
        for j in 0..taps {
            let index = base + j as i64 - half + 1;
            if index < 0 || index >= input.len() as i64 {
                continue;
            }
            let weight = match &table {
                Some(table) => table[phase * taps + j],
                None => kernel((j as i64 - half + 1) as f64 - phase as f64 / up as f64) as f32,
            };
            acc += weight * input[index as usize];
        }
        out.push(acc);
    }
    out
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// Clicks at 0.5s, 1.0s and 1.5s, encoded by libopus 1.5 through ffmpeg:
    /// `ffmpeg -i clicks.wav -c:a libopus -b:a 64k clicks.opus`. Its pre-skip
    /// is the usual 312 samples; left in, every click would land 6.5ms late.
    #[cfg(feature = "opus")]
    #[test]
    fn opus_clicks_land_where_they_were_put() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/clicks.opus");
        let pcm = decode_file(&path).unwrap();
        assert_eq!(pcm.rate, opus::RATE);
        assert!((pcm.seconds() - 2.0).abs() < 0.001, "{}s", pcm.seconds());

        for at in [0.5, 1.0, 1.5] {
            let from = ((at - 0.1) * opus::RATE as f64) as usize;
            let window = &pcm.samples[from..from + (0.2 * opus::RATE as f64) as usize];
            let peak = window.iter().map(|v| v.abs()).fold(0f32, f32::max);
            let onset = window.iter().position(|v| v.abs() > 0.5 * peak).unwrap();
            let found = (from + onset) as f64 / opus::RATE as f64;
            assert!((found - at).abs() < 0.001, "click at {at}s decoded at {found:.4}s");
        }

        // Stereo on the same timeline, so beats found in mono land in a render.
        let stereo = decode_file_stereo(&path).unwrap();
        assert_eq!((stereo.len(), stereo.rate), (pcm.samples.len(), pcm.rate));
    }

    #[test]
    fn resampling_keeps_a_tone_and_its_length() {
        let from = 44_100;
        let tone: Vec<f32> = (0..from)
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / from as f32).sin())
            .collect();
        let out = resample(&tone, from, 48_000);
        assert_eq!(out.len(), 48_000);
        // Away from the edges it should still be a unit sine at 1kHz.
        for (n, v) in out.iter().enumerate().skip(1000).take(1000) {
            let expected = (2.0 * std::f32::consts::PI * 1000.0 * n as f32 / 48_000.0).sin();
            assert!((v - expected).abs() < 2e-3, "sample {n}: {v} vs {expected}");
        }
    }
}
