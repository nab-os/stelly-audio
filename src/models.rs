//! CLAP's weights: where they come from and what they hash to, so that a
//! track embedded by Stelly, a phrase embedded by stelly-mix and a
//! description all land in one space.
//!
//! Xenova's ONNX export of `laion/clap-htsat-unfused`, pinned to one revision
//! and checked by hash, so what runs is what was validated against the torch
//! model. `Download` checks whatever the bytes come through; the `fetch`
//! feature adds a blocking fetch for callers with no HTTP client of their own.

use crate::clap::{CLAP_AUDIO, CLAP_TEXT, CLAP_TOKENIZER};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const REPO: &str = "https://huggingface.co/Xenova/clap-htsat-unfused/resolve";
pub const REVISION: &str = "c28f2883575e590e04d3146ff0713c2448d691ba";

/// What the audio tower needs.
pub const AUDIO: &[&str] = &[CLAP_AUDIO];
/// What the text tower needs.
pub const TEXT: &[&str] = &[CLAP_TEXT, CLAP_TOKENIZER];

pub struct Weights {
    /// The file name in a model directory.
    pub name: &'static str,
    /// The path in the repository.
    pub remote: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

impl Weights {
    pub fn url(&self) -> String {
        format!("{REPO}/{REVISION}/{}", self.remote)
    }
}

pub const WEIGHTS: [Weights; 3] = [
    Weights {
        name: CLAP_AUDIO,
        remote: "onnx/audio_model.onnx",
        bytes: 117_528_416,
        sha256: "a1c2b43c44f71e0fa841a4b86700886c199bf87699ea45632c4d831bc6c88957",
    },
    Weights {
        name: CLAP_TEXT,
        remote: "onnx/text_model.onnx",
        bytes: 501_513_769,
        sha256: "6df7c51f2d78e236a03f2b816e613d538c815639d13644fe7c2124f439da9648",
    },
    Weights {
        name: CLAP_TOKENIZER,
        remote: "tokenizer.json",
        bytes: 2_108_774,
        sha256: "dc239041d98de27ffc3975473a1a23e3db4c937b23c138c38bbc66588bd247e5",
    },
];

/// Whether a file is already in place. Size only: hashing 500MB on every
/// start would be slower than the thing it guards.
pub fn present(model_dir: &Path, name: &str) -> bool {
    let expected = WEIGHTS.iter().find(|w| w.name == name).map(|w| w.bytes);
    std::fs::metadata(model_dir.join(name)).is_ok_and(|m| Some(m.len()) == expected)
}

/// Those of `names` not yet in place.
pub fn missing<'a>(model_dir: &'a Path, names: &'a [&str]) -> impl Iterator<Item = &'static Weights> + 'a {
    WEIGHTS
        .iter()
        .filter(move |w| names.contains(&w.name) && !present(model_dir, w.name))
}

/// One file on its way in: written beside its target, hashed as it comes,
/// and moved into place only if the hash is right. Dropped unfinished, a
/// cancelled fetch say, it leaves nothing behind.
pub struct Download {
    weights: &'static Weights,
    file: std::fs::File,
    hasher: Sha256,
    received: u64,
    partial: PathBuf,
    target: PathBuf,
    done: bool,
}

impl Download {
    pub fn create(model_dir: &Path, weights: &'static Weights) -> Result<Self> {
        std::fs::create_dir_all(model_dir).with_context(|| format!("creating {}", model_dir.display()))?;
        let target = model_dir.join(weights.name);
        let partial = target.with_extension("part");
        let file = std::fs::File::create(&partial).with_context(|| format!("creating {}", partial.display()))?;
        Ok(Self {
            weights,
            file,
            hasher: Sha256::new(),
            received: 0,
            partial,
            target,
            done: false,
        })
    }

    pub fn write(&mut self, chunk: &[u8]) -> Result<()> {
        self.received += chunk.len() as u64;
        if self.received > self.weights.bytes {
            bail!("{} is longer than its {} bytes", self.weights.name, self.weights.bytes);
        }
        self.hasher.update(chunk);
        self.file.write_all(chunk)?;
        Ok(())
    }

    /// How much of the file has come, 0..1.
    pub fn fraction(&self) -> f64 {
        self.received as f64 / self.weights.bytes as f64
    }

    pub fn finish(mut self) -> Result<()> {
        self.file.flush()?;
        let digest: String = std::mem::take(&mut self.hasher)
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if digest != self.weights.sha256 {
            bail!("{} arrived with sha256 {digest}, expected {}", self.weights.name, self.weights.sha256);
        }
        std::fs::rename(&self.partial, &self.target)?;
        self.done = true;
        Ok(())
    }
}

impl Drop for Download {
    fn drop(&mut self) {
        if !self.done {
            let _ = std::fs::remove_file(&self.partial);
        }
    }
}

/// Fetch whichever of `names` are missing, blocking, telling `fetching`
/// about each before it starts.
#[cfg(feature = "fetch")]
pub fn ensure(model_dir: &Path, names: &[&str], mut fetching: impl FnMut(&Weights)) -> Result<()> {
    use std::io::Read;

    for weights in missing(model_dir, names) {
        fetching(weights);
        let fetch = || -> Result<()> {
            let mut response = ureq::get(&weights.url()).call()?;
            let mut body = response.body_mut().with_config().limit(weights.bytes + 1).reader();
            let mut download = Download::create(model_dir, weights)?;
            let mut buffer = vec![0u8; 1 << 20];
            loop {
                let n = body.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                download.write(&buffer[..n])?;
            }
            download.finish()
        };
        fetch().with_context(|| format!("fetching {}", weights.name))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("stelly-audio-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_wrong_download_leaves_nothing_behind() {
        let dir = scratch("wrong");
        let mut download = Download::create(&dir, &WEIGHTS[2]).unwrap();
        download.write(b"not a tokenizer").unwrap();
        assert!(download.finish().is_err());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        assert_eq!(missing(&dir, TEXT).count(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_abandoned_download_leaves_nothing_behind() {
        let dir = scratch("abandoned");
        let mut download = Download::create(&dir, &WEIGHTS[0]).unwrap();
        download.write(&[0; 1024]).unwrap();
        drop(download);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
