//! Is this binary's instruction stream being translated?
//!
//! `docs/pmtiles-benchmarks.md` publishes 480 lines of numbers and records no
//! platform at all. The run that produced them called itself "the amd64 Linux
//! container"; the machine it ran on is Apple Silicon with
//! `DOCKER_DEFAULT_PLATFORM=linux/amd64` and Docker Desktop's Rosetta enabled,
//! so those numbers were almost certainly instruction-translated. Nothing in
//! the artefact can settle it, and that silence is the defect. This module is
//! the observation the artefact was missing.
//!
//! # What it answers, and what it does not
//!
//! It answers "is *this process* running through a userspace instruction
//! translator" — Rosetta 2 or qemu-user. It deliberately does not answer "is
//! this a virtual machine": a whole-system emulator or a hypervisor presents a
//! native instruction set to the process, there is nothing in the process's own
//! address space to see, and pretending otherwise would be a probe that guesses.
//! A run inside a VM on matching architecture reports [`Emulated::No`], which is
//! the honest answer to the question the field is named after.
//!
//! # Why this file depends on nothing
//!
//! Only `std`, and it is pulled into [`crate::provenance`] and into the
//! standalone `probe-emulation` binary by the same `#[path]`. The standalone
//! binary exists so `tools/probe-emulation.sh` can compile the probe with bare
//! `rustc` under two Docker platforms in a few seconds each. Building it with
//! cargo would drag the entire `libviprs` path dependency through an emulated
//! compiler, which is minutes per side for a probe that is a handful of `/proc`
//! reads. A serde derive in here would cost that, so the JSON is hand-written.
//!
//! # The evidence ladder, and why a single source is not enough
//!
//! Measured on the machine this exists for (Apple M-series, Docker Desktop
//! 4.x, Rosetta on) by `tools/probe-emulation.sh`:
//!
//! | source | `--platform linux/amd64` | `--platform linux/arm64` |
//! |---|---|---|
//! | `/proc/self/maps` | carries `/run/rosetta/rosetta` | clean |
//! | `/proc/sys/fs/binfmt_misc` | present and **empty** | present and empty |
//! | `BENCH_DAEMON_ARCH` vs binary arch | `arm64` vs `x86_64` | `arm64` vs `aarch64` |
//!
//! So `binfmt_misc` is blind inside a Docker Desktop container — the directory
//! exists and lists nothing, which reads exactly like a host with no handlers
//! registered. `/proc/self/maps` is not blind and is the primary evidence:
//! Rosetta maps its translator into the address space of every process it
//! translates, and the path survives even though `/run/rosetta` is not visible
//! in the mount namespace. The runner-supplied daemon architecture is the
//! fallback for a host where `/proc` tells us nothing, and the report always
//! names which source produced the verdict so a reader never has to guess.

use std::fmt::Write as _;

/// The answer, with the third state spelled out rather than folded into
/// `false`.
///
/// `Unknown` exists because "I could not observe it" and "I observed that it is
/// not happening" are different claims, and the aggregator refuses both of the
/// first two. Collapsing `Unknown` into `No` is precisely the mistake that made
/// the published PMTiles numbers unfalsifiable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emulated {
    /// At least one source observed instruction translation.
    Yes,
    /// No source observed translation, and at least one source was in a
    /// position to have seen it.
    No,
    /// Nothing was in a position to observe anything either way.
    Unknown,
}

impl Emulated {
    /// The JSON spelling: `true`, `false`, or the string `"unknown"`.
    ///
    /// A mixed-type field is unusual and is what `SUITE-PLAN.md` §5.4 asks for.
    /// It reads correctly from JavaScript (`x === true` is the refusal, and
    /// `x !== false` is the stricter refusal the aggregator uses) without a
    /// reader having to know a sentinel string means "missing".
    pub fn as_json(self) -> &'static str {
        match self {
            Emulated::Yes => "true",
            Emulated::No => "false",
            Emulated::Unknown => "\"unknown\"",
        }
    }
}

