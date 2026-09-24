// Build-time feature detection.
//
// * `arc_swap_nightly_tls`: set when compiling with a nightly compiler. The
//   `experimental-thread-local` feature needs the nightly-only
//   `#![feature(thread_local)]` attribute; on stable compilers we silently
//   fall back to the standard thread-local storage so that
//   `cargo test --all-features` keeps working everywhere.
// * `loom`: the crate switches its atomic types to loom's instrumented ones
//   when compiled with `--cfg loom` (see TESTING.md). The cfg is set through
//   RUSTFLAGS, we only declare it here so the compiler doesn't warn about it.
//
// The `cargo::` syntax is understood by cargo >= 1.77; older cargos only warn
// about the unknown key and otherwise ignore it, which is fine (the cfgs
// didn't exist back then either, so nothing was checked).

use std::env;
use std::process::Command;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(arc_swap_nightly_tls)");
    println!("cargo::rustc-check-cfg=cfg(loom)");

    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .unwrap_or_default();
    // Release versions look like "rustc 1.98.1 (abc 2026-09-01)", nightly and
    // dev builds contain "nightly" or "dev".
    if version.contains("nightly") || version.contains("-dev") {
        println!("cargo:rustc-cfg=arc_swap_nightly_tls");
    }
}
