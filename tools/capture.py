#!/usr/bin/env python3
"""Capture both benchmark families on the NAS and publish the result into this
repository.

    tools/capture.py [--profile full|xl] [--families storage,engines]
                     [--repo <dir>] [--out <dir>] [--json <path>]
                     [--plan] [--keep] [--commit] [--nas user@host]

One command: stage clean checkouts, push them to the NAS with their git
history, build the images there through the socket-mounted driver, settle,
capture, retrieve, check, archive, verify, import, and leave the machine as it
was found. What it publishes is the sealed document under `archive/<family>/`,
that family's `archive/<family>/index.json`, and the one shared
`tools/publish/history.json` the entry is derived into. libviprs-org reads all
of those out of this repository at a pinned revision; nothing is pushed to the
site from here.

This driver runs on the Mac. It is an orchestrator, exactly as
`tools/capture-nas.sh` is, and it does not reimplement anything the chain
already has: `storage-aggregate --check`, `--archive`, `--verify` and
`tools/publish/import-run.mjs` are the gates, and this calls them.

-----------------------------------------------------------------------------
NOTHING THIS RUN CAUSES TO EXECUTE ON THE NAS RUNS OUTSIDE A CONTAINER

Only three things touch that machine: ssh, docker, and writes into the one
bind-mounted scratch directory under ~/workspace/. Unpacking a tarball, reading
the load, listing what is left and running the aggregator are all container
work, including the ones that do not feel like work. `tools/capture-nas.test.mjs`
walks `--plan` and holds this, so the guard fails rather than the habit lapsing.

-----------------------------------------------------------------------------
THE TRAPS, CARRIED FORWARD FROM tools/capture-nas.sh RATHER THAN REDISCOVERED

 1. The tree is pushed WITH `.git`. `tools/nas.sh` excludes it, which is right
    for every other job on that machine and wrong for this one: without it the
    provenance layer cannot resolve a commit and the aggregator refuses the run.
 2. There is no wait-for-idle. The NAS carries resident backupd and postgres
    containers and its load floor is 1.4 to 2.2 on six cores with all of them at
    0% CPU, so an absolute threshold spins for the full timeout every time. The
    settle threshold is half the core count, which is reachable there and well
    under the ncpu ceiling the harness refuses at.
 3. Retrieval is `ssh cat` through a container, never scp. scp fails there with
    "No such file or directory" on a file that plainly exists, because the SFTP
    subsystem is not available.
 4. The scratch tree is removed from INSIDE a container mounting its parent. The
    builds run as root and leave files the host account cannot delete, and
    docker creates a missing bind-mount source as root, so mounting the scratch
    root itself is what makes it unwritable next time.
 5. Cleanup runs on the way out however this exits, so a failed capture does not
    leave two multi-gigabyte images and 30 MB of source behind.
 6. The driver image is BUILT, never assumed. It is local, on no registry, so a
    missing one sends docker to a pull that cannot succeed and tells the reader
    to `docker login`, which is the wrong trail entirely.
 7. Six cores decline every T=8 rung, so an x86_64 run carries fewer storage
    cells than an eight-core arm64 one. The summary says so rather than leaving
    a reader to wonder why two runs of one profile have different cell counts.
 8. Every directory in the scratch tree is created INSIDE a container: the root,
    `out`, `scratch` and the archive staging. Leaving one to docker, which makes
    a missing bind-mount source itself, reaches the same end state with no record
    of who did it, and it is the mechanism this trap and trap 4 are both about. The shell script creates the root with a host-side
    `mkdir -p` and gets away with it because it only ever creates the root. This
    driver pushes a second tree beside the first, and by then the root is not the
    host account's: `tar -xf` of an archive whose top entry is `.` restores that
    entry's ownership onto the destination, the tar is made on a Mac at uid 501
    and the NAS account is uid 1001. So the second `mkdir -p` dies with
    "Permission denied" on a directory that plainly exists. The first end-to-end
    run failed there, nine minutes in, with both images already built and the
    whole capture still ahead of it.
 9. The build driver's container carries a NAME. Interrupting this script does
    not stop the build: SIGINT reaches the Python process, `subprocess.run`
    raises, and the ssh child and everything downstream of it carry on, so the
    machine keeps compiling with nobody attached. Cleanup can only remove a
    container it can name, and an unnamed `docker run` is a random two-word
    container whose only identification is what it happens to be running.

-----------------------------------------------------------------------------
WHAT IS MEASURED IS MAIN, NOT THIS WORKING TREE

Both repositories are cloned fresh from GitHub. A run measured against an edited
tree stamps `provenance.dirty` and is refused, and more to the point a number
that cannot say which commit produced it is not evidence. So a change to the
harness has to be on `main` before it can be measured, and the document records
that commit rather than whatever happens to be checked out here.

-----------------------------------------------------------------------------
WHY NOTHING IS WRITTEN UNTIL EVERYTHING HAS PASSED

The whole chain runs against a staging copy of `archive/` and
`tools/publish/history.json`, and the repository is only touched once the
import has accepted the run. That is structural rather than careful: a
contended, emulated, debug-built, unarchived, digest-broken or unpublishable
capture cannot reach the repository at all, so there is no state in which the
page shows a number with a caveat attached. A refused capture leaves
`git status` clean and prints every reason.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path

# The families this driver knows how to capture, and the binary each one runs.
FAMILIES = ("storage", "engines")

# The image every container-only errand on the NAS runs in. Small, and already
# on the machine because the shell script uses the same one.
UTIL_IMAGE = "alpine:3.20"

# The image that holds the host's docker socket and a docker CLI, so a build
# driver runs in a container without docker-in-docker. `docker build` is a
# client-side call; this is what makes "everything in a container" reachable for
# a thing whose whole job is to drive docker.
#
# docker-buildx-plugin is not optional. On Docker 29 `docker build` IS
# `docker buildx build`, so a CLI-only image dies the moment a BuildKit flag is
# passed, with exit 125 and no message worth reading.
DRIVER_IMAGE = "nas-driver:latest"
DRIVER_DOCKERFILE = """FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \\
        python3 git ca-certificates curl gnupg \\
    && install -m 0755 -d /etc/apt/keyrings \\
    && curl -fsSL https://download.docker.com/linux/debian/gpg \\
        -o /etc/apt/keyrings/docker.asc \\
    && chmod a+r /etc/apt/keyrings/docker.asc \\
    && echo 'deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian bookworm stable' \\
        > /etc/apt/sources.list.d/docker.list \\
    && apt-get update && apt-get install -y --no-install-recommends \\
        docker-ce-cli docker-buildx-plugin \\
    && rm -rf /var/lib/apt/lists/*
"""

# Local container work is arm64 and says so. This machine is Apple Silicon, an
# amd64 container here is Rosetta, and a platform left to default is a platform
# nobody wrote down.
LOCAL_PLATFORM = "linux/arm64"
NODE_IMAGE = "node:22-alpine"

# The NAS is native x86_64, so its containers are amd64 and say so too.
NAS_PLATFORM = "linux/amd64"

CLONE_DEPTH = "50"
REPOS = {
    "libviprs-bench": "https://github.com/libviprs/libviprs-bench.git",
    "libviprs": "https://github.com/libviprs/libviprs.git",
}

# How long the driver waits for the machine to go quiet before it gives up, and
# gives up meaning REFUSES rather than measures anyway.
#
# 300 seconds was the shell script's budget and it is not enough. Two image
# builds leave this box at a load that five minutes does not shed: a run of the
# shell script measured the first family at 1-minute load 10.62 on six cores with
# 0 of 539 cells quiet, while the second family, going straight afterwards,
# settled at 2.69 after 100 seconds and came out 48% quiet. The first document
# was worthless and the ten minutes that produced it were spent knowing it would
# be refused.
#
# So the budget is twenty minutes, which covers the decay from a build, and
# running out of it is fatal. "Capture anyway and record the load" is the right
# call for a tool a person runs and watches, because the load is in the document
# and the publish gate refuses it. It is the wrong call for an automated
# publishing path: the run costs real NAS time and its output cannot be
# published either way, so the useful outcome is "the machine did not go quiet,
# nothing was captured" rather than a refused document.
SETTLE_TIMEOUT_S = 1200
SETTLE_POLL_S = 20


class Refused(Exception):
    """The run may not be published, and every reason is in `reasons`.

    Not an error: it is an answer. Exit 1, the reasons printed, and nothing
    written.
    """

    def __init__(self, reasons: list[str]):
        super().__init__("; ".join(reasons))
        self.reasons = reasons


@dataclass
class Step:
    """One command this driver causes to run.

    `where` is 'nas' or 'local'. A NAS step carries the script ssh hands to the
    remote shell in `remote`, which is what the containment guard walks. A local
    step carries `argv`.
    """

    label: str
    where: str
    remote: str | None = None
    argv: list[str] | None = None
    stdin_path: str | None = None
    # The head of what would be piped in, read when the step is recorded. The
    # path alone is no use to a reader or to the guard, because it is in a
    # temporary directory this process deletes on the way out, and a plan that
    # names a file nobody can open cannot be checked.
    stdin_head: str = ""
    # Steps whose non-zero exit is an answer rather than a failure: the
    # aggregator's refusal and the importer's are both exit 1 on purpose.
    allow_failure: bool = False


@dataclass
class Result:
    step: Step
    code: int
    stdout: str
    stderr: str


@dataclass
class Executor:
    """Runs steps. `plan` records them instead, so the guard can walk every
    command without a machine anywhere near it."""

    nas: str
    plan: bool = False
    echo: bool = True
    steps: list[Step] = field(default_factory=list)
    # Canned stdout for plan mode, keyed by label prefix. Plan mode walks the
    # SAME code path a real run walks, so anything the driver reads back has to
    # have a stand-in or the walk stops at the first branch on output. These are
    # obvious fakes on purpose: a reader who sees one in a real transcript knows
    # immediately that the run was a plan.
    canned: dict[str, str] = field(
        default_factory=lambda: {
            "settle.cores": "6",
            "settle.load": "0.00",
            "load.at-start": "0.00 0.00 0.00",
            "archive": "archived /out/x.json as PLANNED-RUN-ID (sha256:planned)",
            "retrieve.document": "{}",
            "retrieve.index": "[]",
            "cleanup.containers": "",
            "cleanup.list.images": "viprs-nas-storage:someone-else",
            "cleanup.list.scratch": "someone-else",
        }
    )

    def ssh_argv(self, remote: str) -> list[str]:
        return ["ssh", "-o", "BatchMode=yes", "-o", "LogLevel=ERROR", self.nas, remote]

    def run(self, step: Step) -> Result:
        if step.stdin_path and Path(step.stdin_path).exists():
            step.stdin_head = Path(step.stdin_path).read_text(errors="replace")[:4000]
        self.steps.append(step)
        if self.echo and not self.plan:
            print(f"== {step.label}", flush=True)
        if self.plan:
            for prefix, out in self.canned.items():
                if step.label.startswith(prefix) or step.label.endswith(prefix):
                    return Result(step, 0, out, "")
            return Result(step, 0, "", "")
        argv = step.argv if step.where == "local" else self.ssh_argv(step.remote or "")
        stdin = open(step.stdin_path, "rb") if step.stdin_path else None
        try:
            proc = subprocess.run(
                argv,
                stdin=stdin,
                capture_output=True,
                text=True,
                errors="replace",
            )
        finally:
            if stdin:
                stdin.close()
        result = Result(step, proc.returncode, proc.stdout, proc.stderr)
        if proc.returncode != 0 and not step.allow_failure:
            sys.stderr.write(proc.stdout)
            sys.stderr.write(proc.stderr)
            raise RuntimeError(f"{step.label} failed with exit {proc.returncode}")
        return result

    def nas_step(self, label: str, remote: str, **kw) -> Result:
        return self.run(Step(label=label, where="nas", remote=remote, **kw))

    def local_step(self, label: str, argv: list[str], **kw) -> Result:
        return self.run(Step(label=label, where="local", argv=argv, **kw))


# --------------------------------------------------------------------------
# the pieces that are worth testing on their own
# --------------------------------------------------------------------------


def settle_threshold(cores: int) -> float:
    """The load this driver waits for, as a fraction of the core count.

    Half the cores. NOT an absolute number: this machine's floor is 1.4 to 2.2
    with every resident container at 0% CPU, so a threshold of 1.2 can never be
    met and the wait becomes a fixed five-minute delay that teaches nobody
    anything. Half of six is three, which is reachable there and still well
    under the ncpu ceiling the harness itself refuses at.
    """
    if cores <= 0:
        raise ValueError("core count must be positive; a zero-core host is a read that failed")
    return cores / 2


def publishable_profiles(repo: Path) -> list[str]:
    """The profiles the importer will publish, read from the config the importer
    reads.

    Read rather than restated. `ci` proves the harness runs, is never a
    measurement, and archives indistinguishably from a calibrated sweep, so the
    only thing that keeps it off the page is this set, and a second copy of it
    here is a second thing to forget. An empty or missing set is a configuration
    fault and not a verdict on a profile: with nothing in that set no profile
    could pass, so saying "full is not publishable ()" would be nonsense dressed
    as a gate. That is libviprs-bench #82 and the shape that let it hide is not
    repeated here.
    """
    candidates = [repo / "tools" / "contract" / "config.json", repo / "tools" / "publish" / "config.json"]
    for path in candidates:
        if not path.exists():
            continue
        producer = json.loads(path.read_text()).get("producer", {})
        values = list(producer.get("publishableProfiles") or [])
        if not values:
            raise Refused(
                [
                    f"{path} defines producer.publishableProfiles as "
                    f"{json.dumps(producer.get('publishableProfiles'))}, so no profile could "
                    "pass this check and nothing about the profile you asked for reached it. "
                    "That is a fault in the config, not a verdict on the run."
                ]
            )
        return values
    raise Refused(
        [
            "no importer config at "
            + " or ".join(str(p) for p in candidates)
            + ", so the set of publishable profiles is unknown and this driver will not guess it"
        ]
    )


def parse_run_id(archive_stdout: str) -> str:
    """The run id `storage-aggregate --archive` filed the document under.

    It prints `archived <path> as <runId> (<digest>)`, or, when the same run is
    already filed, `<path> is already archived as <runId>, and its digest still
    matches`. Both forms are read, because re-running a capture whose import
    failed has to be able to get to the import again.
    """
    for line in archive_stdout.splitlines():
        parts = line.split()
        if line.startswith("archived ") and len(parts) >= 4 and parts[2] == "as":
            return parts[3]
        if " is already archived as " in line:
            return line.split(" is already archived as ", 1)[1].split(",")[0].strip()
    raise RuntimeError(
        "storage-aggregate --archive printed no run id, so there is no archived file to "
        f"verify or import:\n{archive_stdout}"
    )


def cell_summary(document: dict) -> dict:
    """What the run measured, per outcome, with the core count beside it.

    The core count is in here because it is the honest explanation for a cell
    count that differs between two runs of the same profile: six cores decline
    every T=8 rung, so an x86_64 run carries fewer storage cells than an
    eight-core arm64 one, and a reader comparing the two should be told rather
    than left to work it out.
    """
    cells = document.get("cells") or []
    outcomes: dict[str, int] = {}
    for cell in cells:
        outcomes[str(cell.get("outcome"))] = outcomes.get(str(cell.get("outcome")), 0) + 1
    with_load = [c for c in cells if isinstance((c.get("machineLoad") or {}).get("quiet"), bool)]
    noisy = [c for c in with_load if c["machineLoad"]["quiet"] is False]
    provenance = document.get("provenance") or {}
    ncpu = provenance.get("ncpu")
    return {
        "cells": len(cells),
        "byOutcome": outcomes,
        "ncpu": ncpu,
        "noisyCells": len(noisy),
        "cellsWithLoad": len(with_load),
        "fewerCellsThanAnEightCoreHost": bool(isinstance(ncpu, int) and ncpu < 8),
        "emulated": provenance.get("emulated"),
        "arch": provenance.get("arch"),
        "buildProfile": (provenance.get("node") or {}).get("buildProfile"),
    }


# --------------------------------------------------------------------------
# the NAS half
# --------------------------------------------------------------------------


def scratch_root(name: str) -> str:
    return f"$HOME/workspace/nas-work/{name}"


def nas_container(
    image: str,
    command: str,
    mounts: tuple[tuple[str, str], ...] = (),
    interactive: bool = False,
    env: tuple[tuple[str, str], ...] = (),
    workdir: str | None = None,
    container: str | None = None,
) -> str:
    """One container invocation on the NAS, spelled out.

    Every errand goes through here, which is the mechanical half of the rule:
    there is one place that can produce a remote command, it always starts
    `docker run`, and a step that wanted to skip it would have to be written
    somewhere the guard is looking.
    """
    parts = ["docker run --rm"]
    if container:
        # A name, so cleanup can reach it. Without one the build driver is a
        # random two-word container and the only way to find it is to read what
        # it is running, which cleanup cannot do reliably and a person has to.
        parts.append(f"--name {container}")
    if interactive:
        parts.append("-i")
    parts.append(f"--platform {NAS_PLATFORM}")
    for host, inside in mounts:
        parts.append(f"-v {host}:{inside}")
    for key, value in env:
        parts.append(f"-e {key}={value}")
    if workdir:
        parts.append(f"-w {workdir}")
    parts.append(image)
    parts.append(command)
    return " ".join(parts)


def push_tree(ex: Executor, name: str, tarball: Path, dest: str) -> None:
    """Send a tar to the NAS and unpack it inside a container.

    The unpack is the trap the rule names explicitly, and it is the mistake the
    first version of the shell script made: a host-side `tar -xf` does not feel
    like work and executes on the machine all the same.

    The mkdir runs in a container too, and that is trap 8 in the header rather
    than tidiness. The shell script creates the scratch root with a host-side
    `mkdir -p` and gets away with it because it only ever creates the root. This
    driver pushes a second tree beside the first one, and by then the scratch
    root is no longer the host account's: `tar -xf` extracting an archive whose
    top entry is `.` restores that entry's ownership onto the destination, and
    the tar is made on a Mac where the uid is 501 while the NAS account is 1001.
    So the first push silently hands the directory to a uid nobody on that
    machine is, and the second `mkdir -p` dies with "Permission denied" on a
    directory that plainly exists. The first end-to-end run failed exactly there,
    nine minutes in, with both images built and the whole capture still ahead.

    Doing it in a container removes the special case as well as the failure:
    every command this driver sends to the NAS is now a `docker` command, with no
    exception at all, and `tools/capture-nas.test.mjs` holds the driver to that
    stronger rule than the one it holds the script to. Root ownership is not a
    problem here because nothing on the host ever writes into the tree: the
    containers do, and cleanup removes it from inside a container.
    """
    leaf = dest.rsplit("/", 1)[-1]
    # $HOME/workspace is the mount and the path inside is derived from `dest`,
    # so the container creates it rather than docker creating a missing
    # bind-mount source, which is the same thing with no record of who did it.
    inside = dest.replace("$HOME/workspace", "/ws")
    ex.nas_step(
        f"push.mkdir.{leaf}",
        nas_container(
            UTIL_IMAGE,
            f"mkdir -p {inside}",
            mounts=(("$HOME/workspace", "/ws"),),
        ),
    )
    ex.nas_step(
        f"push.untar.{leaf}",
        nas_container(
            UTIL_IMAGE,
            "tar -xf - -C /dest",
            mounts=((dest, "/dest"),),
            interactive=True,
        ),
        stdin_path=str(tarball),
    )


def build_driver_image(ex: Executor, dockerfile: Path) -> None:
    """Build the socket-mounted driver rather than assuming it.

    It is a local image on no registry. A missing one sends docker to a pull
    that cannot succeed and reports an authentication problem, which is the
    wrong trail entirely. The daemon caches it, so rebuilding each run costs
    nothing after the first.

    The Dockerfile goes over stdin rather than in a heredoc. A heredoc would make
    this remote command a shell script with a `>` redirect in it, and the rule
    the guard holds is that every command this driver sends to the NAS is one
    docker invocation and nothing else. Feeding `docker build -` its input the
    way the tar push is fed keeps it to that.
    """
    ex.nas_step(
        "driver.image",
        f"docker build -q -t {DRIVER_IMAGE} -",
        stdin_path=str(dockerfile),
    )


def build_container(name: str, family: str) -> str:
    """The name the build driver's container carries, so cleanup can find it.

    This is not tidiness. Interrupting the driver on this machine does not stop
    the build: SIGINT reaches the Python process, `subprocess.run` raises, and
    the ssh child and everything downstream of it keep going, so the NAS carries
    on compiling with nobody attached until somebody removes the container by
    hand. Cleanup can remove a container it can name, and it could not name this
    one.
    """
    return f"viprs-build-{family}-{name}"


def build_family_image(ex: Executor, name: str, family: str) -> str:
    tag = f"viprs-nas-{family}:{name}"
    root = scratch_root(name)
    ex.nas_step(
        f"build.{family}",
        nas_container(
            DRIVER_IMAGE,
            f"sh -c 'docker build --platform {NAS_PLATFORM} -f libviprs-bench/Dockerfile "
            f"--target {family} -t {tag} . >/dev/null'",
            mounts=(("/var/run/docker.sock", "/var/run/docker.sock"), (root, "/work")),
            workdir="/work",
            container=build_container(name, family),
        ),
    )
    return tag


def settle(ex: Executor, family: str, timeout_s: int = SETTLE_TIMEOUT_S) -> dict:
    """Wait for the machine to go quiet, and refuse if it does not.

    The first end-to-end run of the shell script failed its own publish gate for
    this: the image builds left the box hot, storage opened at 1-minute load 7.77
    on six cores, and the importer refused 284 of 526 measured cells for
    `machineLoad.quiet: false`. The capture was contending with itself.

    Two things changed after that. The threshold is relative to core count, not
    absolute, because this machine's floor is 1.4 to 2.2 with every resident
    container at 0% CPU and an absolute 1.2 can never be met. And running out of
    the budget REFUSES. The shell script prints a warning and captures anyway,
    which is honest, since the load is recorded and the publish gate turns the
    run away; it is also ten minutes of measuring spent on a document whose only
    use is to be thrown out, and on a path that publishes without a person
    watching that is the wrong trade.

    Every reading is printed as it is taken, so the transcript shows the decay
    and the load the family actually started at, rather than leaving that in the
    document for whoever reads the refusal afterwards.
    """
    cores_out = ex.nas_step("settle.cores", nas_container(UTIL_IMAGE, "nproc")).stdout
    try:
        cores = int(cores_out.strip() or "6")
    except ValueError:
        cores = 6
    threshold = settle_threshold(cores)
    waited = 0
    readings: list[float] = []
    while True:
        raw = ex.nas_step(
            "settle.load",
            nas_container(UTIL_IMAGE, "sh -c 'cut -d\" \" -f1 /proc/loadavg'"),
        ).stdout
        try:
            load = float(raw.strip())
        except ValueError:
            load = float("inf")
        readings.append(load)
        print(f"  settling {family}: load {load} after {waited}s, waiting for under {threshold}")
        if load < threshold:
            print(
                f"  {family} STARTS AT LOAD {load} on {cores} cores, under the {threshold} this "
                f"driver waits for, after {waited}s"
            )
            return {
                "cores": cores,
                "threshold": threshold,
                "startedAtLoad": load,
                "waitedSeconds": waited,
                "settled": True,
            }
        # The wait goes AFTER the decision to keep waiting, so a budget that is
        # already spent does not buy one more sleep before it gives up.
        if ex.plan or waited >= timeout_s:
            break
        time.sleep(min(SETTLE_POLL_S, timeout_s - waited))
        waited += SETTLE_POLL_S
    raise Refused(
        [
            f"the machine did not go quiet for {family}: after {waited}s the 1-minute load was "
            f"{readings[-1] if readings else 'unread'} on {cores} cores and this driver waits for "
            f"under {threshold}. Readings: {', '.join(str(r) for r in readings[-10:])}. Nothing "
            "was captured, because a sweep taken at this load produces a document whose cells "
            "are not quiet, and the importer refuses a run whose typical cell was not quiet. "
            "Measuring anyway would cost ten minutes of this machine and produce a file to throw "
            "away. Wait for whatever else is running to finish, or raise --settle-timeout."
        ]
    )


def cleanup(ex: Executor, name: str) -> dict:
    """Remove what this run made, then report what is left by listing it.

    `nas-driver:latest` is deliberately kept: other jobs on that machine use it,
    tearing down something shared because this run happened to rebuild it would
    be rude, and it is a cached layer the next run reuses. The two images this
    run built are ours alone and do go, and so does the build driver's own
    container, which outlives an interrupted run otherwise.

    One docker invocation per step, with the filtering done here rather than in a
    pipeline on the machine. The shell script pipes `docker images` into `grep`
    and `grep -c`, which is two host binaries doing work, and it says so in a
    comment drawing the boundary there. This draws it tighter: every command this
    driver sends is one docker command, so the guard can hold a rule with no
    exception in it and no substring test that a `;` can walk past.
    """
    root = scratch_root(name)
    for family in FAMILIES:
        ex.nas_step(
            f"cleanup.builder.{family}",
            f"docker rm -f {build_container(name, family)}",
            allow_failure=True,
        )
    for family in FAMILIES:
        running = ex.nas_step(
            f"cleanup.containers.{family}",
            f"docker ps -aq --filter ancestor=viprs-nas-{family}:{name}",
            allow_failure=True,
        ).stdout.split()
        for container_id in running:
            ex.nas_step(
                f"cleanup.container.{family}.{container_id}",
                f"docker rm -f {container_id}",
                allow_failure=True,
            )
        ex.nas_step(
            f"cleanup.image.{family}",
            f"docker rmi -f viprs-nas-{family}:{name}",
            allow_failure=True,
        )
    # The parent is mounted, never the scratch root: docker creates a missing
    # bind-mount source as root, and mounting the root itself is what leaves it
    # unwritable for the next run.
    ex.nas_step(
        "cleanup.scratch",
        nas_container(
            UTIL_IMAGE,
            f"rm -rf /ws/nas-work/{name}",
            mounts=(("$HOME/workspace", "/ws"),),
        ),
        allow_failure=True,
    )
    # Report what is left by listing it, never by asserting the machine is
    # clean. Names and not counts: a count answers "is anything here", and the
    # question is "is anything of MINE here", which a count cannot answer on a
    # machine other jobs also use.
    images = [
        line.strip()
        for line in ex.nas_step(
            "cleanup.list.images",
            "docker images --format '{{.Repository}}:{{.Tag}}'",
            allow_failure=True,
        ).stdout.splitlines()
        if "viprs-nas" in line
    ]
    trees = [
        line.strip()
        for line in ex.nas_step(
            "cleanup.list.scratch",
            nas_container(
                UTIL_IMAGE,
                "ls /ws/nas-work",
                mounts=(("$HOME/workspace", "/ws"),),
            ),
            allow_failure=True,
        ).stdout.splitlines()
        if line.strip()
    ]
    left = {"images": images, "scratchTrees": trees}
    left["mine"] = sorted(item for item in images + trees if name in item)
    for kind in ("images", "scratchTrees"):
        print(f"  {kind} left on the machine: {', '.join(left[kind]) or 'none'}")
    print(
        "  nothing of this run's is left"
        if not left["mine"]
        else f"  STILL HERE, and it is this run's: {', '.join(left['mine'])}"
    )
    return left


# --------------------------------------------------------------------------
# the run
# --------------------------------------------------------------------------


def stage_checkouts(ex: Executor, stage: Path) -> dict[str, str]:
    """Clone both repositories fresh.

    A clean checkout and not this working tree: the harness stamps
    `provenance.dirty` from the tree it was built from, and a dirty one cannot
    say what produced the numbers. Cloning is also why the archive staging below
    is a separate directory rather than an edit to this tree, which would make
    every cell of the run dirty.
    """
    revisions = {}
    for name, url in REPOS.items():
        target = stage / name
        ex.local_step(
            f"stage.clone.{name}",
            ["git", "clone", "--quiet", "--depth", CLONE_DEPTH, url, str(target)],
        )
        revisions[name] = (
            ex.local_step(
                f"stage.rev.{name}", ["git", "-C", str(target), "rev-parse", "HEAD"]
            ).stdout.strip()
            or "PLANNED"
        )
    return revisions


def make_tarball(ex: Executor, label: str, source: Path, out: Path, excludes: tuple[str, ...]) -> Path:
    """Tar a directory for the push.

    `.git` is NOT excluded, and that is the point: `tools/nas.sh` excludes it
    for every other job and the provenance layer cannot resolve a commit without
    it, so the aggregator refuses the run. Build output is excluded because it is
    30 MB of nothing the image wants.
    """
    argv = ["tar", "-cf", str(out), "-C", str(source)]
    for pattern in excludes:
        argv.append(f"--exclude={pattern}")
    argv.append(".")
    ex.local_step(label, argv)
    if ex.plan and not out.exists():
        out.write_bytes(b"")
    return out


def capture_family(ex: Executor, name: str, family: str, profile: str, out_dir: Path) -> Path:
    """Measure one family and bring its document back."""
    root = scratch_root(name)
    tag = f"viprs-nas-{family}:{name}"
    # Printed, not just run. The shell script's equivalent line exists "only so
    # the transcript shows it too", and here the Executor captures output, so a
    # step whose result is never read is a container started for nothing.
    at_start = ex.nas_step(
        f"load.at-start.{family}",
        nas_container(UTIL_IMAGE, "cut -d' ' -f1-3 /proc/loadavg"),
        allow_failure=True,
    ).stdout.strip()
    print(f"  load at start: {at_start or 'unread'}")
    ex.nas_step(
        f"capture.{family}",
        nas_container(
            tag,
            f"/src/libviprs-bench/target/release/{family} "
            f"--family {family} --profile {profile} --out /out/{family}-x86.json",
            mounts=((f"{root}/out", "/out"), (f"{root}/scratch", "/scratch")),
            env=(("TMPDIR", "/scratch"),),
        ),
    )
    # `ssh cat` through a container, never scp: scp fails on that host with "No
    # such file or directory" on a file that plainly exists, because the SFTP
    # subsystem is not there.
    document = ex.nas_step(
        f"retrieve.document.{family}",
        nas_container(
            UTIL_IMAGE,
            f"cat /out/{family}-x86.json",
            mounts=((f"{root}/out", "/out"),),
        ),
    ).stdout
    path = out_dir / f"{family}-x86.json"
    path.write_text(document)
    print(f"  retrieved {path} ({len(document)} bytes)")
    return path


def gate_refused(result: Result, family: str, what: str, refusals: list[str]) -> None:
    """Record every reason a gate gave, and one of our own when it gave none.

    A gate that exits non-zero with both streams empty used to append nothing.
    The family was then skipped with no sentence anywhere saying so, and because
    the publish rule was keyed on the refusal list being empty, the OTHER family
    went to the repository and the run reported success. An aggregator killed by
    the OOM killer exits 137 and says nothing, which on a six-core machine
    running a full sweep is not a hypothetical.

    So a silent non-zero exit is itself a refusal, and it says that is what it
    is, because "the gate failed and told me nothing" is a different problem from
    "the run was refused" and the reader needs to know which one they have.
    """
    said = False
    for line in (result.stderr or result.stdout).splitlines():
        if line.strip():
            refusals.append(f"{family}: {what} {line.strip()}")
            said = True
    if not said:
        refusals.append(
            f"{family}: {what} exited {result.code} and printed nothing at all, so there is no "
            "reason to report. That is a failure of the gate rather than a verdict on the run, "
            "and it is refused the same way: a step that cannot say why it said no is not "
            "evidence that anything is fine."
        )


def archive_family(
    ex: Executor, name: str, family: str, staging: Path, refusals: list[str]
) -> str | None:
    """Check, archive and verify one family's document, on the NAS, in the image
    that measured it.

    `--check` first, so a document that is going to be refused is refused before
    the archive is touched and the reasons arrive on their own rather than
    inside a failed `--archive`. `--root` is the family's directory and not its
    parent: `archive_text` writes `<root>/<runId>.json` and `<root>/index.json`
    straight into what it is given, so `--root archive` files an engines run
    beside the storage ones in a directory whose index is not theirs.
    """
    root = scratch_root(name)
    tag = f"viprs-nas-{family}:{name}"
    aggregate = "/src/libviprs-bench/target/release/storage-aggregate"
    document = f"/out/{family}-x86.json"

    check = ex.nas_step(
        f"check.{family}",
        nas_container(
            tag,
            f"{aggregate} --check {document}",
            mounts=((f"{root}/out", "/out"),),
        ),
        allow_failure=True,
    )
    if check.code != 0:
        gate_refused(check, family, "`storage-aggregate --check`", refusals)
        return None

    archived = ex.nas_step(
        f"archive.{family}",
        nas_container(
            tag,
            f"{aggregate} --archive {document} --root /archive/{family}",
            mounts=((f"{root}/out", "/out"), (f"{root}/archive", "/archive")),
        ),
        allow_failure=True,
    )
    if archived.code != 0:
        gate_refused(archived, family, "`storage-aggregate --archive`", refusals)
        return None
    run_id = parse_run_id(archived.stdout)

    verified = ex.nas_step(
        f"verify.{family}",
        nas_container(
            tag,
            f"{aggregate} --verify /archive/{family}/{run_id}.json",
            mounts=((f"{root}/archive", "/archive"),),
        ),
        allow_failure=True,
    )
    if verified.code != 0:
        gate_refused(
            verified,
            family,
            "the archived document does not verify, and `storage-aggregate --verify` says",
            refusals,
        )
        return None

    sealed = ex.nas_step(
        f"retrieve.document.archived.{family}",
        nas_container(
            UTIL_IMAGE,
            f"cat /archive/{family}/{run_id}.json",
            mounts=((f"{root}/archive", "/archive"),),
        ),
    ).stdout
    index = ex.nas_step(
        f"retrieve.index.{family}",
        nas_container(
            UTIL_IMAGE,
            f"cat /archive/{family}/index.json",
            mounts=((f"{root}/archive", "/archive"),),
        ),
    ).stdout
    family_dir = staging / "archive" / family
    family_dir.mkdir(parents=True, exist_ok=True)
    (family_dir / f"{run_id}.json").write_text(sealed)
    (family_dir / "index.json").write_text(index)
    return run_id


def import_family(
    ex: Executor, repo: Path, staging: Path, family: str, run_id: str, refusals: list[str]
) -> bool:
    """Run the importer, in a container, against the staging history.

    Not reimplemented and not second-guessed. It recomputes the four digests
    rather than trusting them, joins the run to the archive index, and refuses
    an emulated, dirty, debug-built, unpublishable, unattested, invariant-moved
    or contended run. Exit 1 from it is an answer, so the reasons are collected
    rather than raised.
    """
    result = ex.local_step(
        f"import.{family}",
        [
            "docker",
            "run",
            "--rm",
            "--platform",
            LOCAL_PLATFORM,
            "-v",
            # Read-only, because this is the stage that decides whether anything
            # may be written and it must not be able to write anything itself.
            # `import-run.mjs` only writes the history, which is in /staging, so
            # this costs nothing and turns a promise into a mount flag.
            f"{repo}:/repo:ro",
            "-v",
            f"{staging}:/staging",
            "-w",
            "/repo",
            NODE_IMAGE,
            "node",
            "/repo/tools/publish/import-run.mjs",
            "--document",
            f"/staging/archive/{family}/{run_id}.json",
            "--archive",
            f"/staging/archive/{family}",
            "--history",
            "/staging/history.json",
        ],
        allow_failure=True,
    )
    print(result.stdout)
    if result.code != 0:
        gate_refused(result, family, "`import-run.mjs`", refusals)
        return False
    return True


def publish(repo: Path, staging: Path, families: list[str]) -> list[str]:
    """Copy the staging result into the repository. Only reached once every gate
    has passed, which is what makes a refused capture leave `git status` clean.

    Named paths, one at a time. A wildcard here would sweep whatever else the
    run left in the staging tree into a commit.
    """
    written = []
    for family in families:
        source = staging / "archive" / family
        if not source.exists():
            continue
        target = repo / "archive" / family
        target.mkdir(parents=True, exist_ok=True)
        for item in sorted(source.iterdir()):
            if item.suffix != ".json":
                continue
            shutil.copyfile(item, target / item.name)
            written.append(str((target / item.name).relative_to(repo)))
    history = repo / "tools" / "publish" / "history.json"
    shutil.copyfile(staging / "history.json", history)
    written.append(str(history.relative_to(repo)))
    return written


def main(argv: list[str] | None = None, executor: Executor | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--profile", default="full")
    parser.add_argument("--families", default=",".join(FAMILIES))
    parser.add_argument("--repo", default=str(Path(__file__).resolve().parent.parent))
    parser.add_argument("--out", default=None, help="where the raw captured documents land")
    parser.add_argument("--json", default=None, help="where the run summary is written; - for stdout")
    parser.add_argument("--nas", default=os.environ.get("NAS", "rom@192.168.0.10"))
    parser.add_argument("--name", default=None, help="the scratch name; derived from the pid otherwise")
    parser.add_argument(
        "--plan",
        action="store_true",
        help="print every command this would run, and run none of them",
    )
    parser.add_argument(
        "--settle-timeout",
        type=int,
        default=SETTLE_TIMEOUT_S,
        help="seconds to wait for the machine to go quiet before refusing (default %(default)s)",
    )
    parser.add_argument("--keep", action="store_true", help="leave the scratch tree on the NAS")
    parser.add_argument(
        "--commit",
        action="store_true",
        help="commit the published files, by name, once every gate has passed",
    )
    args = parser.parse_args(argv)

    repo = Path(args.repo).resolve()
    families = [f for f in args.families.split(",") if f]
    unknown = [f for f in families if f not in FAMILIES]
    if unknown:
        print(f"unknown family/families: {', '.join(unknown)}", file=sys.stderr)
        return 2
    name = args.name or f"viprs-capture-{os.getpid()}"
    # Injected by the tests, which drive the whole of this function against a
    # recorder. The control flow under test is this one and not a copy of it.
    ex = executor or Executor(nas=args.nas, plan=args.plan)

    summary: dict = {
        "schemaVersion": 1,
        "tool": "libviprs-bench tools/capture.py",
        "nas": args.nas,
        "profile": args.profile,
        "families": {},
        "published": False,
        "refusals": [],
        "writes": [],
    }
    refusals: list[str] = []

    try:
        # Before anything is measured. `ci` archives indistinguishably from a
        # calibrated sweep and is refused only at import, so finding out after
        # forty minutes of measuring is the expensive way to learn it.
        allowed = publishable_profiles(repo)
        if args.profile not in allowed:
            raise Refused(
                [
                    f"profile {args.profile!r} is not publishable ({', '.join(allowed)}). A `ci` "
                    "sweep proves the harness runs, takes three reps of a cut-down cell list and "
                    "archives indistinguishably from a calibrated one, so the importer refuses it. "
                    "Nothing is measured for a run that cannot be published."
                ]
            )

        with tempfile.TemporaryDirectory(prefix="viprs-capture-") as tmp:
            tmpdir = Path(tmp)
            stage = tmpdir / "stage"
            stage.mkdir()
            staging = tmpdir / "staging"
            (staging / "archive").mkdir(parents=True)
            out_dir = Path(args.out).resolve() if args.out else tmpdir / "capture"
            out_dir.mkdir(parents=True, exist_ok=True)

            # The staging copy the whole chain runs against. Nothing in the
            # repository is touched until every gate has passed.
            shutil.copyfile(repo / "tools" / "publish" / "history.json", staging / "history.json")
            for family in families:
                source = repo / "archive" / family
                if source.exists():
                    shutil.copytree(source, staging / "archive" / family)

            print("== staging clean checkouts")
            revisions = stage_checkouts(ex, stage)
            summary["benchCommit"] = revisions.get("libviprs-bench")
            summary["engineCommit"] = revisions.get("libviprs")
            print(f"  bench {summary['benchCommit']}, engine {summary['engineCommit']}")

            root = scratch_root(name)
            tree_tar = make_tarball(ex, "stage.tar.tree", stage, tmpdir / "tree.tar", ("target",))
            archive_tar = make_tarball(
                ex, "stage.tar.archive", staging / "archive", tmpdir / "archive.tar", ()
            )

            try:
                push_tree(ex, name, tree_tar, root)
                dockerfile = tmpdir / "nas-driver.Dockerfile"
                dockerfile.write_text(DRIVER_DOCKERFILE)
                build_driver_image(ex, dockerfile)
                for family in families:
                    build_family_image(ex, name, family)
                # `out` and `scratch` are made here rather than left to docker,
                # which creates a missing bind-mount source itself and as root.
                # That is the same end state and no record of who did it, and it
                # is the mechanism traps 4 and 8 are both about, so the driver
                # does it where the guard can see it.
                for leaf in ("out", "scratch"):
                    ex.nas_step(
                        f"scratch.mkdir.{leaf}",
                        nas_container(
                            UTIL_IMAGE,
                            f"mkdir -p /ws/nas-work/{name}/{leaf}",
                            mounts=(("$HOME/workspace", "/ws"),),
                        ),
                    )
                # The archive goes up as its own tree, OUTSIDE the git checkout,
                # and after the builds. Copying it into the pushed clone would
                # make that tree dirty, and `provenance.dirty` is a refusal: the
                # staging that exists to file the run would refuse it. After the
                # builds because the scratch root is the build context, and the
                # archive is not something the image should be able to see.
                push_tree(ex, name, archive_tar, f"{root}/archive")

                for family in families:
                    print(f"== capturing {family}, profile {args.profile}, native x86_64")
                    settled = settle(ex, family, args.settle_timeout)
                    document_path = capture_family(ex, name, family, args.profile, out_dir)
                    document = json.loads(document_path.read_text() or "{}")
                    entry = {"settle": settled, "document": str(document_path)}
                    entry.update(cell_summary(document))
                    run_id = archive_family(ex, name, family, staging, refusals)
                    entry["runId"] = run_id
                    entry["archived"] = run_id is not None
                    entry["imported"] = False
                    summary["families"][family] = entry
            finally:
                if args.keep:
                    print(f"  --keep: the scratch tree stays at {root}")
                    summary["keptScratchTree"] = root
                else:
                    summary["nasLeftAsFound"] = cleanup(ex, name)

            for family in families:
                entry = summary["families"].get(family)
                if not entry or not entry.get("runId"):
                    continue
                entry["imported"] = import_family(
                    ex, repo, staging, family, entry["runId"], refusals
                )

            # Keyed on the families asked for, never on whether anything was
            # refused. Those are not the same question, and the difference is
            # what let a gate that failed silently publish the other family and
            # report success: no reasons were collected, so `if refusals` was
            # false, and `if not imported` only asks whether ANY family got
            # through. A family that could not be published does not publish the
            # one that could through the same commit either, because a history
            # holding one half of a capture reads as a complete run of one
            # family.
            imported = [f for f, e in summary["families"].items() if e.get("imported")]
            missing = [f for f in families if f not in imported]
            if missing:
                raise Refused(
                    refusals
                    + [
                        f"{', '.join(missing)} did not reach the history, so nothing is "
                        "published: a capture publishes every family it was asked for or none "
                        "of them."
                    ]
                )
            if refusals:
                raise Refused(refusals)

            if not args.plan:
                summary["writes"] = publish(repo, staging, families)
            # A plan publishes nothing, so it does not get to say it published.
            summary["published"] = not args.plan
            summary["history"] = {
                "path": "tools/publish/history.json",
                "entries": len(json.loads((staging / "history.json").read_text())),
            }

            if args.commit and not args.plan:
                subprocess.run(["git", "-C", str(repo), "add", *summary["writes"]], check=True)
                # `-- <paths>` and not a bare `git commit`. Without the pathspec
                # this commits the whole index, so anything the operator had
                # already staged rides along in a commit whose message says it
                # is a capture. The help text promises the named paths; this is
                # what makes that true.
                subprocess.run(
                    [
                        "git",
                        "-C",
                        str(repo),
                        "commit",
                        "-m",
                        f"bench: publish {', '.join(imported)} at profile {args.profile}",
                        "--",
                        *summary["writes"],
                    ],
                    check=True,
                )
    except Refused as refused:
        summary["refusals"] = refused.reasons
        print("\nREFUSED. Nothing was published and the repository is untouched:\n", file=sys.stderr)
        for reason in refused.reasons:
            print(f"  · {reason}\n", file=sys.stderr)
        emit(args, summary, ex)
        return 1
    except Exception as broke:
        # Not a refusal: something failed rather than answered. The summary is
        # still written, because a capture that dies after the builds is exactly
        # when the record of what it had already done is worth having, and a
        # traceback on its own says nothing about which families were captured or
        # whether the machine was left clean. The traceback goes to stderr after
        # it, unchanged.
        summary["failed"] = str(broke)
        emit(args, summary, ex)
        raise

    emit(args, summary, ex)
    if args.plan:
        return 0
    print("\npublished:")
    for path in summary["writes"]:
        print(f"  {path}")
    print(
        "\nlibviprs-org reads these out of this repository at a pinned revision. Bump\n"
        "benchmarks/BENCH_REV there to this commit and run its ingest to move the page."
    )
    return 0


def emit(args, summary: dict, ex: Executor) -> None:
    """The run summary, and in --plan the command list the guard walks."""
    if args.plan:
        summary["plan"] = [
            {
                "label": s.label,
                "where": s.where,
                **({"remote": s.remote} if s.where == "nas" else {"argv": s.argv}),
                **({"stdin": s.stdin_path, "stdinHead": s.stdin_head} if s.stdin_path else {}),
            }
            for s in ex.steps
        ]
    text = json.dumps(summary, indent=2)
    if args.json == "-" or (args.plan and not args.json):
        print(text)
    elif args.json:
        Path(args.json).write_text(text + "\n")
        print(f"\nsummary written to {args.json}")


if __name__ == "__main__":
    sys.exit(main())
