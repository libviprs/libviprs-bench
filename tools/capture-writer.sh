#!/usr/bin/env bash
# Capture the PMTiles writer sweep on MARS, natively, against one revision of the
# core, and bring back a checked, sealed, verified document (libviprs-bench #132).
#
#   tools/capture-writer.sh --core-rev <sha|tag> [--bench-rev <sha>] [--profile full|xl|ci]
#                           [--core-repo <url|path>] [--bench-repo <url|path>]
#                           [--out <dir>] [--name <tag>] [--settle-below <load>] [--keep]
#
# One invocation measures ONE core revision, because a document carries exactly
# one `provenance.library.commit` and the comparison the issue asks for is
# between documents. Run it once per revision: the pre-epic baseline, the
# first release with the epic, and the current writer.
#
# What this is for, and what it is not. `tools/capture.py` is the publishing
# path for the storage and engines families and it measures `main` of both
# repositories, by design ("a number that cannot say which commit produced it is
# not evidence"). A writer sweep is measured against a CHOSEN core revision and
# against a harness commit that is not on `main` until it merges, so this is the
# capture half only, with the same rules carried over:
#
#   1. The trees are pushed WITH `.git`, or provenance cannot resolve a commit and
#      the aggregator refuses the run. The pushed harness tree is a clean checkout
#      of the commit named, never this working tree.
#   2. Everything executes in a container. Only ssh, docker, and writes into one
#      scratch directory touch the host. Unpacking the tarball, reading the load,
#      archiving and listing what is left are all container work.
#   3. Retrieval is `docker run ... cat` over ssh, never scp.
#   4. Cleanup runs on a trap, from inside a container (the builds run as root and
#      leave files the host account cannot delete), and it removes only what this
#      run made: its scratch tree and its own image. The shared `nas-driver` image
#      is left alone, other jobs use it.
#   5. The machine is settled before measuring and the capture REFUSES to run if
#      it never settles. MARS is shared, and a sweep taken at load 90 is a number
#      about the neighbours. The harness records the load per row either way; this
#      is the earlier, cheaper refusal.
#   6. The aggregator runs in the image that measured, on the document that was
#      measured, before the document leaves the box: --check, --archive into a
#      copy of the repository's archive directory, then --verify.
#
# Nothing here writes into the repository. The sealed document and the updated
# index come back under --out, and committing them (and importing the run into
# tools/publish/history.json) is the next, separate step.
set -euo pipefail

NAS=${NAS:-devshell@192.168.0.100}
SSH=(ssh -o BatchMode=yes -o LogLevel=ERROR "$NAS")
WORK=/work/nas-work
CORE_REPO=https://github.com/libviprs/libviprs.git
BENCH_REPO=https://github.com/libviprs/libviprs-bench.git
CORE_REV=
BENCH_REV=
PROFILE=full
OUT=$(pwd)/writer-capture
NAME=viprs-writer-$$
SETTLE_BELOW=4
SETTLE_BUDGET_S=3600
KEEP=false

while [ $# -gt 0 ]; do
  case $1 in
    --core-rev)     CORE_REV=$2; shift 2 ;;
    --bench-rev)    BENCH_REV=$2; shift 2 ;;
    --core-repo)    CORE_REPO=$2; shift 2 ;;
    --bench-repo)   BENCH_REPO=$2; shift 2 ;;
    --profile)      PROFILE=$2; shift 2 ;;
    --out)          OUT=$2; shift 2 ;;
    --name)         NAME=$2; shift 2 ;;
    --settle-below) SETTLE_BELOW=$2; shift 2 ;;
    --settle-budget) SETTLE_BUDGET_S=$2; shift 2 ;;
    --keep)         KEEP=true; shift ;;
    -h|--help)      sed -n '2,38p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$CORE_REV" ] || { echo "--core-rev is required" >&2; exit 2; }
