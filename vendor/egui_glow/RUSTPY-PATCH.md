# RustPy egui_glow patch provenance

Based on published egui_glow 0.36.2, upstream egui commit
49682f8baa058bf49e011035cfbd6e825f88a5ef, crates/egui_glow.
License: MIT OR Apache-2.0; both upstream license texts are retained.

Compared with the published crate, RustPy exports `text_rendering` from
`src/lib.rs` and changes `src/painter.rs` and `src/shader/fragment.glsl` to
implement grayscale, monochrome and RGB/BGR text coverage passes.
The manifest and lockfile are maintained locally;
examples, upstream README and Cargo packaging metadata were omitted.
Modified upstream source files carry a RustPy modification notice.
