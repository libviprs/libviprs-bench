//! `probe-emulation`: run the emulation probe and print what it saw.
//!
//! Two callers, and the second is the reason this file exists at all.
//!
//! `cargo run --bin probe-emulation` is the convenient one. The one that
//! matters is `tools/probe-emulation.sh`, which compiles *this file* with bare
//! `rustc` inside a `rust:1.89-slim-bookworm` container under
//! `--platform linux/amd64` and again under `--platform linux/arm64`, and
//! asserts opposite answers on one machine. That control is the whole evidence
//! for the probe, because a probe hard-wired to `false` passes every `#[test]`
//! anyone could write for it.
//!
//! So this file pulls `src/emulation.rs` in by `#[path]` rather than by
//! `use libviprs_bench::emulation`, and neither file touches a crate outside
//! `std`. Through cargo the module is compiled twice, once into the library and
//! once into this binary; that is a few hundred lines duplicated in the
//! artefact and it buys a probe whose control finishes in seconds instead of
//! compiling `libviprs` under an emulated compiler.
//!
//! Exit status is 0 whatever the verdict. The probe reporting "yes, emulated"
//! is a successful observation, not a failure, and it is the aggregator's job
//! to refuse the run.

#[path = "emulation.rs"]
mod emulation;

fn main() {
    let report = emulation::probe();
    println!("{}", report.to_json());
}
