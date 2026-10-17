//! Audio analysis shared by Stelly and stelly-mix: no database, and no
//! network but for fetching CLAP's weights, just samples in and numbers out.

#[cfg(feature = "clap")]
pub mod clap;
pub mod decode;
pub mod descriptors;
#[cfg(feature = "clap")]
pub mod models;
