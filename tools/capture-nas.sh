#!/usr/bin/env bash
# Capture both benchmark families on the UGREEN NAS, natively, and leave the
# machine as we found it.
#
#   tools/capture-nas.sh [--profile full|xl|ci] [--out <dir>] [--keep]
#
# This is the capture half only: it brings two documents back and stops. The
# publishing path is `tools/capture.py`, which does this and then checks,
# archives, verifies and imports, and writes nothing into the repository unless
# every one of those passes. The six traps below are six of the nine
# that driver carries: it found three more by running, and they are in its header.
# Two of them apply here too, so if you edit this script read that list.
#
# The NAS (rom@192.168.0.10, HIGARA) is the only native x86_64 host available.
# Everything this suite publishes otherwise is arm64, and an amd64 container on
# an Apple Silicon Mac is Rosetta, which the emulation probe refuses. So a run
# that is meant to stand for x86_64 happens here or it does not happen.
#
# Nothing executes on the machine itself. Only ssh, docker, and writes into one
# scratch directory under ~/workspace/. `docker build` is a client-side call, so
# a container holding the host's socket and a Docker CLI performs the whole build
# with no docker-in-docker involved.
#
# Six traps are encoded below rather than left as habits, each because it cost
# something to find:
#
#   1. The tree is pushed WITH `.git`. tools/nas.sh excludes it, and without it
#      the provenance layer cannot resolve a commit, so the aggregator refuses
#      the run. The benchmark tree is the one case where that exclusion is wrong.
#   2. There is no wait-for-idle. The NAS carries resident backupd and postgres
#      containers and its load floor is 1.4 to 2.2 on six cores with all of them
#      at 0% CPU, so an absolute threshold can never be met. The harness records
#      the load and refuses at or above ncpu, which is the right gate.
#   3. Retrieval is `ssh cat`, not scp. scp fails there with "No such file or
#      directory" on a file that plainly exists, because the SFTP subsystem is
#      not available.
#   4. Every command runs in a container, including the ones that do not feel
#      like work. Unpacking the pushed tarball, reading the load, listing what
#      is left: the rule says if it executes, it executes in a container, and
#      unpacking a tarball is named in it explicitly. Only ssh, docker, and
#      writes into the one bind-mounted scratch directory touch the machine.
#      The scratch tree is also removed from inside a container, because the
#      build containers run as root and leave files the host account cannot
#      delete.
#   5. Cleanup runs on a trap, so a failed capture does not leave two
#      multi-gigabyte images and 30 MB of source behind. A capture that litters
#      is a capture nobody runs twice.
#   6. This host has six cores, so every T=8 rung is declined with a reason and
#      an x86_64 run carries fourteen fewer storage cells than an eight-core
#      arm64 one. That is honest, and it also means the x86_64 concurrency knee
#      that libviprs#1024 puts at T=8 is not reachable here.
set -euo pipefail

NAS=${NAS:-rom@192.168.0.10}
SSH=(ssh -o BatchMode=yes -o LogLevel=ERROR "$NAS")
NAME=viprs-capture-$$
PROFILE=full
OUT=$(pwd)/nas-capture
KEEP=false

while [ $# -gt 0 ]; do
  case $1 in
    --profile) PROFILE=$2; shift 2 ;;
    --out)     OUT=$2; shift 2 ;;
    --keep)    KEEP=true; shift ;;     # leave the scratch tree for debugging
    -h|--help) sed -n '2,51p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

say() { printf '\n== %s\n' "$*"; }

# Remove the scratch tree and every image this run built. Mounts the PARENT of
# the scratch root: docker creates a missing bind-mount source as root, so
# mounting the scratch root itself is what leaves it unwritable next time.
cleanup() {
  local code=$?
  say "cleaning up $NAS"
  # `nas-driver:latest` is deliberately NOT removed. Other jobs on this machine
  # use it, tearing down something shared because we happened to (re)build it
  # would be rude, and it is a 332 MB cached layer that the next run reuses. The
  # images this run built are ours alone and do go.
  "${SSH[@]}" "
    docker rm -f \$(docker ps -aq --filter ancestor=viprs-nas-storage:$NAME) 2>/dev/null || true
    docker rm -f \$(docker ps -aq --filter ancestor=viprs-nas-engines:$NAME) 2>/dev/null || true
    docker rmi -f viprs-nas-storage:$NAME viprs-nas-engines:$NAME 2>/dev/null || true
    docker run --rm --platform linux/amd64 -v \$HOME/workspace:/ws alpine:3.20 \
      sh -c 'rm -rf /ws/nas-work/$NAME' 2>/dev/null || true
  " 2>/dev/null || true
  # Report what is left rather than asserting the machine is clean. The listing
  # runs in a container too; `ls` on the host is still executing on the host.
  # The printf and grep here are shell plumbing around docker's own output, which
  # is the boundary I drew: the rule is aimed at toolchains and build drivers, not
  # at reading what a docker command just printed.
  "${SSH[@]}" "
    printf 'images matching this run: '
    docker images --format '{{.Repository}}:{{.Tag}}' | grep -c '$NAME' || echo 0
    printf 'scratch trees remaining: '
    docker run --rm --platform linux/amd64 -v \$HOME/workspace:/ws alpine:3.20 \
      sh -c 'ls /ws/nas-work 2>/dev/null | wc -l'
  " || true
  exit $code
}
$KEEP || trap cleanup EXIT

say "staging a clean checkout"
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' RETURN 2>/dev/null || true
git clone --quiet --depth 50 https://github.com/libviprs/libviprs-bench.git "$STAGE/libviprs-bench"
git clone --quiet --depth 50 https://github.com/libviprs/libviprs.git "$STAGE/libviprs"
BENCH_REV=$(git -C "$STAGE/libviprs-bench" rev-parse --short HEAD)
ENGINE_REV=$(git -C "$STAGE/libviprs" rev-parse --short HEAD)
echo "bench $BENCH_REV, engine $ENGINE_REV"

