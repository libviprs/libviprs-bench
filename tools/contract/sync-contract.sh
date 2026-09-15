#!/usr/bin/env bash
# The sync gate for the frozen contract.
#
#   tools/contract/sync-contract.sh --check           verify, never write
#   tools/contract/sync-contract.sh --write            re-copy from the clone
#   tools/contract/sync-contract.sh --check --upstream <dir>
#
# `upstream/` is a byte-for-byte copy of causl-org `pages/benchmarks/` at the
# revision `UPSTREAM_REV` names. Nothing libviprs-specific is ever edited into
# one of those files: the differences live in `config.json` and are applied by
# `parameterize.mjs`. This script is what turns "nothing was edited in" from a
# convention into a red test.
#
# Two modes, and the point is that at least one of them ALWAYS runs:
#
#   manifest  Hash every frozen file the way git hashes a blob and compare
#             against `UPSTREAM.manifest`. Needs no clone and no git, so it runs
#             in CI and in the node container, where the causl repositories
#             (private Gitea) are not reachable. The numbers in the manifest are
#             the blob sha1s `git ls-tree` reported at the pin, so this is a
#             check against upstream, not a self-hash.
#
#   clone     Re-derive each blob sha1 from a local causl-org clone with
#             `git rev-parse <rev>:<path>/<file>` and compare. Runs only when the
#             clone is there, and it does not trust the manifest's copy of the
#             number, so a manifest doctored to match a doctored frozen file
#             still fails.
#
# A missing clone downgrades to manifest-only and says so. It never turns into a
# silent pass: a host-capability skip is the same colour as a pass, and that is
# exactly the failure this file exists to avoid. If the manifest itself cannot
# be read, the script refuses (exit 3) rather than reporting green.
#
# Exit codes: 0 in sync · 1 drift · 2 usage · 3 refused (cannot check)

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NODE="${NODE:-node}"
BLOBSHA="$HERE/lib/blob-sha1.mjs"
MANIFEST="$HERE/UPSTREAM.manifest"
REV_FILE="$HERE/UPSTREAM_REV"
FROZEN="$HERE/upstream"

EXIT_OK=0
EXIT_DRIFT=1
EXIT_USAGE=2
EXIT_REFUSED=3

mode=""
upstream_dir="${CAUSL_ORG_DIR:-}"

while [ $# -gt 0 ]; do
  case "$1" in
    --check) mode="check" ;;
    --write) mode="write" ;;
    --upstream)
      shift
      [ $# -gt 0 ] || { echo "sync-contract: --upstream needs a directory" >&2; exit $EXIT_USAGE; }
      upstream_dir="$1"
      ;;
    --help|-h)
      sed -n '2,35p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit $EXIT_USAGE
      ;;
    *)
      echo "sync-contract: unknown argument: $1" >&2
      echo "usage: sync-contract.sh (--check | --write) [--upstream <causl-org clone>]" >&2
      exit $EXIT_USAGE
      ;;
  esac
  shift
done

if [ -z "$mode" ]; then
  echo "sync-contract: pass --check or --write" >&2
  exit $EXIT_USAGE
fi

# --- what the pin is --------------------------------------------------------

[ -r "$MANIFEST" ] || { echo "sync-contract: refused, no readable manifest at $MANIFEST" >&2; exit $EXIT_REFUSED; }
[ -r "$BLOBSHA" ]  || { echo "sync-contract: refused, no hasher at $BLOBSHA" >&2; exit $EXIT_REFUSED; }
command -v "$NODE" >/dev/null 2>&1 || { echo "sync-contract: refused, node is not on PATH" >&2; exit $EXIT_REFUSED; }
[ -r "$REV_FILE" ] || { echo "sync-contract: refused, no readable rev at $REV_FILE" >&2; exit $EXIT_REFUSED; }
[ -d "$FROZEN" ]   || { echo "sync-contract: refused, no frozen copy at $FROZEN" >&2; exit $EXIT_REFUSED; }

# One `<sha1>  <name>` line per argument, computed the way git computes a blob
# sha1. Never `git hash-object`: the gate has to run in the node container too.
hash_files() { "$NODE" "$BLOBSHA" "$@"; }

REV="$(tr -d ' \t\n' < "$REV_FILE")"
[ -n "$REV" ] || { echo "sync-contract: refused, UPSTREAM_REV is empty" >&2; exit $EXIT_REFUSED; }

manifest_rev="$(awk '$1=="rev"{print $2}' "$MANIFEST")"
manifest_path="$(awk '$1=="path"{print $2}' "$MANIFEST")"
if [ "$manifest_rev" != "$REV" ]; then
  echo "sync-contract: UPSTREAM_REV says $REV but UPSTREAM.manifest says $manifest_rev" >&2
  exit $EXIT_DRIFT
fi
[ -n "$manifest_path" ] || { echo "sync-contract: refused, the manifest names no upstream path" >&2; exit $EXIT_REFUSED; }