/// What one source concluded on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceVerdict {
    /// This source saw translation.
    Emulated,
    /// This source was able to look and saw none.
    Native,
    /// This source could not see either way. Recorded, never counted.
    Inconclusive,
}

impl EvidenceVerdict {
    fn as_str(self) -> &'static str {
        match self {
            EvidenceVerdict::Emulated => "emulated",
            EvidenceVerdict::Native => "native",
            EvidenceVerdict::Inconclusive => "inconclusive",
        }
    }
}

/// One source's observation, kept verbatim so the document can say which
/// evidence it used rather than only what it concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    /// Stable source name: `proc-self-maps`, `binfmt-misc`, `daemon-arch`,
    /// `uname`.
    pub source: &'static str,
    /// What this source concluded alone.
    pub verdict: EvidenceVerdict,
    /// The raw observation, in words, including the reason for an
    /// `Inconclusive`.
    pub detail: String,
}

/// The whole probe result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmulationReport {
    /// The combined verdict.
    pub emulated: Emulated,
    /// Every source that was consulted, in a stable order, including the ones
    /// that saw nothing.
    pub evidence: Vec<Evidence>,
    /// The architecture this binary was *compiled* for
    /// (`std::env::consts::ARCH`). Under translation this is the foreign arch.
    pub binary_arch: &'static str,
    /// What `uname -m` reports, or `None` when `uname` could not be run.
    pub uname_arch: Option<String>,
    /// What the runner said the Docker daemon runs on, from `BENCH_DAEMON_ARCH`,
    /// normalised to `std::env::consts::ARCH` spelling. `None` when the runner
    /// did not supply it.
    pub daemon_arch: Option<String>,
}

impl EmulationReport {
    /// The probe's JSON, one key per line so a shell control can read a field
    /// out with `sed` and no `jq` on the host.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        out.push_str("{\n");
        let _ = writeln!(out, "  \"emulated\": {},", self.emulated.as_json());
        let _ = writeln!(out, "  \"binaryArch\": {},", json_string(self.binary_arch));
        let _ = writeln!(
            out,
            "  \"unameArch\": {},",
            json_opt(self.uname_arch.as_deref())
        );
        let _ = writeln!(
            out,
            "  \"daemonArch\": {},",
            json_opt(self.daemon_arch.as_deref())
        );
        out.push_str("  \"evidence\": [\n");
        for (i, e) in self.evidence.iter().enumerate() {
            let comma = if i + 1 == self.evidence.len() {
                ""
            } else {
                ","
            };
            let _ = writeln!(
                out,
                "    {{\"source\": {}, \"verdict\": {}, \"detail\": {}}}{}",
                json_string(e.source),
                json_string(e.verdict.as_str()),
                json_string(&e.detail),
                comma
            );
        }
        out.push_str("  ]\n}");
        out
    }
}

/// Minimal JSON string escaping. The canonical-JSON escaping rules the digests
/// depend on live in `storage::integrity`; this one only has to survive a
/// `sed` and a human, because nothing digests the probe's own pretty-printed
/// output.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_opt(s: Option<&str>) -> String {
    match s {
        Some(s) => json_string(s),
        None => "null".to_string(),
    }
}

/// Normalise an architecture name onto `std::env::consts::ARCH` spelling.
///
/// Docker says `amd64` / `arm64`, `uname -m` says `x86_64` / `aarch64`, and
/// Rust says `x86_64` / `aarch64`. Comparing two of those without normalising
/// makes every comparison disagree, which would read as "emulated" on every
/// host. Anything unrecognised is passed through unchanged so a comparison
/// against it is at worst inconclusive, never a confident wrong answer.
pub fn normalise_arch(arch: &str) -> String {
    match arch.trim() {
        "amd64" | "x86_64" | "x64" => "x86_64".to_string(),
        "arm64" | "aarch64" | "arm64/v8" => "aarch64".to_string(),
        "386" | "i386" | "i686" => "x86".to_string(),
        other => other.to_string(),
    }
}