[ -n "$BENCH_REV" ] || BENCH_REV=$(git -C "$(dirname "$0")/.." rev-parse HEAD)

say() { printf '\n== %s\n' "$*"; }
IMAGE=viprs-writer:$NAME
BUILDER=viprs-writer-build-$NAME

# Every remote command is ONE docker invocation, with no host shell around it.
# `mars` sends whatever it is given, so the discipline is in what is given, and
# `tools/capture-writer.test.mjs` walks this file and holds it: anything chained
# after a docker command with `;`, `|` or `&&` would run on the machine itself.
mars() { "${SSH[@]}" "$@"; }
# A throw-away alpine container, with --platform spelled out.
util() { # util <host:container mounts...> -- <command...>
  local mounts=""
  while [ "$1" != "--" ]; do mounts="$mounts -v $1"; shift; done; shift
  mars "docker run --rm --platform linux/amd64$mounts alpine:3.20 $*"
}

cleanup() {
  local code=$?
  say "cleaning up $NAS"
  mars "docker rm -f $BUILDER" >/dev/null 2>&1 || true
  mars "docker rmi -f $IMAGE" >/dev/null 2>&1 || true
  util "$WORK:/ws" -- sh -c "'rm -rf /ws/$NAME'" >/dev/null 2>&1 || true
  # Report what is left rather than asserting the machine is clean. The counting
  # is local plumbing on docker's own output.
  printf 'images matching this run: %s\n' \
    "$(mars "docker images --format '{{.Repository}}:{{.Tag}}'" 2>/dev/null | grep -c "$NAME" || true)"
  printf 'scratch trees matching this run: %s\n' \
    "$(util "$WORK:/ws" -- ls /ws 2>/dev/null | grep -c "^$NAME\$" || true)"
  exit $code
}
$KEEP || trap cleanup EXIT

# Fetch one revision of one repository into <dest>, with .git, at depth 1. A
# local path is fetched with any-sha enabled on the sending side (a path has no
# host policy to satisfy); a URL is fetched as it is.
fetch_rev() { # fetch_rev <repo> <rev> <dest> <remote-url-to-record>
  mkdir -p "$3"
  git -C "$3" init -q
  if [ -d "$1" ]; then
    git -C "$3" fetch -q --depth 1 --upload-pack='git -c uploadpack.allowAnySHA1InWant=true upload-pack' "$1" "$2"
  else
    git -C "$3" fetch -q --depth 1 "$1" "$2"
  fi
  git -C "$3" checkout -q --detach FETCH_HEAD
  git -C "$3" remote add origin "$4"
}

say "staging clean checkouts"
# Under /tmp, and never removed by this script: it holds only clones, the OS
# clears it, and a delete outside the scratch tree is not this script's to make.
STAGE=$(mktemp -d /tmp/writer-capture.XXXXXX)
fetch_rev "$CORE_REPO" "$CORE_REV" "$STAGE/libviprs" https://github.com/libviprs/libviprs.git
fetch_rev "$BENCH_REPO" "$BENCH_REV" "$STAGE/libviprs-bench" https://github.com/libviprs/libviprs-bench.git
CORE_SHA=$(git -C "$STAGE/libviprs" rev-parse HEAD)
BENCH_SHA=$(git -C "$STAGE/libviprs-bench" rev-parse HEAD)
CORE_VER=$(sed -n 's/^version = "\(.*\)"/\1/p' "$STAGE/libviprs/Cargo.toml" | head -1)
echo "core  $CORE_SHA (libviprs $CORE_VER)"
echo "bench $BENCH_SHA"

say "pushing to $NAS:$WORK/$NAME"
util "$WORK:/ws" -- sh -c "'mkdir -p /ws/$NAME/src /ws/$NAME/out /ws/$NAME/scratch'"
tar -cf - -C "$STAGE" --exclude=target --exclude=node_modules . \
  | mars "docker run --rm -i --platform linux/amd64 -v $WORK/$NAME/src:/dest alpine:3.20 tar -xf - -C /dest --no-same-owner"

