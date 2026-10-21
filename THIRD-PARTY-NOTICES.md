# Third-party notices

Flick is licensed under AGPL-3.0. It uses the following third-party components. Re-verify every entry
before each release (see SPEC.md section 11).

| Component | Used in | License | Copyright |
|---|---|---|---|
| [Tauri](https://github.com/tauri-apps/tauri) | core | MIT OR Apache-2.0 | The Tauri Programme within The Commons Conservancy |
| [serde](https://github.com/serde-rs/serde), [serde_json](https://github.com/serde-rs/json) | core, workers | MIT OR Apache-2.0 | Erick Tryzelaar, David Tolnay |
| [anyhow](https://github.com/dtolnay/anyhow), [thiserror](https://github.com/dtolnay/thiserror) | core, workers | MIT OR Apache-2.0 | David Tolnay |

Transitive Rust dependencies can be listed with `cargo tree` and audited with
[`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) (`cargo deny check licenses`).