/// Whether a mapped file path is an instruction translator.
///
/// Matched on path *components* rather than a substring of the whole path, so a
/// perfectly ordinary `/opt/qemuchart/lib.so` in a native container does not
/// read as emulation. The two shapes that matter are Rosetta's
/// `/run/rosetta/rosetta` and qemu-user's `qemu-x86_64` / `qemu-aarch64`
/// interpreters, wherever a distribution puts them.
pub fn path_is_translator(path: &str) -> bool {
    path.split('/').any(|component| {
        let c = component.to_ascii_lowercase();
        c == "rosetta"
            || c.starts_with("rosetta-")
            || c == "qemu-user"
            || (c.starts_with("qemu-") && !c.starts_with("qemu-system"))
    })
}

/// Run the probe.
pub fn probe() -> EmulationReport {
    let binary_arch = std::env::consts::ARCH;
    let uname_arch = uname_machine().map(|m| normalise_arch(&m));
    let daemon_arch = std::env::var("BENCH_DAEMON_ARCH")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(|v| normalise_arch(&v));

    // In the order the report prints them: the primary evidence first, then the
    // one that is blind in a container, then the two that need something from
    // outside the process.
    let evidence = vec![
        maps_evidence(),
        binfmt_evidence(binary_arch),
        daemon_arch_evidence(binary_arch, daemon_arch.as_deref()),
        uname_evidence(binary_arch, uname_arch.as_deref()),
    ];

    // Any positive sighting wins: a translator that was actually seen in the
    // address space is not outvoted by three sources that could not see it.
    // Otherwise a source that was in a position to look and saw nothing settles
    // it. If nothing was in a position to look, the answer is `Unknown` and the
    // aggregator refuses the run rather than averaging it in.
    let emulated = if evidence
        .iter()
        .any(|e| e.verdict == EvidenceVerdict::Emulated)
    {
        Emulated::Yes
    } else if evidence
        .iter()
        .any(|e| e.verdict == EvidenceVerdict::Native)
    {
        Emulated::No
    } else {
        Emulated::Unknown
    };

    EmulationReport {
        emulated,
        evidence,
        binary_arch,
        uname_arch,
        daemon_arch,
    }
}

/// The primary evidence: does this process have a translator mapped into it?
///
/// Rosetta 2 maps `/run/rosetta/rosetta` into every process it translates, and
/// the mapping is visible in `/proc/self/maps` even though `/run/rosetta` is
/// not reachable through the container's own mount namespace — the path in a
/// maps line is the one the mapping was created from, not one you can `open`.
/// qemu-user is itself the process, so its interpreter shows up the same way.
fn maps_evidence() -> Evidence {
    match std::fs::read_to_string("/proc/self/maps") {
        Ok(text) => {
            // A maps line is `addr perms offset dev inode [path]`; the path is
            // optional and may contain spaces, so take everything from the sixth
            // whitespace-separated field on rather than splitting on every space.
            for line in text.lines() {
                let path = line.split_whitespace().nth(5).unwrap_or("");
                if !path.is_empty() && path_is_translator(path) {
                    return Evidence {
                        source: "proc-self-maps",
                        verdict: EvidenceVerdict::Emulated,
                        detail: format!("a translator is mapped into this process: {path}"),
                    };
                }
            }
            Evidence {
                source: "proc-self-maps",
                verdict: EvidenceVerdict::Native,
                detail: format!(
                    "read {} mappings, none of them a translator",
                    text.lines().count()
                ),
            }
        }
        Err(err) => Evidence {
            source: "proc-self-maps",
            verdict: EvidenceVerdict::Inconclusive,
            detail: format!("/proc/self/maps is unreadable ({err}), so nothing was observed here"),
        },
    }
}

