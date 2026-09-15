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
