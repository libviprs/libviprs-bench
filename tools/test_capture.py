"""Tests for tools/capture.py.

Each test names, in a comment, the wrong implementation it goes red against.
The ones that matter most are the last three: they drive the whole driver with
a recording executor and assert what reaches the repository, because the claim
this driver has to hold is not "it captures" but "a capture the gates refuse
cannot reach the page", and that is a claim about files that were never
written.
"""

from __future__ import annotations

import json
import shutil
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import capture  # noqa: E402


REPO = Path(__file__).resolve().parent.parent


class SettleThreshold(unittest.TestCase):
    def test_scales_with_the_core_count(self):
        # Red against an absolute threshold. The NAS carries resident backupd
        # and postgres containers and its load floor is 1.4 to 2.2 with all of
        # them at 0% CPU, so a constant 1.2 can never be met and the wait
        # becomes a fixed five-minute delay that measures nothing. A fixed
        # number would give the same answer for both core counts here.
        self.assertNotEqual(capture.settle_threshold(6), capture.settle_threshold(12))
        self.assertEqual(capture.settle_threshold(6), 3.0)

    def test_the_six_core_threshold_clears_that_machines_floor(self):
        # Red against a threshold that is relative but still unreachable, which
        # is the failure the absolute one actually was: a quarter of six is 1.5
        # and sits inside the 1.4 to 2.2 floor.
        self.assertGreater(capture.settle_threshold(6), 2.2)

    def test_a_zero_core_read_is_an_error_and_not_a_default(self):
        # Red against `cores or 6`. A container that printed nothing is a read
        # that failed, and turning it into a plausible number hides the failure
        # behind a threshold that happens to work.
        with self.assertRaises(ValueError):
            capture.settle_threshold(0)


class PublishableProfiles(unittest.TestCase):
    def setUp(self):
        tmp = __import__("tempfile").TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.repo = Path(tmp.name)
        (self.repo / "tools" / "contract").mkdir(parents=True)

    def write(self, producer):
        (self.repo / "tools" / "contract" / "config.json").write_text(
            json.dumps({"producer": producer})
        )

    def test_the_set_comes_from_the_config_the_importer_reads(self):
        # Red against a list written out again in the driver. The importer's
        # copy is the one that decides, so a second one here is a second thing
        # to forget when the first moves.
        self.write({"publishableProfiles": ["xl"]})
        self.assertEqual(capture.publishable_profiles(self.repo), ["xl"])

    def test_an_empty_set_is_a_configuration_fault_and_not_a_verdict(self):
        # Red against `profile "full" is not publishable ()`. That sentence is
        # libviprs-bench #82: read as a verdict it is nonsense, because with
        # nothing in the set no profile could ever have passed. The message has
        # to say the rule was never written down.
        self.write({"publishableProfiles": []})
        with self.assertRaises(capture.Refused) as raised:
            capture.publishable_profiles(self.repo)
        reason = raised.exception.reasons[0]
        self.assertIn("publishableProfiles", reason)
        self.assertIn("fault in the config", reason)
        self.assertNotIn("()", reason)

    def test_a_missing_config_refuses_rather_than_guessing(self):
        # Red against a default of ["full", "xl"] in the driver. A driver that
        # guesses the rule is a driver that can publish a profile the importer
        # would refuse, and the refusal would arrive after the measuring.
        shutil.rmtree(self.repo / "tools")
        with self.assertRaises(capture.Refused):
            capture.publishable_profiles(self.repo)

    def test_the_real_config_names_a_profile(self):
        # The positive control on the three above: they all pass against a
        # fixture, and this is the one that reads the repository's own config,
        # so a config that stopped defining the key fails here rather than
        # everywhere at once on the next capture.
        self.assertIn("full", capture.publishable_profiles(REPO))