/// `/proc/sys/fs/binfmt_misc`, which on the machine this was written for is
/// present and empty inside every container, on both platforms.
///
/// It is kept anyway because it is not blind everywhere: a plain Linux host
/// with `qemu-user-static` registered does list its handlers, and the CI
/// runners this suite will eventually use are plain Linux hosts. The reading is
/// narrow on purpose: a handler registered for *this binary's own*
/// architecture, since that is the one that would have to fire to run it. A
/// native x86_64 NAS with `qemu-aarch64` registered for cross-building does not
/// trip it, because `qemu-aarch64` is not a handler for an x86_64 binary.
fn binfmt_evidence(binary_arch: &str) -> Evidence {
    let dir = std::path::Path::new("/proc/sys/fs/binfmt_misc");
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            return Evidence {
                source: "binfmt-misc",
                verdict: EvidenceVerdict::Inconclusive,
                detail: format!("{} is unreadable ({err})", dir.display()),
            };
        }
    };

    let mut names = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "register" || name == "status" {
            continue;
        }
        names.push(name);
    }
    names.sort();

    let wanted = format!("qemu-{binary_arch}");
    for name in &names {
        if name == "rosetta" || *name == wanted {
            let enabled = std::fs::read_to_string(dir.join(name))
                .map(|s| s.lines().any(|l| l.trim() == "enabled"))
                .unwrap_or(false);
            if enabled {
                return Evidence {
                    source: "binfmt-misc",
                    verdict: EvidenceVerdict::Emulated,
                    detail: format!(
                        "an enabled binfmt_misc handler '{name}' is registered for this \
                         binary's own architecture ({binary_arch})"
                    ),
                };
            }
        }
    }

    // Empty is NOT native. Inside Docker Desktop the directory exists and lists
    // nothing whether or not Rosetta is translating this very process, so an
    // empty listing is exactly as informative as an unreadable one.
    Evidence {
        source: "binfmt-misc",
        verdict: EvidenceVerdict::Inconclusive,
        detail: if names.is_empty() {
            "the directory exists and lists no handlers, which inside a Docker Desktop \
             container is what it says whether or not translation is happening"
                .to_string()
        } else {
            format!(
                "handlers present ({}), none of them registered for {binary_arch}",
                names.join(", ")
            )
        },
    }
}

/// The fallback for a host where `/proc` says nothing: the runner tells the
/// process what the daemon runs on, and the process compares it against what it
/// was compiled for.
///
/// This is the one fact the container genuinely cannot observe from inside
/// itself, which is why `tools/probe-emulation.sh` passes it in as
/// `BENCH_DAEMON_ARCH` from `docker version --format '{{.Server.Arch}}'`. It is
/// weaker than the maps evidence because it trusts the runner, and the report
/// records that it was used so a reader can weigh it.
fn daemon_arch_evidence(binary_arch: &str, daemon_arch: Option<&str>) -> Evidence {
    match daemon_arch {
        Some(daemon) if daemon != normalise_arch(binary_arch) => Evidence {
            source: "daemon-arch",
            verdict: EvidenceVerdict::Emulated,
            detail: format!(
                "the runner says the daemon runs on {daemon} and this binary was compiled \
                 for {binary_arch}, so something is translating it"
            ),
        },
        Some(daemon) => Evidence {
            source: "daemon-arch",
            verdict: EvidenceVerdict::Native,
            detail: format!("the runner says the daemon runs on {daemon}, matching this binary"),
        },
        None => Evidence {
            source: "daemon-arch",
            verdict: EvidenceVerdict::Inconclusive,
            detail: "BENCH_DAEMON_ARCH was not set, so the runner supplied no daemon \
                     architecture to compare against"
                .to_string(),
        },
    }
}

/// `uname -m` against the compiled-for architecture.
///
/// Agreement proves nothing: Rosetta mangles the reported machine so an x86_64
/// binary under it sees `x86_64`, which is why agreement here is
/// `Inconclusive` and not `Native`. Disagreement is real evidence, and it is
/// the shape qemu-user takes when it is invoked without `-p` personality
/// mangling.
fn uname_evidence(binary_arch: &str, uname_arch: Option<&str>) -> Evidence {
    match uname_arch {
        Some(machine) if machine != normalise_arch(binary_arch) => Evidence {
            source: "uname",
            verdict: EvidenceVerdict::Emulated,
            detail: format!(
                "the kernel reports machine {machine} while this binary was compiled for \
                 {binary_arch}"
            ),
        },
        Some(machine) => Evidence {
            source: "uname",
            verdict: EvidenceVerdict::Inconclusive,
            detail: format!(
                "the kernel reports machine {machine}, matching this binary, which a \
                 personality-mangling translator would also produce"
            ),
        },
        None => Evidence {
            source: "uname",
            verdict: EvidenceVerdict::Inconclusive,
            detail: "uname could not be run".to_string(),
        },
    }
}