say "pushing to $NAS:~/workspace/nas-work/$NAME"
"${SSH[@]}" "mkdir -p \$HOME/workspace/nas-work/$NAME"
# `.git` is deliberately included; see trap 1 above. The tar is created here and
# unpacked INSIDE a container: unpacking a tarball on the machine is exactly what
# the containers-only rule names, and the host tar would run as the host account.
tar -cf - -C "$STAGE" --exclude='target' . | "${SSH[@]}" "docker run --rm -i --platform linux/amd64 \
  -v \$HOME/workspace/nas-work/$NAME:/dest alpine:3.20 tar -xf - -C /dest"

say "building the driver image"
# Not assumed. The driver is a local image, on no registry, so a missing one
# sends docker to a pull that cannot succeed and tells the reader to
# `docker login`, which is the wrong trail entirely. It went unnoticed because
# I built it by hand, then removed it in cleanup, then wrote this against a
# machine that still had it: the script's own teardown is what exposes its own
# prerequisite. The daemon caches it, so rebuilding each run costs nothing after
# the first.
#
# docker-buildx-plugin is not optional. On Docker 29 `docker build` IS
# `docker buildx build`, so a CLI-only image dies the moment a BuildKit flag is
# passed, with exit 125 and no message worth reading.
"${SSH[@]}" "docker build -q -t nas-driver:latest - >/dev/null <<'DOCKEREOF'
FROM debian:bookworm-slim
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
DOCKEREOF"

say "building both images through the socket-mounted driver"
for target in storage engines; do
  "${SSH[@]}" "docker run --rm --platform linux/amd64 \
    -v /var/run/docker.sock:/var/run/docker.sock \
    -v \$HOME/workspace/nas-work/$NAME:/work -w /work nas-driver:latest \
    sh -c 'docker build --platform linux/amd64 -f libviprs-bench/Dockerfile \
      --target $target -t viprs-nas-$target:$NAME . >/dev/null'"
  echo "built viprs-nas-$target:$NAME"
done

# Wait for the machine to shed the load the previous phase put on it.
#
# The first end-to-end run failed its own publish gate for this: the image
# builds left the box hot, storage opened at 1-minute load 7.77 on six cores,
# engines opened at 6.39, and the importer refused both with "284 of 526
# measured cells record machineLoad.quiet: false ... repetitions do not remove
# competing work". The script was contending with itself.
#
# The threshold is relative to core count, not absolute. An earlier version
# waited for load below 1.2 and spun the full four minutes every time, because
# this NAS carries resident backupd and postgres containers and its floor is 1.4
# to 2.2 with all of them at 0% CPU. Half the core count is reachable here and
# still well under the ncpu ceiling the harness refuses at.
settle() {
  local cores half waited=0
  cores=$("${SSH[@]}" "docker run --rm --platform linux/amd64 alpine:3.20 nproc" 2>/dev/null | tr -d '[:space:]')
  cores=${cores:-6}
  half=$(( cores / 2 ))
  while [ "$waited" -lt 300 ]; do
    local load
    load=$("${SSH[@]}" "docker run --rm --platform linux/amd64 alpine:3.20 \
      sh -c 'cut -d\" \" -f1 /proc/loadavg'" 2>/dev/null | tr -d '[:space:]')
    if [ -n "$load" ] && awk "BEGIN{exit !($load < $half)}"; then
      echo "  settled at load $load (under $half, ${cores} cores) after ${waited}s"
      return
    fi
    sleep 20; waited=$(( waited + 20 ))
  done
  # Not fatal. The runner samples the machine before it measures anything and the
  # publish gate refuses a run that started on a busy box (#100), so a machine
  # that never settles produces a refused document rather than a quiet lie. The
  # two thresholds are deliberately different: this waits for half the cores, the
  # gate refuses at one runnable thread per core, so settling leaves headroom
  # rather than landing on the line.
  echo "  still above $half after ${waited}s; capturing anyway, and the load is recorded"
}

mkdir -p "$OUT"
for fam in storage engines; do
  say "capturing $fam, profile $PROFILE, native x86_64"
  settle
  # The harness records the load it ran at; this line is only so the transcript
  # shows it too, and it reads /proc from inside a container like everything else.
  "${SSH[@]}" "docker run --rm --platform linux/amd64 alpine:3.20 \
    sh -c 'awk \"{print \\\"  load at start: \\\" \\\$1}\" /proc/loadavg'"
  "${SSH[@]}" "docker run --rm --platform linux/amd64 \
    -v \$HOME/workspace/nas-work/$NAME/out:/out \
    -v \$HOME/workspace/nas-work/$NAME/scratch:/scratch -e TMPDIR=/scratch \
    viprs-nas-$fam:$NAME /src/libviprs-bench/target/release/$fam \
      --family $fam --profile $PROFILE --out /out/$fam-x86.json"
  # ssh cat, not scp; see trap 3. The cat runs in a container so nothing but
  # ssh and docker executes on the machine.
  "${SSH[@]}" "docker run --rm --platform linux/amd64 \
    -v \$HOME/workspace/nas-work/$NAME/out:/out alpine:3.20 cat /out/$fam-x86.json" \
    > "$OUT/$fam-x86.json"
  echo "  retrieved $OUT/$fam-x86.json ($(wc -c < "$OUT/$fam-x86.json") bytes)"
done

say "done"
echo "bench $BENCH_REV, engine $ENGINE_REV, profile $PROFILE, native x86_64"
echo "documents in $OUT; archive them with storage-aggregate --archive before quoting a number"
