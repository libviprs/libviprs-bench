//! The `engines` family's attestation: whether a cell measured the engine it
//! names, decided from the pyramid that engine really wrote.
//!
//! The reviewer's warning about the storage family applies here word for word:
//! the cheapest way to make a document archivable is to stamp `attested: true`
//! and move on, and in a sweep where everything really is attested no assertion
//! over the document can tell an observation from a constant. So the verdict is
//! tested where it CAN fail — handed a group that disagrees, a group of one, an
//! engine that wrote nothing — and every case has to move it.
//!
//! What this does not cover, stated plainly: the one line in `run_sweep` that
//! copies the verdict onto the row. A constant there is indistinguishable from
//! a correct run in which everything really is attested, and its mutation row
//! is published as surviving rather than quietly dropped.

use std::time::Duration;

use libviprs_bench::engines::attest::attest_group;
use libviprs_bench::harness::Engine;
use libviprs_bench::{ArtefactFacts, RunMetrics};

/// One repetition that wrote a real pyramid.
fn wrote(engine: Engine, grid: Vec<u64>, tiles: u64) -> RunMetrics {
    RunMetrics {
        label: engine.as_str().to_string(),
        width: 1024,
        height: 720,
        engine: engine.as_str().to_string(),
        measurement_path: String::new(),
        wall_time: Duration::from_millis(500),
        tracked_memory_bytes: 2_097_152,
        peak_rss_bytes: 9_700_000,
        stats: None,
        per_level_tiles: grid,
        artefact: Some(ArtefactFacts {
            output_bytes: 1_438_844,
            filesystem_entries: 37,
            directories: 12,
            allocated_bytes: 1_474_560,
        }),
        equivalence_psnr_db: None,
        tiles_produced: tiles,
        levels_processed: 4,
        tiles_skipped: 0,
        strips: 0,
        batches: 0,
        inflight_strips: 0,
        concurrency: 1,
        memory_budget_bytes: 0,
    }
}

/// The ordinary group: three engines, two repetitions each, all agreeing.
fn agreeing_group() -> Vec<(Engine, Vec<RunMetrics>)> {
    [Engine::Monolithic, Engine::Streaming, Engine::MapReduce]
        .into_iter()
        .map(|e| (e, vec![wrote(e, vec![16, 4, 4, 1], 25), wrote(e, vec![16, 4, 4, 1], 25)]))
        .collect()
}

fn verdict(group: &[(Engine, Vec<RunMetrics>)], engine: Engine) -> libviprs_bench::engines::attest::Attestation {
    attest_group(group)
        .into_iter()
        .find(|(e, _)| *e == engine)
        .map(|(_, v)| v)
        .expect("the engine is in the group")
}

/// RED against `attested: true` as a constant.
///
/// Three moves, each of which a constant survives and an observation does not:
/// an engine that wrote nothing, an engine that wrote two different pyramids,
/// and an engine whose siblings wrote a different one.
#[test]
fn the_verdict_moves_with_what_was_observed() {
    // The control first. A green below means nothing unless the ordinary case
    // really is attested.
    let ok = agreeing_group();
    for (engine, verdict) in attest_group(&ok) {
        assert!(
            verdict.is_attested(),
            "{} should be attested in a group that agrees: {:?}",
            engine.as_str(),
            verdict.reasons()
        );
        assert!(verdict.reasons().is_empty());
    }

    // Nothing walked. `artefact: None` is what a run that left no directory to
    // walk looks like, and an unobserved cell is a label rather than a
    // measurement.
    let mut blind = agreeing_group();
    blind[0].1[1].artefact = None;
    let v = verdict(&blind, Engine::Monolithic);
    assert!(!v.is_attested());
    assert!(!v.observed);
    assert!(
        v.reasons().iter().any(|r| r.contains("no pyramid")),
        "the refusal names the half that failed: {:?}",
        v.reasons()
    );

    // Two repetitions of one engine wrote different pyramids. The plan and the
    // source are identical across them, so that is a defect rather than a
    // delta.
    let mut wobbled = agreeing_group();
    wobbled[1].1[1].per_level_tiles = vec![16, 4, 4];
    let v = verdict(&wobbled, Engine::Streaming);
    assert!(!v.is_attested());
    assert!(!v.reproduced);
    assert!(
        v.reasons()
            .iter()
            .any(|r| r.contains("same pyramid every repetition")),
        "{:?}",
        v.reasons()
    );

    // One engine disagrees with the other two about the tile count. All three
    // are then unattested, which is right: the group did not measure equal
    // work and nothing in it says which member is the wrong one.
    let mut split: Vec<(Engine, Vec<RunMetrics>)> = agreeing_group();
    for run in split[2].1.iter_mut() {
        run.tiles_produced = 24;
    }
    let v = verdict(&split, Engine::MapReduce);
    assert!(!v.is_attested());
    assert!(!v.agreed);
    assert!(
        v.reasons().iter().any(|r| r.contains("equal work")),
        "{:?}",
        v.reasons()
    );
}

