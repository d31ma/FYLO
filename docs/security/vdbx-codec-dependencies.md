# Dependency review: `.vdbx` record codecs

ADR 0004 review for the three crates ADR 0016 section 8 adds. Samuel chose the
codec split on 2026-10-29 from spike 4 (`vdbx/spike-4-codec.md` in the project
files). All three are used only by `crates/fylo-storage-native/src/container_codec.rs`,
which `segments/container_frames.rs` calls for every record frame of a `.vdbx`
collection. A directory collection's frames never reach them.

| | `zstd` 0.14.3 | `ruzstd` 0.9.1 | `lz4_flex` 0.14.0 |
| --- | --- | --- | --- |
| Purpose | Native zstd frames (level 3 writes, level 19 sealed packs) and dictionary training | Browser decoding of native zstd frames | LZ4 frames the browser writes, read everywhere |
| Targets | Native only (`cfg(not(target_family = "wasm"))`) | Browser only (`wasm32-unknown-unknown`); native tests only | Native and browser; not WASI, which reads no `.vdbx` segments |
| Features | `zdict_builder` only; defaults off (no legacy, no arrays, no multithread) | `std` only; defaults off (no `hash`, so no `twox-hash`) | `std`, `safe-encode`, `safe-decode`; defaults off |
| License | BSD-3-Clause (crate and bundled libzstd 1.5.7) | MIT | MIT |
| New transitive crates | `zstd-safe`, `zstd-sys`, `jobserver`, `pkg-config` (`cc` was already in the graph) | none | none |
| Unsafe and native code | `zstd-sys` builds and links the C library with `cc`; `zstd-safe` wraps it | Pure Rust | Pure Rust; the `safe-*` features select the implementations without `unsafe` |
| Build script and reproducibility | `zstd-sys`'s `build.rs` compiles the bundled libzstd 1.5.7 sources with the target's C compiler and hides their symbols; it links a system libzstd only when `ZSTD_SYS_USE_PKG_CONFIG` is set, which FYLO's builds never set. No network access. Output depends on the C compiler version, as any `cc` build does | None | None |
| Secrets | None: records are compressed before they are sealed, and the key never reaches a codec | None | None |
| Untrusted input | Record bodies from disk. `container_codec` reads the declared content size first, refuses it above the caller's limit, and requires exactly that many bytes out | Same check, plus decoding in 128 KiB steps that stop at the declared size, and a window capped at it | The 4-byte size prefix is checked against the limit before any allocation |
| Binary impact | libzstd adds about 433 KB as WebAssembly, which is why it stays native | About +16 KB gzip in `fylo-browser.wasm` | About +6 KB gzip |
| Maintenance | Widely used; tracks upstream zstd releases | Active; the only maintained pure-Rust zstd decoder | Active |
| Removal path | `lz4_flex` everywhere, at 31–53% larger files (spike 4) | Drop browser reads of zstd frames; native compaction would then have to write LZ4 for files meant for the browser | `zstd` frames only, with the browser read-only |

Advisories: none known for these versions at the time of review. `cargo-deny`
licenses, bans and sources still apply in CI.