/// `uname -m` by subprocess.
///
/// The obvious `libc::uname` is not available here: this file is compiled by
/// bare `rustc` with no crates for the shell control, so it has `std` and
/// nothing else. The cost is one fork per run of a probe that runs once per
/// sweep, and a failure is honestly reported as `None` rather than guessed.
fn uname_machine() -> Option<String> {
    let out = std::process::Command::new("uname")
        .arg("-m")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    // RED against a `path_is_translator` written as
    // `path.contains("rosetta") || path.contains("qemu")`, which is the obvious
    // first implementation and calls an ordinary library emulated.
    #[test]
    fn a_substring_match_on_the_whole_path_is_not_the_rule() {
        assert!(path_is_translator("/run/rosetta/rosetta"));
        assert!(path_is_translator("/usr/bin/qemu-x86_64"));
        assert!(path_is_translator("/usr/libexec/qemu-user/qemu-aarch64"));
        assert!(!path_is_translator("/opt/qemuchart/lib/libqemuchart.so"));
        assert!(!path_is_translator("/usr/lib/librosettastone.so"));
        assert!(!path_is_translator("/usr/bin/qemu-system-x86_64"));
    }

    // RED against a probe that compares Docker's `amd64` against Rust's
    // `x86_64` without normalising, which reports every single host as emulated.
    #[test]
    fn docker_and_rust_arch_spellings_are_reconciled_before_they_are_compared() {
        assert_eq!(normalise_arch("amd64"), "x86_64");
        assert_eq!(normalise_arch("arm64"), "aarch64");
        assert_eq!(normalise_arch("aarch64"), "aarch64");
        assert_eq!(normalise_arch(" arm64 "), "aarch64");
        let matching = daemon_arch_evidence("x86_64", Some(&normalise_arch("amd64")));
        assert_eq!(matching.verdict, EvidenceVerdict::Native, "{matching:?}");
    }

    // RED against a probe that treats an empty binfmt_misc directory as proof of
    // a native run. Inside Docker Desktop the directory is empty on BOTH
    // platforms, so reading empty as native returns `false` for a Rosetta run.
    #[test]
    fn an_empty_binfmt_misc_directory_is_not_evidence_of_anything() {
        let e = binfmt_evidence("x86_64");
        assert_ne!(
            e.verdict,
            EvidenceVerdict::Native,
            "binfmt_misc must never return a native verdict, it produced: {e:?}"
        );
    }

    // RED against collapsing the third state into `false`. `Unknown` has to
    // serialise as something a reader cannot mistake for an observation.
    #[test]
    fn unknown_is_not_spelled_false() {
        assert_eq!(Emulated::Unknown.as_json(), "\"unknown\"");
        assert_eq!(Emulated::No.as_json(), "false");
        assert_eq!(Emulated::Yes.as_json(), "true");
    }

    // RED against a `to_json` that hides the sources it consulted, which is the
    // difference between a document that says what produced it and one that
    // only says what it concluded.
    #[test]
    fn the_report_names_the_evidence_it_used() {
        let report = probe();
        let json = report.to_json();
        for source in ["proc-self-maps", "binfmt-misc", "daemon-arch", "uname"] {
            assert!(json.contains(source), "{source} missing from {json}");
        }
        // The shell control reads this field with sed, so its exact spelling is
        // load-bearing rather than cosmetic.
        assert!(
            json.lines()
                .any(|l| l.trim_start().starts_with("\"emulated\": ")),
            "the control's sed expression will not find the verdict in {json}"
        );
    }
}
