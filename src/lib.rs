//! Audio analysis shared by Stelly and stelly-mix: no network, no
//! database, just samples in and numbers out.

#[cfg(feature = "clap")]
pub mod clap;
pub mod decode;
pub mod descriptors;