say "ensuring the driver image (shared, never removed here)"
mars "docker image inspect nas-driver:latest" >/dev/null 2>&1 || mars "docker build -q -t nas-driver:latest -" >/dev/null <<'DOCKEREOF'
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
        docker-ce-cli docker-buildx-plugin
DOCKEREOF

say "building the writer image through the socket-mounted driver"
mars "docker run --rm --name $BUILDER --platform linux/amd64 \
  -v /var/run/docker.sock:/var/run/docker.sock -v $WORK/$NAME/src:/work -w /work nas-driver:latest \
  sh -c 'docker build --platform linux/amd64 -f libviprs-bench/Dockerfile --target writer -t $IMAGE . >/dev/null'"

DAEMON_ARCH=$(mars "docker info --format '{{.Architecture}}'" | tr -d '[:space:]')
CORES=$(util -- nproc | tr -d '[:space:]')

# Settle: two consecutive readings under the threshold, or refuse. A sweep that
# starts on a busy box is a measurement of the neighbours, and on a shared host
# the honest answer to "it never went quiet" is to say so and not measure.
say "settling (load below $SETTLE_BELOW on $CORES threads, budget ${SETTLE_BUDGET_S}s)"
waited=0; calm=0
while :; do
  load=$(util -- cat /proc/loadavg | cut -d' ' -f1)
  if awk "BEGIN{exit !($load < $SETTLE_BELOW)}"; then calm=$((calm + 1)); else calm=0; fi
  echo "  load $load (calm readings: $calm)"
  [ "$calm" -ge 2 ] && break
  if [ "$waited" -ge "$SETTLE_BUDGET_S" ]; then
    echo "the machine did not settle below $SETTLE_BELOW within ${SETTLE_BUDGET_S}s; refusing to measure" >&2
    exit 3
  fi
  sleep 30; waited=$((waited + 30))
done

say "capturing profile $PROFILE, native x86_64, core $CORE_VER@${CORE_SHA:0:8}"
mars "docker run --rm --platform linux/amd64 -v $WORK/$NAME/out:/out -v $WORK/$NAME/scratch:/scratch \
  -e TMPDIR=/scratch -e BENCH_DAEMON_ARCH=$DAEMON_ARCH $IMAGE \
  /src/libviprs-bench/target/release/pmtiles-writer --profile $PROFILE --out /out/writer-x86.json"

say "checking, archiving and verifying inside the image that measured"
AGG=/src/libviprs-bench/target/release/storage-aggregate
mars "docker run --rm --platform linux/amd64 -v $WORK/$NAME/out:/out $IMAGE sh -c '
  set -e
  $AGG --check /out/writer-x86.json
  mkdir -p /out/archive-storage
  cp -r /src/libviprs-bench/archive/storage/. /out/archive-storage/
  $AGG --archive /out/writer-x86.json --root /out/archive-storage
  doc=\$(ls -t /out/archive-storage/*.json | grep -v index.json | head -1)
  $AGG --verify \$doc
'"

say "retrieving"
mkdir -p "$OUT"
mars "docker run --rm --platform linux/amd64 -v $WORK/$NAME/out:/out alpine:3.20 cat /out/writer-x86.json" > "$OUT/writer-x86.json"
mars "docker run --rm --platform linux/amd64 -v $WORK/$NAME/out:/out alpine:3.20 tar -cf - -C /out/archive-storage ." \
  | tar -xf - -C "$(mkdir -p "$OUT/archive-storage" && echo "$OUT/archive-storage")"
echo "document: $OUT/writer-x86.json ($(wc -c < "$OUT/writer-x86.json") bytes)"
echo "archive:  $OUT/archive-storage"
echo "core $CORE_VER@$CORE_SHA, bench $BENCH_SHA, profile $PROFILE, native x86_64 on $NAS"
