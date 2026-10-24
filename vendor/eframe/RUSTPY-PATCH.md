# RustPy native viewport handle patch

This is the published eframe 0.36.2 source, with its manifest and icon assets.
The upstream revision is recorded in .cargo_vcs_info.json. No build products or
dependency cache are copied. Upstream license: MIT OR Apache-2.0.

RustPy adds a weak native-window registry keyed by the actual egui ViewportId.
The root integration and both native render backends register owned windows.
Embedded native webviews use the matching viewport window, including floating
tool windows, rather than attaching to the root Frame window. Weak references
do not keep closed windows alive. No desktop-wide window lookup is performed.

Local delta: src/native_window.rs; one module export in src/lib.rs; registration
in native/epi_integration.rs, native/glow_integration.rs, and
native/wgpu_integration.rs. Retain these hooks when updating upstream.
Native acceptance is deferred with the rest of RustPy feature verification.