class ParseRunId(unittest.TestCase):
    def test_reads_a_fresh_archive(self):
        self.assertEqual(
            capture.parse_run_id("archived /out/storage-x86.json as 2026-abc-0bc0 (sha256:x)"),
            "2026-abc-0bc0",
        )

    def test_reads_an_already_archived_run(self):
        # Red against reading only the `archived ... as ...` form. Re-running a
        # capture whose import failed archives nothing new and prints the other
        # sentence, and a driver that cannot read it cannot get back to the
        # import it was trying to reach.
        self.assertEqual(
            capture.parse_run_id(
                "/out/storage-x86.json is already archived as 2026-abc-0bc0, and its digest "
                "still matches, so nothing changed"
            ),
            "2026-abc-0bc0",
        )

    def test_no_id_raises_rather_than_returning_empty(self):
        # Red against `return ""`. An empty id makes the next step verify
        # `/archive/storage/.json`, which does not exist, and the run fails on a
        # missing file instead of on the aggregator saying nothing it expected.
        with self.assertRaises(RuntimeError):
            capture.parse_run_id("something went sideways")


class CellSummary(unittest.TestCase):
    def test_counts_outcomes_and_says_what_a_six_core_host_declined(self):
        # Red against a summary that reports a cell count and nothing else. Six
        # cores decline every T=8 rung, so an x86_64 run carries fewer storage
        # cells than an eight-core arm64 one, and a reader comparing the two
        # needs to be told rather than left to work it out.
        document = {
            "cells": [
                {"outcome": "ok", "machineLoad": {"quiet": True}},
                {"outcome": "ok", "machineLoad": {"quiet": False}},
                {"outcome": "failed", "reason": "a directory tree has no root"},
            ],
            "provenance": {"ncpu": 6, "arch": "x86_64", "emulated": False, "node": {"buildProfile": "release"}},
        }
        summary = capture.cell_summary(document)
        self.assertEqual(summary["byOutcome"], {"ok": 2, "failed": 1})
        self.assertEqual(summary["ncpu"], 6)
        self.assertTrue(summary["fewerCellsThanAnEightCoreHost"])
        self.assertEqual(summary["noisyCells"], 1)

    def test_an_eight_core_host_is_not_flagged(self):
        # The fixed-point control. A flag that is true for every host says
        # nothing, and this is the case where it has to be false.
        summary = capture.cell_summary({"cells": [], "provenance": {"ncpu": 8}})
        self.assertFalse(summary["fewerCellsThanAnEightCoreHost"])


# ---------------------------------------------------------------------------
# the whole driver, against a recording executor
# ---------------------------------------------------------------------------


def a_document(run_id="20260101T000000Z-abc-0bc00939"):
    return {
        "family": "libviprs-storage",
        "profile": "full",
        "cells": [{"outcome": "ok", "machineLoad": {"quiet": True}}],
        "provenance": {"ncpu": 6, "arch": "x86_64", "emulated": False, "node": {"buildProfile": "release"}},
        "runIdForTest": run_id,
    }


class Recorder(capture.Executor):
    """Answers every step from a table instead of running it.

    The point is not to simulate the NAS. It is that `main` walks its own real
    control flow, so what the tests below assert is what the driver does with
    the answers, which is the half that decides whether a refused run can reach
    the repository.
    """

    def __init__(self, *, import_code=0, check_code=0, families=("storage", "engines")):
        super().__init__(nas="test@nowhere", plan=False, echo=False)
        self.import_code = import_code
        self.check_code = check_code
        self.run_ids = {f: f"20260101T00000{i}Z-abc-0bc00939" for i, f in enumerate(families)}

    def run(self, step):
        self.steps.append(step)
        label = step.label
        out = ""
        code = 0
        if label.startswith("stage.rev."):
            out = "a" * 40
        elif label.startswith("settle.cores"):
            out = "6"
        elif label.startswith("settle.load"):
            out = "0.5"
        elif label.startswith("retrieve.document.archived."):
            family = label.rsplit(".", 1)[1]
            out = json.dumps(a_document(self.run_ids[family]))
        elif label.startswith("retrieve.document."):
            family = label.rsplit(".", 1)[1]
            out = json.dumps(a_document(self.run_ids[family]))
        elif label.startswith("retrieve.index."):
            family = label.rsplit(".", 1)[1]
            out = json.dumps([{"runId": self.run_ids[family], "file": f"{self.run_ids[family]}.json"}])
        elif label.startswith("check."):
            code = self.check_code
            out = "refused for 1 reasons\n  REFUSED[emulated]: the probe says true"
        elif label.startswith("archive."):
            family = label.rsplit(".", 1)[1]
            out = f"archived /out/{family}-x86.json as {self.run_ids[family]} (sha256:deadbeef)"
        elif label.startswith("import."):
            code = self.import_code
            out = "run 20260101"
        elif label.startswith("cleanup.list.images"):
            # Somebody else's run is on the machine, which is the ordinary case
            # and the reason the listing names things instead of counting them.
            out = "viprs-nas-storage:someone-else\nalpine:3.20"
        elif label.startswith("cleanup.list.scratch"):
            out = "someone-else"
        result = capture.Result(step, code, out, out if code else "")
        return result


