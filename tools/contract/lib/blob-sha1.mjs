#!/usr/bin/env node
// Print the git blob sha1 of each file named on the command line, one
// `<sha1>  <basename>` line per file.
//
// This exists so `sync-contract.sh` needs node and nothing else. `git
// hash-object` gives the same number, but git is not in the node container the
// gate runs in, and a gate that cannot run where it is pointed is a gate that
// gets skipped. The numbers here are directly comparable to what `git ls-tree`
// printed at the pin, which is what makes the manifest a check against upstream
// rather than a self-hash.
//
// The hash is sha1 over the literal bytes `blob <byteLength>\0` followed by the
// file's bytes, which is git's object header, byte for byte.

import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { basename } from 'node:path';

export function blobSha1(bytes) {
  const h = createHash('sha1');
  h.update(Buffer.from(`blob ${bytes.length}\0`, 'binary'));
  h.update(bytes);
  return h.digest('hex');
}

const args = process.argv.slice(2);
if (args.length > 0) {
  for (const p of args) {
    process.stdout.write(`${blobSha1(readFileSync(p))}  ${basename(p)}\n`);
  }
}
