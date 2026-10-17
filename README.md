# stelly-audio

Audio analysis with no network and no database: samples in, numbers out.
Shared by [Stelly](https://github.com/nab-os) and
[stelly-mix](https://github.com/nab-os/stelly-mix).

- **Decoding** to mono with symphonia: MP3 always, every codec and container
  it has with the `all-codecs` feature.
- **Resampling**, windowed-sinc, polyphase when the rates share a short period.
- **Descriptors**: tempo, onset rate and the onset envelope itself, key and
  its strength, EBU R128 loudness and range, dynamic complexity, spectral
  centroid, rolloff and flatness, zero-crossing rate.
- **CLAP's audio tower** (`laion/clap-htsat-unfused`, Xenova's ONNX export),
  with a mel front end ported from transformers' `ClapFeatureExtractor` and
  checked against it: a mean-pooled, L2-normalised 512-d embedding. Behind the
  default `clap` feature; the weights are not bundled, load them from a model
  directory.

```toml
[dependencies]
stelly-audio = { git = "https://github.com/nab-os/stelly-audio", features = ["all-codecs"] }
```

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