class DriverWiring(unittest.TestCase):
    def setUp(self):
        tmp = __import__("tempfile").TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.repo = Path(tmp.name)
        (self.repo / "tools" / "publish").mkdir(parents=True)
        (self.repo / "tools" / "contract").mkdir(parents=True)
        shutil.copyfile(
            REPO / "tools" / "contract" / "config.json",
            self.repo / "tools" / "contract" / "config.json",
        )
        (self.repo / "tools" / "publish" / "history.json").write_text("[]\n")
        for family in ("storage", "engines"):
            (self.repo / "archive" / family).mkdir(parents=True)
            (self.repo / "archive" / family / "index.json").write_text("[]\n")
        self.summary_path = self.repo / "summary.json"

    def drive(self, recorder, extra=()):
        return capture.main(
            [
                "--repo",
                str(self.repo),
                "--nas",
                "test@nowhere",
                "--name",
                "unit-test",
                "--json",
                str(self.summary_path),
                *extra,
            ],
            executor=recorder,
        )

    def repo_state(self):
        return sorted(
            str(p.relative_to(self.repo))
            for p in self.repo.rglob("*.json")
            if p != self.summary_path
        )

    def test_a_clean_run_publishes_both_families(self):
        # The positive control for the two below. Without it a driver that
        # writes nothing ever would pass both refusal tests and prove nothing.
        code = self.drive(Recorder())
        self.assertEqual(code, 0)
        state = self.repo_state()
        self.assertIn("archive/storage/20260101T000000Z-abc-0bc00939.json", state)
        self.assertIn("archive/engines/20260101T000001Z-abc-0bc00939.json", state)
        summary = json.loads(self.summary_path.read_text())
        self.assertTrue(summary["published"])

    def test_an_import_refusal_leaves_the_repository_untouched(self):
        # Red against a driver that archives into `archive/` and imports into
        # `tools/publish/history.json` directly, which is the obvious way to
        # write this and the way that puts a refused run's document in the
        # repository with only the history entry missing. The whole chain runs
        # against a staging copy so that a refusal is a no-op rather than a
        # partial write somebody has to notice and undo.
        before = self.repo_state()
        code = self.drive(Recorder(import_code=1))
        self.assertEqual(code, 1)
        self.assertEqual(self.repo_state(), before)
        self.assertEqual((self.repo / "tools" / "publish" / "history.json").read_text(), "[]\n")
        summary = json.loads(self.summary_path.read_text())
        self.assertFalse(summary["published"])
        self.assertTrue(summary["refusals"], "a refusal says why")

    def test_a_refusal_at_the_archive_door_never_reaches_the_importer(self):
        # Red against a driver that archives regardless and lets the importer be
        # the only gate. `--check` is exit 1 for a reason: it is an answer, and
        # a document it turned away must not be filed. The assertion is on the
        # steps and on the repository, because "it exited 1" is also what a
        # driver that filed the document and then refused would do.
        before = self.repo_state()
        recorder = Recorder(check_code=1)
        code = self.drive(recorder)
        self.assertEqual(code, 1)
        labels = [s.label for s in recorder.steps]
        self.assertFalse([l for l in labels if l.startswith("archive.")], "nothing was archived")
        self.assertFalse([l for l in labels if l.startswith("import.")], "nothing was imported")
        self.assertEqual(self.repo_state(), before)
        summary = json.loads(self.summary_path.read_text())
        self.assertFalse(summary["published"])
        self.assertTrue(any("REFUSED" in r for r in summary["refusals"]))

    def test_an_archived_document_that_does_not_verify_is_not_imported(self):
        # The digests-do-not-verify door, which had no test at all. `--verify`
        # recomputes the four digests over the file as it was written, so a
        # failure here means the archive holds something other than what was
        # measured, and nothing downstream may be told about it.
        recorder = Recorder()
        original = recorder.run

        def fail_verify(step):
            if step.label.startswith("verify."):
                recorder.steps.append(step)
                return capture.Result(step, 1, "", "the cells block moved")
            return original(step)

        recorder.run = fail_verify
        before = self.repo_state()
        code = self.drive(recorder)
        self.assertEqual(code, 1)
        self.assertFalse(
            [s.label for s in recorder.steps if s.label.startswith("import.")],
            "a document that does not verify never reaches the importer",
        )
        self.assertEqual(self.repo_state(), before)
        summary = json.loads(self.summary_path.read_text())
        self.assertTrue(any("does not verify" in r for r in summary["refusals"]))

    def test_a_gate_that_fails_and_says_nothing_still_refuses_the_whole_run(self):
        # Red against collecting reasons by iterating over a failed step's
        # output and nothing else. A gate that exits non-zero with both streams
        # empty then appends nothing, the family is skipped in silence, and
        # because the publish rule used to be keyed on the refusal list being
        # empty, the OTHER family reached the repository and the run reported
        # success. An aggregator killed by the OOM killer exits 137 and says
        # nothing, which on a six-core machine running a full sweep is not
        # hypothetical.
        recorder = Recorder()
        original = recorder.run

        def die_quietly(step):
            if step.label == "check.engines":
                recorder.steps.append(step)
                return capture.Result(step, 137, "", "")
            return original(step)

        recorder.run = die_quietly
        before = self.repo_state()
        code = self.drive(recorder)
        self.assertEqual(code, 1)
        self.assertEqual(self.repo_state(), before, "the storage half is not published either")
        summary = json.loads(self.summary_path.read_text())
        self.assertFalse(summary["published"])
        self.assertTrue(
            any("printed nothing at all" in r for r in summary["refusals"]),
            f"a silent failure is named as one: {summary['refusals']}",
        )
        self.assertTrue(
            any("every family it was asked for or none" in r for r in summary["refusals"]),
            "and the run says why nothing was published",
        )

    def test_one_refused_family_does_not_publish_the_other(self):
        # Red against a per-family publish. A history that carries the storage
        # half of a capture and not the engines half reads as a complete run of
        # one family, and nothing on the page says the other one was refused.
        recorder = Recorder()
        original = recorder.run

        def refuse_engines(step):
            if step.label == "check.engines":
                recorder.steps.append(step)
                return capture.Result(step, 1, "", "REFUSED[emulated]: engines was refused")
            return original(step)

        recorder.run = refuse_engines
        before = self.repo_state()
        code = self.drive(recorder)
        self.assertEqual(code, 1)
        # The whole repository, not just the history. The recorder's importer
        # never writes the staging history, so `history.json` reads "[]" whether
        # or not publish() ran: asserting on it alone is a fixed point, and this
        # test was green against a driver that published the storage half.
        self.assertEqual(self.repo_state(), before)
        self.assertEqual((self.repo / "tools" / "publish" / "history.json").read_text(), "[]\n")

    def test_the_cleanup_runs_even_when_the_capture_fails(self):
        # Red against cleanup on the success path only. A failed capture that
        # litters leaves two multi-gigabyte images behind, and a capture that
        # litters is a capture nobody runs twice.
        recorder = Recorder()
        original = recorder.run

        def blow_up(step):
            if step.label == "capture.engines":
                recorder.steps.append(step)
                raise RuntimeError("the sweep died")
            return original(step)

        recorder.run = blow_up
        with self.assertRaises(RuntimeError):
            self.drive(recorder)
        labels = [s.label for s in recorder.steps]
        for needed in (
            "cleanup.builder.storage",
            "cleanup.image.storage",
            "cleanup.scratch",
            "cleanup.list.images",
            "cleanup.list.scratch",
        ):
            self.assertIn(needed, labels)

    def test_cleanup_can_name_the_build_container_it_has_to_stop(self):
        # Red against an unnamed `docker run`. Interrupting the driver does not
        # stop the build: SIGINT reaches the Python process, subprocess.run
        # raises, and the ssh child and everything downstream keep going, so the
        # machine keeps compiling with nobody attached until somebody removes the
        # container by hand. I watched that happen. Cleanup can only remove a
        # container it can name.
        recorder = Recorder()
        self.drive(recorder)
        sent = {s.label: (s.remote or "") for s in recorder.steps}
        for family in ("storage", "engines"):
            expected = f"viprs-build-{family}-unit-test"
            self.assertIn(expected, sent[f"build.{family}"], "the build container is named")
            self.assertEqual(
                sent[f"cleanup.builder.{family}"],
                f"docker rm -f {expected}",
                "and cleanup removes it",
            )

    def test_a_hard_failure_still_writes_the_summary(self):
        # Red against letting the exception out with nothing written. The first
        # end-to-end run died after both images were built and left a traceback
        # and no
        # record: which families had been captured, whether the machine was
        # clean, what the revisions were, all of it only in the scrollback.
        recorder = Recorder()
        original = recorder.run

        def blow_up(step):
            if step.label == "capture.storage":
                recorder.steps.append(step)
                raise RuntimeError("the sweep died")
            return original(step)

        recorder.run = blow_up
        with self.assertRaises(RuntimeError):
            self.drive(recorder)
        summary = json.loads(self.summary_path.read_text())
        self.assertIn("the sweep died", summary["failed"])
        self.assertFalse(summary["published"])
        self.assertIn("nasLeftAsFound", summary, "and it says whether the machine is clean")

    def test_a_non_publishable_profile_is_refused_before_anything_is_measured(self):
        # Red against leaving the profile to the importer. `ci` archives
        # indistinguishably from a calibrated sweep, so the only thing that
        # turns it away is the importer, and finding out after forty minutes of
        # measuring is the expensive way to learn it.
        recorder = Recorder()
        code = self.drive(recorder, extra=["--profile", "ci"])
        self.assertEqual(code, 1)
        self.assertEqual(recorder.steps, [], "nothing ran")
        summary = json.loads(self.summary_path.read_text())
        self.assertTrue(any("not publishable" in r for r in summary["refusals"]))
        self.assertTrue(any("full" in r for r in summary["refusals"]), "the allowed set is named")

    def test_the_nas_is_reported_by_listing_rather_than_asserted(self):
        # Red against printing "the NAS is clean". The claim in the issue is
        # that the machine is left as it was found, verified by listing, so the
        # listing's own answer is what the summary carries.
        self.drive(Recorder())
        left = json.loads(self.summary_path.read_text())["nasLeftAsFound"]
        # What is left is named, and what is left OF THIS RUN is the answer. A
        # count cannot give it: this machine carries other people's jobs, and a
        # run that reported "1 scratch tree remaining" would be reporting on
        # somebody else while saying nothing about itself.
        # Only the viprs images, so the machine's own hundred unrelated ones do
        # not drown the answer.
        self.assertEqual(left["images"], ["viprs-nas-storage:someone-else"])
        self.assertEqual(left["scratchTrees"], ["someone-else"])
        self.assertEqual(left["mine"], [])

    def test_a_leftover_of_this_run_is_named_rather_than_counted_away(self):
        # Red against `mine = []`, and the reason this test exists separately:
        # in the case above nothing of the run is left, so an implementation that
        # never looks still answers correctly. A guard whose fixture cannot
        # produce the thing it is guarding against is a fixed point, not a test.
        recorder = Recorder()
        original = recorder.run

        def leave_something(step):
            if step.label == "cleanup.list.images":
                recorder.steps.append(step)
                return capture.Result(
                    step,
                    0,
                    "viprs-nas-storage:unit-test\nviprs-nas-storage:someone-else\n",
                    "",
                )
            if step.label == "cleanup.list.scratch":
                recorder.steps.append(step)
                return capture.Result(step, 0, "unit-test\n", "")
            return original(step)

        recorder.run = leave_something
        self.drive(recorder)
        left = json.loads(self.summary_path.read_text())["nasLeftAsFound"]
        self.assertEqual(left["mine"], ["unit-test", "viprs-nas-storage:unit-test"])
        self.assertIn("viprs-nas-storage:someone-else", left["images"])


if __name__ == "__main__":
    unittest.main()
