# Third-party notices

Flick is licensed under AGPL-3.0. It uses the following third-party components. Re-verify every entry
before each release (see SPEC.md section 11).

| Component | Used in | License | Copyright |
|---|---|---|---|
| [Tauri](https://github.com/tauri-apps/tauri) | core | MIT OR Apache-2.0 | The Tauri Programme within The Commons Conservancy |
| [serde](https://github.com/serde-rs/serde), [serde_json](https://github.com/serde-rs/json) | core, workers | MIT OR Apache-2.0 | Erick Tryzelaar, David Tolnay |
| [lopdf](https://github.com/J-F-Liu/lopdf) | pdf-light and image-tools workers | MIT | Junfeng Liu and contributors |
| [image](https://github.com/image-rs/image) (jpeg, png, webp, bmp only), [png](https://github.com/image-rs/image-png), [image-webp](https://github.com/image-rs/image-webp), [fdeflate](https://github.com/image-rs/fdeflate), [byteorder-lite](https://github.com/image-rs/byteorder-lite) | image-tools worker | MIT OR Apache-2.0 (byteorder-lite: Unlicense OR MIT) | The image-rs Developers |
| [zune-jpeg](https://github.com/etemesi254/zune-image), zune-core | image-tools worker (JPG decoding) | MIT OR Apache-2.0 OR Zlib | Caleb Etemesi and contributors |
| [moxcms](https://github.com/awxkee/moxcms), [pxfm](https://github.com/awxkee/pxfm) | image-tools worker (colour profiles, via image) | BSD-3-Clause OR Apache-2.0 | Radzivon Bartoshyk |
| [flate2](https://github.com/rust-lang/flate2-rs), [miniz_oxide](https://github.com/Frommi/miniz_oxide), [adler2](https://github.com/oyvindln/adler2), [crc32fast](https://github.com/srijs/rust-crc32fast), [simd-adler32](https://github.com/mcountryman/simd-adler32) | workers (Deflate compression) | MIT OR Apache-2.0 (miniz_oxide also Zlib, adler2 also 0BSD; simd-adler32: MIT) | Alex Crichton, Josh Triplett, Frommi, oyvindln, Sam Rijs, Marvin Countryman and contributors |
| [bytemuck](https://github.com/Lokathor/bytemuck), [num-traits](https://github.com/rust-num/num-traits), [quick-error](https://github.com/tailhook/quick-error) | image-tools worker (via image) | MIT OR Apache-2.0 (bytemuck also Zlib) | Lokathor, The Rust Project Developers, Paul Colomiets |
| [windows-rs](https://github.com/microsoft/windows-rs) | core | MIT OR Apache-2.0 | Microsoft Corporation |
| [anyhow](https://github.com/dtolnay/anyhow), [thiserror](https://github.com/dtolnay/thiserror) | core, workers | MIT OR Apache-2.0 | David Tolnay |
| [toml](https://github.com/toml-rs/toml) | core (config, pack manifests) | MIT OR Apache-2.0 | The toml-rs developers |
| [semver](https://github.com/dtolnay/semver) | core (pack version checks) | MIT OR Apache-2.0 | David Tolnay |

Transitive Rust dependencies can be listed with `cargo tree` and audited with
[`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) (`cargo deny check licenses`).