# Files are exactly the manifest's list, so a frozen file that was deleted and
# one that was added are both drift rather than something the loop skips over.
files="$(awk 'f{print $2} /^---$/{f=1}' "$MANIFEST")"
[ -n "$files" ] || { echo "sync-contract: refused, the manifest lists no files" >&2; exit $EXIT_REFUSED; }

# --- find a clone, if there is one ------------------------------------------

if [ -z "$upstream_dir" ]; then
  for guess in \
    "$HERE/../../../causl/causl-org" \
    "$HERE/../../../../causl/causl-org" \
    "$HOME/workspace/causl/causl-org"
  do
    if [ -d "$guess/.git" ]; then upstream_dir="$guess"; break; fi
  done
fi

have_clone=0
if [ -n "$upstream_dir" ] && [ -d "$upstream_dir/.git" ]; then
  if git -C "$upstream_dir" cat-file -e "$REV^{commit}" 2>/dev/null; then
    have_clone=1
  else
    echo "sync-contract: $upstream_dir is a clone but does not have $REV; manifest mode only" >&2
  fi
fi

drift=0
report() { printf '  %-26s %s\n' "$1" "$2"; }

if [ "$mode" = "write" ]; then
  if [ "$have_clone" -ne 1 ]; then
    echo "sync-contract: refused, --write needs a causl-org clone carrying $REV" >&2
    echo "               pass --upstream <dir> or set CAUSL_ORG_DIR" >&2
    exit $EXIT_REFUSED
  fi
  for f in $files; do
    git -C "$upstream_dir" show "$REV:$manifest_path/$f" > "$FROZEN/$f"
    report "$f" "written"
  done
  {
    sed -n '1,/^---$/p' "$MANIFEST"
    for f in $files; do hash_files "$FROZEN/$f"; done
  } > "$MANIFEST.next"
  mv "$MANIFEST.next" "$MANIFEST"
  echo "sync-contract: re-copied ${REV} from $upstream_dir and refreshed the manifest"
  exit $EXIT_OK
fi

# --- manifest mode: always runs ---------------------------------------------

echo "sync-contract: checking tools/contract/upstream against causl-org $manifest_path @ $REV"
echo "manifest (blob sha1 recorded at the pin):"
while read -r want name; do
  case "$want" in ''|'#'*) continue ;; esac
  if [ ! -f "$FROZEN/$name" ]; then
    report "$name" "MISSING from upstream/"
    drift=1
    continue
  fi
  got="$(hash_files "$FROZEN/$name" | cut -d' ' -f1)"
  if [ "$got" = "$want" ]; then
    report "$name" "ok"
  else
    report "$name" "DRIFTED (have $got, pinned $want)"
    drift=1
  fi
done <<EOF
$(awk 'f{print} /^---$/{f=1}' "$MANIFEST")
EOF

# A file sitting in upstream/ that the manifest does not name is drift too: it
# is an un-pinned file inside the frozen directory, which is the shape a
# libviprs-specific edit would take if someone added one instead of editing one.
for p in "$FROZEN"/*; do
  [ -e "$p" ] || continue
  n="$(basename "$p")"
  if ! printf '%s\n' $files | grep -qx -- "$n"; then
    report "$n" "UNPINNED (in upstream/, not in the manifest)"
    drift=1
  fi
done

# --- clone mode: runs when a clone is there ---------------------------------

if [ "$have_clone" -eq 1 ]; then
  echo "clone ($upstream_dir):"
  # The clone check re-derives upstream's blob sha1 from the clone itself
  # rather than trusting the manifest's copy of it, so a manifest edited to
  # match a doctored frozen file still fails here.
  for f in $files; do
    want="$(git -C "$upstream_dir" rev-parse "$REV:$manifest_path/$f" 2>/dev/null || true)"
    got="$(hash_files "$FROZEN/$f" | cut -d' ' -f1)"
    if [ -z "$want" ]; then
      report "$f" "NOT AT $REV upstream"
      drift=1
    elif [ "$want" = "$got" ]; then
      report "$f" "identical"
    else
      report "$f" "DIFFERS from $REV"
      echo "      git -C $upstream_dir show $REV:$manifest_path/$f | diff - $FROZEN/$f"
      drift=1
    fi
  done
else
  echo "clone: not available, manifest mode only"
  echo "       (the causl repositories are private Gitea; the manifest is the check that"
  echo "        runs everywhere, and it carries the blob sha1s from the pin itself)"
fi

if [ "$drift" -ne 0 ]; then
  echo "sync-contract: DRIFT. A frozen file is not what causl-org $REV holds." >&2
  echo "               Nothing libviprs-specific belongs in tools/contract/upstream/:" >&2
  echo "               put the difference in config.json instead. To move the pin," >&2
  echo "               edit UPSTREAM_REV and run --write." >&2
  exit $EXIT_DRIFT
fi

echo "sync-contract: in sync with causl-org $manifest_path @ $REV"
exit $EXIT_OK
