// The writer capture script may only make the measurement host do three things:
// ssh, docker, and writes into the one bind-mounted scratch directory
// (libviprs-bench #132).
//
// It is the same rule `capture-nas.test.mjs` holds the older script to, and for
// the same reason: the trap is the command that does not feel like work. So this
// walks the script instead of trusting the next edit to remember.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const script = readFileSync(join(here, 'capture-writer.sh'), 'utf8');

/** The remote command of every `mars "..."` call, comments excluded. */
function remoteCommands(src) {
  const out = [];
  const code = src
    .split('\n')
    .filter((l) => !l.trimStart().startsWith('#'))
    .join('\n');
  const marker = /\bmars "/g;
  let m;
  while ((m = marker.exec(code))) {
    let i = marker.lastIndex;
    let body = '';
    while (i < code.length && !(code[i] === '"' && code[i - 1] !== '\\')) body += code[i++];
    // `$DOCKER` is `docker --config /tmp/dcbench`, said once at the top of the script.
    out.push(body.replaceAll('$DOCKER', 'docker --config /tmp/dcbench'));
  }
  return out;
}

/** One docker invocation with no host shell around it: what is inside the
 *  container's single quotes is the container's business, and nothing outside
 *  them may chain, pipe or redirect. */
function isOneDockerCommand(remote) {
  const outside = remote.replace(/'[^']*'/g, "''");
  if (!/^docker --config \/tmp\/dcbench (run|build|images|rm|rmi|image|info) /.test(outside)) return false;
  return !/[;|&><`]|\$\(/.test(outside);
}

test('every command sent to the host is one docker invocation', () => {
  const commands = remoteCommands(script);
  // Positive control: a walk that finds nothing would pass for the wrong reason.
  assert.ok(commands.length >= 8, `expected to find remote commands, found ${commands.length}`);
  const offenders = commands.filter((c) => !isOneDockerCommand(c));
  assert.deepEqual(offenders, [], `these run on the machine itself:\n${offenders.join('\n---\n')}`);
});

test('the throw-away helper pins the platform and runs in a container', () => {
  const helper = script.match(/util\(\) \{[\s\S]*?\n\}/)?.[0] ?? '';
  assert.match(helper, /\$DOCKER run --rm --platform linux\/amd64/);
});

test('every docker run names its platform', () => {
  const runs = remoteCommands(script).filter((c) => c.startsWith('docker --config /tmp/dcbench run'));
  assert.ok(runs.length >= 4, 'the walk found the docker runs');
  for (const run of runs) assert.match(run, /--platform linux\/amd64/, run);
});

test('nothing is copied with scp, which the host refuses', () => {
  assert.ok(!/\bscp\b/.test(script.replace(/^\s*#.*$/gm, '')));
});

test('the push keeps .git, because provenance cannot resolve a commit without it', () => {
  const push = script.split('\n').find((l) => l.includes('tar -cf -') && l.includes('$STAGE'));
  assert.ok(push, 'the push line is still recognisable');
  assert.ok(!/--exclude=\.git/.test(push), 'the trees are pushed with their git history');
  assert.match(push, /--exclude=target/, 'build output is not worth pushing');
});

test('cleanup is on a trap and removes only what this run made', () => {
  assert.match(script, /trap cleanup EXIT/);
  assert.match(script, /\$DOCKER rmi -f \$IMAGE/, 'its own image goes');
  assert.ok(!/rmi[^\n]*nas-driver/.test(script), 'the shared driver image is left for other jobs');
  assert.match(script, /rm -rf \/ws\/\$NAME/, 'only its own scratch tree goes');
  // The only `rm -rf` in the file is inside that container's quoted argument.
  const bare = script
    .split('\n')
    .filter((l) => !l.trimStart().startsWith('#') && /\brm -rf\b/.test(l) && !l.includes("sh -c"));
  assert.deepEqual(bare, [], 'no rm -rf runs outside a container');
});

test('the scratch tree is removed by mounting its parent, not itself', () => {
  assert.match(script, /util "\$WORK:\/ws" -- sh -c "'rm -rf/);
});

test('the capture refuses to measure on a machine that never settled', () => {
  assert.match(script, /refusing to measure/);
});
