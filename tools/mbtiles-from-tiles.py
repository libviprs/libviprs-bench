#!/usr/bin/env python3
"""Write go-pmtiles' input: a tile set as an MBTiles file (libviprs-bench #132).

    mbtiles-from-tiles.py tree <tiles-dir> <out.mbtiles>
    mbtiles-from-tiles.py synthetic <z> <out.mbtiles>

`pmtiles convert` reads MBTiles and nothing else, so for go-pmtiles to build
the same archive the writer sweep builds, the same tiles have to get into one.
This is that step. It is untimed and it does not touch go-pmtiles.

`tree` reads `<tiles-dir>/<z>/<x>/<y>.png`, the XYZ layout the harness's
directory backend writes, so the tiles are byte for byte the ones the writer is
handed. `synthetic` writes one distinct payload at every address of zoom `z`,
and the payload is `synthetic_payload` in `src/storage/writer_sweep.rs`:
`viprs-synth z=<z> x=<x> y=<y>` padded with `.` to 48 bytes. The two have to
agree or the comparison is between two different tile sets, and the attestation
step reads go-pmtiles' output back against the Rust function to prove they did.

MBTiles is TMS, so `tile_row` is `2^z - 1 - y`. go-pmtiles flips it back.

Standard library only, and run with `-I` so nothing in the current directory is
importable (the tile directory is untrusted input).
"""

import os
import sqlite3
import sys

PAYLOAD_LEN = 48
BATCH = 20000


def synthetic_payload(z, x, y):
    return ("viprs-synth z=%d x=%d y=%d" % (z, x, y)).encode("ascii").ljust(PAYLOAD_LEN, b".")


def tree_tiles(root):
    zooms = sorted(int(d) for d in os.listdir(root) if d.isdigit())
    for z in zooms:
        for x in sorted(int(d) for d in os.listdir(os.path.join(root, str(z))) if d.isdigit()):
            column = os.path.join(root, str(z), str(x))
            for name in sorted(os.listdir(column), key=lambda n: int(n.split(".")[0])):
                y = int(name.split(".")[0])
                with open(os.path.join(column, name), "rb") as handle:
                    yield z, x, y, handle.read()


def synthetic_tiles(z):
    side = 1 << z
    for x in range(side):
        for y in range(side):
            yield z, x, y, synthetic_payload(z, x, y)


def write(tiles, out, fmt):
    if os.path.exists(out):
        os.remove(out)
    db = sqlite3.connect(out)
    db.execute("PRAGMA journal_mode = OFF")
    db.execute("PRAGMA synchronous = OFF")
    db.execute("CREATE TABLE metadata (name text, value text)")
    db.execute(
        "CREATE TABLE tiles (zoom_level integer, tile_column integer, "
        "tile_row integer, tile_data blob)"
    )
    batch, lo, hi, count = [], 99, -1, 0
    for z, x, y, data in tiles:
        batch.append((z, x, (1 << z) - 1 - y, data))
        lo, hi, count = min(lo, z), max(hi, z), count + 1
        if len(batch) >= BATCH:
            db.executemany("INSERT INTO tiles VALUES (?, ?, ?, ?)", batch)
            batch = []
    if batch:
        db.executemany("INSERT INTO tiles VALUES (?, ?, ?, ?)", batch)
    if count == 0:
        sys.exit("no tiles to write")
    db.execute("CREATE UNIQUE INDEX tile_index ON tiles (zoom_level, tile_column, tile_row)")
    meta = {
        "name": "libviprs-bench writer sweep",
        "format": fmt,
        "type": "baselayer",
        "version": "1",
        "minzoom": str(lo),
        "maxzoom": str(hi),
        # The writer's own defaults, so the two headers describe one extent.
        "bounds": "-180.0,-85.0511287,180.0,85.0511287",
        "center": "0,0,%d" % lo,
    }
    db.executemany("INSERT INTO metadata VALUES (?, ?)", list(meta.items()))
    db.commit()
    db.close()
    print("%d tiles, zoom %d..%d" % (count, lo, hi))


def main(argv):
    if len(argv) == 4 and argv[1] == "tree":
        write(tree_tiles(argv[2]), argv[3], "png")
    elif len(argv) == 4 and argv[1] == "synthetic":
        write(synthetic_tiles(int(argv[2])), argv[3], "png")
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