/// RED against an engine that attests itself.
///
/// The whole reason the grid is worth checking is that three engines walking
/// one plan must produce the same tiles. A group of one cannot demonstrate it,
/// and calling it attested would be the label-shaped answer the storage family
/// refuses for its lone backend.
#[test]
fn one_engine_alone_is_never_attested() {
    let alone = vec![(
        Engine::Monolithic,
        vec![wrote(Engine::Monolithic, vec![16, 4, 4, 1], 25)],
    )];
    let verdicts = attest_group(&alone);
    assert_eq!(verdicts.len(), 1);
    assert!(!verdicts[0].1.is_attested());
    assert!(verdicts[0].1.observed, "it did write a pyramid");
    assert!(!verdicts[0].1.agreed, "and had nothing to agree with");
    assert!(
        verdicts[0]
            .1
            .reasons()
            .iter()
            .any(|r| r.contains("only engine")),
        "{:?}",
        verdicts[0].1.reasons()
    );

    // And an empty group attests nothing rather than everything.
    assert!(attest_group(&[]).is_empty());
}

/// RED against an attestation that compares the grid and not the tile count, or
/// the other way round.
///
/// The two can come apart and each one catches what the other cannot. A level
/// directory that lost a tile keeps the level count and moves the grid; an
/// engine that skipped a blank tile moves the count and, if the tile was the
/// only one in its level, not the grid's length.
#[test]
fn the_grid_and_the_tile_count_are_both_compared() {
    // Same tile count, different distribution across levels. Only the grid
    // comparison sees this.
    let mut regrouped = agreeing_group();
    for run in regrouped[0].1.iter_mut() {
        run.per_level_tiles = vec![17, 4, 3, 1];
    }
    assert_eq!(
        regrouped[0].1[0].per_level_tiles.iter().sum::<u64>(),
        25,
        "the fixture has to keep the total, or it is not testing the grid"
    );
    assert!(!verdict(&regrouped, Engine::Monolithic).agreed);

    // Same grid, different tile count. Only the count comparison sees this.
    let mut miscounted = agreeing_group();
    for run in miscounted[0].1.iter_mut() {
        run.tiles_produced = 26;
    }
    assert_eq!(
        miscounted[0].1[0].per_level_tiles,
        miscounted[1].1[0].per_level_tiles,
        "the fixture has to keep the grid, or it is not testing the count"
    );
    assert!(!verdict(&miscounted, Engine::Monolithic).agreed);
}

/// RED against an attestation satisfied by an empty directory.
///
/// Zero entries is a *better* number than the one a real tree costs, so a walk
/// that found nothing must not read as a walk that found agreement.
#[test]
fn an_empty_artefact_is_unobserved_rather_than_small() {
    let mut empty = agreeing_group();
    for run in empty[0].1.iter_mut() {
        run.artefact = Some(ArtefactFacts {
            output_bytes: 0,
            filesystem_entries: 0,
            directories: 0,
            allocated_bytes: 0,
        });
    }
    let v = verdict(&empty, Engine::Monolithic);
    assert!(!v.observed);
    assert!(!v.is_attested());
}
