# The image `cargo test` runs in (#135).
#
# tests/engines_family_end_to_end.rs shells out to node to prove the charts are
# drawn, and fails loudly when node is missing. The stock rust image has none, so
# this is that image plus node, copied from a digest-pinned node image (bookworm's
# own apt nodejs is 18, package.json wants >=20).
FROM rust:1.99.1-bookworm@sha256:e12566d5abc17a0da741628782cdb504b68b8961f5bfcf677936bf3e8b32de29

COPY --from=node:22-bookworm-slim@sha256:de170b39439eb0e5a3444ad7d60b015614a381e10fecae2e30ec9e735122f38d /usr/local/bin/node /usr/local/bin/node
COPY --from=node:22-bookworm-slim@sha256:de170b39439eb0e5a3444ad7d60b015614a381e10fecae2e30ec9e735122f38d /usr/local/lib/node_modules/npm /usr/local/lib/node_modules/npm

# npm is a symlink into the copied tree; render.mjs imports @spdrman/bencharts,
# which `npm ci` fetches from the (public) git pin in package.json.
# The tree is bind-mounted from a host whose uid is not the container's, and
# git refuses such a repository as "dubious ownership" (the version-matrix tests
# run `git worktree add` in it), so every directory is trusted inside this image.
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
 && git config --system --add safe.directory '*' \
 && node --version && npm --version
