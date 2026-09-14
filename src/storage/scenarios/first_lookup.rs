//! The other half of the old cold row: one lookup, in a process that has never
//! opened this archive before.
//!
//! `read_cold` opened a fresh reader per lookup inside one long-lived process,
//! which makes the reader cold and leaves everything else warm: the file is in
//! the page cache, the allocator is warmed up, the branch predictors have seen
//! the varint loop tens of thousands of times. A client that runs
//! `viprs pmtiles tile` pays none of that, and it is the shape this scenario
//! exists to price, so a repetition here is a process.
//!
//! The evidence that it really is a fresh process per repetition is the child's
//! own pid, read out of the child and not asserted by the parent. An
//! implementation that loops in one process reports one pid `reps` times, and
//! `first_lookup_runs_in_a_fresh_process_per_rep` says so.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use libviprs::planner::TileCoord;
use super::super::FileReaderFactory;
use super::super::cells::{Backend, Cell};
use super::ReaderFactory;

/// The environment variable a child reads to learn what to measure.
pub const CHILD_VAR: &str = "LIBVIPRS_STORAGE_FIRST_LOOKUP";

/// The prefix a child puts on the one line the parent reads back.
///
/// A child's stdout carries whatever the test harness feels like printing, so
/// the parent picks its line out by prefix rather than reading the last line
/// and hoping.
pub const CHILD_LINE_PREFIX: &str = "K14-FIRST-LOOKUP";

/// One repetition: which process ran it, and what it cost.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rep {
    /// The child's own process id, printed by the child.
    pub pid: u32,
    pub micros: f64,
    /// Whether the lookup found a tile. A miss is a legal answer and a
    /// different measurement, so it is carried rather than folded away.
    pub hit: bool,
}

/// Spell out what a child should measure.
pub fn child_spec(archive: &Path, coord: TileCoord) -> String {
    format!(
        "{}\t{}\t{}\t{}",
        archive.display(),
        coord.level,
        coord.col,
        coord.row
    )
}

/// Read a spec back.
pub fn parse_child_spec(spec: &str) -> Option<(PathBuf, TileCoord)> {
    let mut parts = spec.split('\t');
    let path = PathBuf::from(parts.next()?);
    let level = parts.next()?.parse().ok()?;
    let col = parts.next()?.parse().ok()?;
    let row = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((path, TileCoord { level, col, row }))
}

/// The whole of what a child does: open the archive, look one tile up, and say
/// what that cost and who did it.
pub fn child_main(spec: &str) -> String {
    let (archive, coord) = parse_child_spec(spec).expect("the parent handed a well-formed spec");
    // Through the family's factory like every other reader here, even in a
    // child. The open is inside the timed section because the open is what this
    // scenario prices.
    let cell = Cell::new(1, 1, 1, super::super::cells::Source::Gradient, 0);
    let plan = cell.plan().expect("a one-tile plan");
    let readers = FileReaderFactory::new(Backend::PmTiles, &archive, &plan);
    let at = Instant::now();
    let reader = readers.fresh().expect("the archive opens for reading");
    let tile = reader.tile(coord).expect("a lookup succeeds");
    let micros = at.elapsed().as_secs_f64() * 1e6;
    format!(
        "{CHILD_LINE_PREFIX} {} {micros} {}",
        std::process::id(),
        u8::from(tile.is_some())
    )
}

/// Read a child's line back out of its stdout.
pub fn parse_child_line(stdout: &str) -> Option<Rep> {
    let line = stdout
        .lines()
        .find(|line| line.starts_with(CHILD_LINE_PREFIX))?;
    let mut parts = line.split_whitespace().skip(1);
    Some(Rep {
        pid: parts.next()?.parse().ok()?,
        micros: parts.next()?.parse().ok()?,
        hit: parts.next()? == "1",
    })
}

/// Run `reps` repetitions, each one a process of its own.
///
/// The command factory is a parameter rather than a hard-coded
/// `current_exe()` so the tests can drive the same loop, and so the production
/// path and the tested path are the same code rather than two implementations
/// that agree today.
pub fn run(
    reps: usize,
    mut command: impl FnMut(usize) -> Command,
) -> Result<Vec<Rep>, String> {
    let mut out = Vec::with_capacity(reps);
    for rep in 0..reps {
        let output = command(rep)
            .output()
            .map_err(|error| format!("repetition {rep} could not be spawned: {error}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed = parse_child_line(&stdout).ok_or_else(|| {
            format!(
                "repetition {rep} printed no `{CHILD_LINE_PREFIX}` line; status {:?}, stderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )
        })?;
        out.push(parsed);
    }
    Ok(out)
}

/// The command the production path runs: this binary again, with the spec in
/// the environment.
pub fn child_command(exe: &Path, archive: &Path, coord: TileCoord) -> Command {
    let mut command = Command::new(exe);
    command.env(CHILD_VAR, child_spec(archive, coord));
    command
}

/// How many distinct processes a set of repetitions ran in.
///
/// The number the fresh-process claim rests on. It is a count over what the
/// children said about themselves, so an implementation that never spawned one
/// cannot fake it without lying about its own pid.
pub fn distinct_pids(reps: &[Rep]) -> usize {
    let mut pids: Vec<u32> = reps.iter().map(|rep| rep.pid).collect();
    pids.sort_unstable();
    pids.dedup();
    pids.len()
}
