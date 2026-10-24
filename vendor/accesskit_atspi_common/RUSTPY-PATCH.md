# RustPy AT-SPI re-added node patch

This is the published accesskit_atspi_common 0.18.1 source with its manifest
(upstream revision in .cargo_vcs_info.json). No build products or dependency
cache are copied. Upstream license: MIT OR Apache-2.0 (AccessKit authors).

`remove_node` emits `object:state-changed:defunct` (true). libatspi records that
state in each client's cached state set for the object path, and nothing clears
it when egui shows a widget with the same id again (a previously open editor
tab, the editor regaining focus after Settings closes, a re-opened tool window).
Orca then dropped every event of the re-added node as "defunct", so tab switches
and focus returning from a dialog were silent.

Local delta: `add_node` in src/adapter.rs also emits defunct = false. Retain it
when updating upstream unless AccessKit itself clears the state. Found with
qa/accessibility/orca_acceptance.sh.

Additional local changes in `src/node.rs` expose a combo box's selected value
as read-only AT-SPI Text for Orca. `Cargo.toml` / `Cargo.lock` are maintained
locally. Modified upstream source files carry RustPy modification notices.

The exact upstream LICENSE-MIT and LICENSE-APACHE files were retrieved from
https://github.com/AccessKit/accesskit/tree/f40dfc01a0c0e76de535969f82fb35e19513737d
on 2026-10-22. The upstream MIT text has no copyright preamble; it is retained
verbatim rather than assigning an invented copyright holder.
