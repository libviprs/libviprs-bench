// The NAS capture script may only make the machine do three things: ssh, docker,
// and writes into the one bind-mounted scratch directory.
//
// The rule names unpacking a tarball explicitly, which is the trap I fell into
// writing this: my first version piped the push through a host-side `tar -xf`,
// and read the load with a host-side `cat /proc/loadavg`. Neither felt like
// work. Both executed on the machine. So this walks the script rather than
// trusting that the next edit will remember.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const script = readFileSync(join(here, 'capture-nas.sh'), 'utf8');

/** Statements sent over ssh, with comments and blank lines removed. */
function sshBlocks(src) {
  const blocks = [];
  const lines = src.split('\n');
  for (let i = 0; i < lines.length; i++) {
    if (lines[i].trimStart().startsWith('#')) continue;
    if (!lines[i].includes('${SSH[@]}')) continue;
    // A block runs until quotes balance, so a heredoc-style multi-line command
    // is read whole rather than as its first line.
    let block = lines[i];
    let quotes = (block.match(/"/g) ?? []).length;
    while (quotes % 2 !== 0 && i + 1 < lines.length) {
      block += '\n' + lines[++i];
      quotes += (lines[i].match(/"/g) ?? []).length;
    }
    blocks.push(block);
  }
  return blocks;
}

/** The single documented exception: creating the scratch directory itself. */
const SCRATCH_MKDIR = /mkdir -p \\?\$HOME\/workspace\/nas-work/;

test('every command sent to the NAS runs in a container', () => {
  const blocks = sshBlocks(script);
  // Positive control: if the walk finds nothing, a green result below means the
  // parser stopped matching, not that the script is clean.
  assert.ok(blocks.length >= 5, `expected to find ssh blocks, found ${blocks.length}`);

  const offenders = blocks.filter((b) => !b.includes('docker') && !SCRATCH_MKDIR.test(b));
  assert.deepEqual(
    offenders,
    [],
    `these run on the machine itself rather than in a container:\n${offenders.join('\n---\n')}`,
  );
});

test('the push keeps .git, because provenance cannot resolve a commit without it', () => {
  // tools/nas.sh excludes .git, which is right for every other job and wrong for
  // this one: the aggregator refuses a document whose commit is null.
  const push = script.split('\n').find((l) => l.includes('tar -cf -') && l.includes('$STAGE'));
  assert.ok(push, 'the push line is still recognisable');
  assert.ok(!push.includes("--exclude='.git'"), 'the benchmark tree is pushed with its git history');
  assert.ok(push.includes("--exclude='target'"), 'build output is not worth pushing');
});

test('cleanup is on a trap, so a failed capture does not litter the machine', () => {
  assert.match(script, /trap cleanup EXIT/, 'cleanup runs even when the capture fails');
  for (const what of ['docker rmi', 'rm -rf /ws/nas-work']) {
    assert.ok(script.includes(what), `cleanup removes ${what}`);
  }
});

test('the scratch tree is removed by mounting its parent, not itself', () => {
  // Docker creates a missing bind-mount source as root, so mounting the scratch
  // root is what left it root-owned and unwritable for the next run.
  assert.match(script, /-v \\?\$HOME\/workspace:\/ws/, 'the parent is mounted, not the scratch root');
});

// ---------------------------------------------------------------------------
// The same rule, over the Python driver.
//
// `tools/capture.py` is the driver the shell script became, and the rule does
// not care which language sends the command. Walking a Python source file with
// a regex would be the fragile way to do this, so the driver has a `--plan`
// mode that runs every line of its own orchestration against a recording
// executor and prints the commands it WOULD send. That is stronger than reading
// the source: the plan is produced by the code path a real run takes, so a
// command added inside a branch is in it and a command a refactor moved is
// still in it.
//
// The plan is also the completeness control. A guard that walks an empty list
// is green for the same reason a clean script is, so the labels below are
// asserted to be present: if the plan stops containing the capture, the archive
// or the cleanup, this fails rather than passing on a walk of nothing.

/** The driver's own account of every command it would send. */
function plan() {
  const proc = spawnSync(
    'python3',
    [join(here, 'capture.py'), '--plan', '--name', 'guard-fixture', '--json', '-'],
    { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 },
  );
  // Never a skip. A host without python3 cannot check this rule, and a check
  // that cannot run has to be the same colour as a check that failed: a
  // capability skip is the exact shape that ships as a pass.
  assert.equal(
    proc.status,
    0,
    `tools/capture.py --plan did not run, so nothing below checked anything:\n${proc.stderr}`,
  );
  const started = proc.stdout.indexOf('{');
  const summary = JSON.parse(proc.stdout.slice(started));
  assert.ok(Array.isArray(summary.plan), '--plan emits the command list');
  return summary.plan;
}

test('the driver sends nothing to the NAS that runs outside a container', () => {
  // No exception, not even the scratch mkdir the script above is allowed. The
  // driver pushes a second tree beside the first one, and by then the scratch
  // root belongs to whatever uid the tar carried: extracting an archive whose
  // top entry is `.` restores that entry's ownership onto the destination, the
  // tar is made on a Mac at uid 501 and the NAS account is 1001, so a host-side
  // `mkdir -p` inside the tree dies with "Permission denied" on a directory that
  // plainly exists. The first end-to-end run failed there after fifty minutes of
  // building. Creating it in a container fixes the failure and removes the
  // exception in the same move, so this asserts the stronger rule.
  const steps = plan().filter((s) => s.where === 'nas');
  assert.ok(steps.length >= 15, `expected a plan with the whole run in it, got ${steps.length} steps`);
  const offenders = steps.filter((s) => !s.remote.includes('docker'));
  assert.deepEqual(
    offenders.map((s) => `${s.label}: ${s.remote}`),
    [],
    'these would run on the machine itself rather than in a container',
  );
});

test('the driver creates its scratch directories in a container, not as the host account', () => {
  // The specific half of the rule above, named so that a regression reads as
  // what it is rather than as "something is not containerised".
  for (const step of plan().filter((s) => s.label.startsWith('push.mkdir.'))) {
    assert.match(step.remote, /^docker run /, `${step.label} runs on the machine itself`);
    assert.match(step.remote, /-v \$HOME\/workspace:\/ws/, `${step.label} mounts the parent`);
  }
});

test('the driver plans the whole run, so the walk above is not a walk of nothing', () => {
  const labels = plan().map((s) => s.label);
  for (const needed of [
    'stage.clone.libviprs-bench',
    'push.untar.guard-fixture',
    'driver.image',
    'build.storage',
    'build.engines',
    'settle.cores',
    'capture.storage',
    'retrieve.document.storage',
    'check.storage',
    'archive.storage',
    'verify.storage',
    'import.storage',
    'cleanup.images',
    'cleanup.scratch',
    'cleanup.list',
  ]) {
    assert.ok(labels.includes(needed), `the plan has no ${needed} step; it holds ${labels.join(', ')}`);
  }
});

test('the driver builds the driver image rather than assuming it', () => {
  // A local image on no registry. Assume it and docker goes to a pull that
  // cannot succeed and reports an authentication problem, which is the wrong
  // trail entirely; the script's own teardown is what exposed it.
  const build = plan().find((s) => s.label === 'driver.image');
  assert.match(build.remote, /docker build .* -t nas-driver:latest/);
  assert.match(build.remote, /docker-buildx-plugin/, 'on Docker 29 a CLI without buildx exits 125');
});

test('the driver pushes the tree with .git and without target', () => {
  const tar = plan().find((s) => s.label === 'stage.tar.tree');
  assert.ok(tar, 'the tree is tarred for the push');
  assert.ok(
    !tar.argv.some((a) => a.includes('--exclude=.git')),
    'the benchmark tree is pushed with its git history, or provenance resolves no commit',
  );
  assert.ok(tar.argv.includes('--exclude=target'), 'build output is not worth pushing');
});

test('the archive staging is pushed outside the git checkout', () => {
  // Copying the repository's archive into the pushed clone would make that tree
  // dirty, and `provenance.dirty` is a refusal: the staging that exists to file
  // the run would refuse it.
  const steps = plan().filter((s) => s.where === 'nas');
  const archivePush = steps.find((s) => s.label === 'push.untar.archive');
  assert.ok(archivePush, 'the archive goes up as its own tree');
  assert.match(archivePush.remote, /nas-work\/guard-fixture\/archive:\/dest/);
});

test('the aggregator is given the family directory, not its parent', () => {
  // `archive_text` writes `<root>/<runId>.json` and `<root>/index.json` into
  // exactly what it is handed, so `--root archive` files an engines run beside
  // the storage ones under an index that is not theirs.
  for (const family of ['storage', 'engines']) {
    const step = plan().find((s) => s.label === `archive.${family}`);
    assert.match(step.remote, new RegExp(`--root /archive/${family}(\\s|$)`));
  }
});

test('retrieval is a container cat, never scp', () => {
  const steps = plan();
  assert.ok(
    !steps.some((s) => JSON.stringify(s).includes('scp')),
    'scp fails on that host with "No such file or directory" on a file that exists',
  );
  const get = steps.find((s) => s.label === 'retrieve.document.storage');
  assert.match(get.remote, /docker run .*cat \/out\/storage-x86\.json/);
});

test('the scratch tree is removed by mounting its parent, not itself', () => {
  const step = plan().find((s) => s.label === 'cleanup.scratch');
  assert.match(step.remote, /-v \$HOME\/workspace:\/ws/, 'the parent is mounted, not the scratch root');
  assert.match(step.remote, /rm -rf \/ws\/nas-work\/guard-fixture/);
});

test('every local command is git, tar, or a container with its platform spelled out', () => {
  // `DOCKER_DEFAULT_PLATFORM` has been both values on this Mac, and it is
  // inherited by a shell started before it changed. An unpinned `docker run`
  // here is whatever that variable happened to be.
  const locals = plan().filter((s) => s.where === 'local');
  const offenders = locals.filter((s) => {
    if (['git', 'tar'].includes(s.argv[0])) return false;
    return !(s.argv[0] === 'docker' && s.argv.includes('--platform') && s.argv.includes('linux/arm64'));
  });
  assert.deepEqual(offenders.map((s) => s.label), [], 'these run on the host toolchain unpinned');
});
