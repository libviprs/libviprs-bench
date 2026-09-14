//! The storage family is a family, and it writes where a family writes.
//!
//! Two models of one concept met at compose. K1.1 made `storage` a family
//! selected with `--family`, which is what issue #64 specified and what the
//! report, the charts and the site page key on. K1.2 made it a command sitting
//! beside `report` and `scalability`, with its own `--storage-profile` and its
//! output at `report/storage-results.json`. Both branches were green on their
//! own, which is why the merge was dry-run first.
//!
//! K1.1's model stands. These two tests are what stops the other one coming
//! back, and each names the wrong implementation it goes red against.

use std::path::{Path, PathBuf};
use std::process::Command;

use libviprs_bench::family::{ALL_FAMILIES, Family};
use libviprs_bench::storage;

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "libviprs-storage-family-{}-{}",
        std::process::id(),
        label
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// Run `run-bench.sh` with a `docker` that records its arguments instead of
/// doing anything.
///
/// The script is the thing under test and Docker is not, so the stub is how a
/// dispatch gets checked without a build. It answers every subcommand the
/// script uses (`info`, `ps`, `rm`, `build`, `run`) by appending its argv to a
/// log and exiting 0.
fn run_bench(args: &[&str], dir: &Path) -> (std::process::Output, String) {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).expect("a stub directory");
    let log = dir.join("docker-argv.log");
    std::fs::write(
        bin.join("docker"),
        format!(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> {}\nexit 0\n",
            log.display()
        ),
    )
    .expect("the stub writes");
    let mut perms = std::fs::metadata(bin.join("docker"))
        .expect("the stub exists")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(bin.join("docker"), perms).expect("the stub is executable");

    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = Command::new("bash")
        .arg(crate_root().join("run-bench.sh"))
        .args(args)
        .env("PATH", path)
        .current_dir(crate_root())
        .output()
        .expect("run-bench.sh runs");
    let recorded = std::fs::read_to_string(&log).unwrap_or_default();
    (output, recorded)
}

/// RED against the K1.2 branch as it stood, where `storage` was a positional
/// command beside `report` and `scalability` with its own `--storage-profile`
/// and no `--family` at all.
///
/// A script that accepted both spellings would have two ways to ask for one
/// thing, and only one of them would put the output where the report, the
/// charts and the site page look for it.
#[test]
fn the_storage_family_is_reached_as_a_family_not_a_command() {
    let dir = scratch("command-vs-family");

    // Asking for it as a command is refused, and the refusal says how to ask.
    let (refused, _) = run_bench(&["storage"], &dir);
    assert!(
        !refused.status.success(),
        "`run-bench.sh storage` was accepted as a command"
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("--family storage"),
        "the refusal has to name the spelling that works, got: {stderr}"
    );
    assert!(
        stderr.contains("not a command"),
        "and say what went wrong, got: {stderr}"
    );

    // Asking for it as a family runs, and dispatches to the storage family's
    // own binary in the small stage. Both halves matter: the binary is what
    // lets the family build with no `libvips` feature, and the stage is what
    // keeps the libvips source build out of a job that cannot use it.
    let (accepted, docker) = run_bench(&["--family", "storage", "--storage-profile", "ci"], &dir);
    assert!(
        accepted.status.success(),
        "`--family storage` failed: {}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let build = docker
        .lines()
        .find(|l| l.starts_with("build ") || l.contains(" build "))
        .unwrap_or_else(|| panic!("no docker build was issued, log was:\n{docker}"));
    assert!(
        build.contains("--target storage"),
        "the storage family has to build its own stage, got: {build}"
    );
    assert!(
        build.contains("--platform linux/"),
        "and spell out the platform, got: {build}"
    );
    let run = docker
        .lines()
        .find(|l| l.starts_with("run ") && l.contains("--bin storage"))
        .unwrap_or_else(|| panic!("no docker run of the storage bin, log was:\n{docker}"));
    assert!(
        run.contains("--family storage"),
        "the binary is told which family it is measuring, got: {run}"
    );
    assert!(
        run.contains("--profile ci"),
        "and --storage-profile reaches it, got: {run}"
    );
    assert!(
        !run.contains("--features"),
        "the storage family never takes a cargo feature, got: {run}"
    );

    // The control: the default family still works and is not the storage one,
    // so the refusal above is about the spelling and not about the script
    // being broken.
    let (default_family, docker) = run_bench(&["--arch", "arm"], &dir);
    assert!(
        default_family.status.success(),
        "the default family failed: {}",
        String::from_utf8_lossy(&default_family.stderr)
    );
    assert!(
        docker.contains("--target engines"),
        "the default family builds the engines stage, log was:\n{docker}"
    );
}

/// RED against the literal `report/storage-results.json` the K1.2 branch wrote
/// to, which sits beside the family directories instead of inside one.
///
/// Two families that write into one directory can overwrite each other's
/// charts and append to each other's history, and the JS renderer takes a
/// `--report-dir` and nothing else, so a file outside a family directory is a
/// file no chart will ever draw.
#[test]
fn every_family_writes_under_its_own_report_directory() {
    let root = Path::new("/tmp/report-root");
    for family in ALL_FAMILIES {
        assert_eq!(
            family.report_dir(root),
            root.join(family.as_str()),
            "{family} writes outside its own directory"
        );
    }

    // The storage family's document, specifically, because it is the one that
    // moved. Its default output is under the family directory and nowhere else.
    let out = storage::default_output_path(root);
    assert_eq!(
        out,
        root.join("storage").join("storage-results.json"),
        "the storage document is not under report/storage/"
    );
    assert!(
        out.starts_with(Family::Storage.report_dir(root)),
        "and it is not derived from the family's own directory"
    );

    // Run the binary and check where the bytes actually land, because a path
    // function agreeing with itself proves nothing about the writer.
    let dir = scratch("report-dir");
    let status = Command::new(env!("CARGO_BIN_EXE_storage"))
        .args(["--family", "storage", "--profile", "ci"])
        .env("LIBVIPRS_BENCH_REPORT_DIR", &dir)
        .status()
        .expect("the storage binary runs");
    assert!(status.success(), "the ci sweep failed");
    let written = dir.join("storage").join("storage-results.json");
    assert!(
        written.is_file(),
        "the sweep wrote nothing at {}",
        written.display()
    );
    assert!(
        !dir.join("storage-results.json").exists(),
        "the sweep also wrote the old flat path beside the family directories"
    );

    // And the old literal is gone from the source, not merely unused.
    for entry in ["src/storage_main.rs", "src/storage/mod.rs", "run-bench.sh"] {
        let text = std::fs::read_to_string(crate_root().join(entry))
            .unwrap_or_else(|e| panic!("read {entry}: {e}"));
        assert!(
            !text.contains("report/storage-results.json"),
            "{entry} still names the flat path"
        );
    }
}
