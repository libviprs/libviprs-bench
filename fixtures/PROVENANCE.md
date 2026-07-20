# Benchmark fixture provenance

This directory is `.gitignore`d for working fixtures (large, locally-generated
`*.pdf` / `*.tiff` / `*.png` / … files are never tracked). The **one**
deliberately committed exception is a small, freely-licensed, real-content PDF
page — the first non-synthetic benchmark workload (issues #30 / #31). Every
committed fixture is pinned by SHA-256 here and enforced by
`tests/pdf_fixture.rs`, so it can never drift silently.

## `cc_licenses_mapping.pdf`

A single-page vector infographic, "Open Content — Creative Commons Licenses
Mapping (EN, colors)", used as the real-content workload for the rasterized-PDF
scalability series. It is dense vector content (tables, icons, colored
call-outs), so rasterizing it at increasing DPI is a meaningful stand-in for a
full-page blueprint — unlike the synthetic gradient it sits alongside.

| Field | Value |
| --- | --- |
| Source page | <https://commons.wikimedia.org/wiki/File:Open_Content_-_Creative_Commons_Licenses_Mapping_(EN,_colors).pdf> |
| Direct download | <https://upload.wikimedia.org/wikipedia/commons/e/e7/Open_Content_-_Creative_Commons_Licenses_Mapping_%28EN%2C_colors%29.pdf> |
| Author | Puersheng (Wikimedia Commons) |
| License | **CC0 1.0 Universal** (Public Domain Dedication) — <https://creativecommons.org/publicdomain/zero/1.0/> |
| Retrieved | 2026-07-20 |
| Size | 65056 bytes |
| Pages | 1 |
| Page size | 1190.52 × 841.861 pt (A3 landscape) |
| Producer | LibreOffice 6.0 |
| SHA-256 | `6012f1c07704f27014737da1585dd7780e215ae6e6df27a3804d4aacfa80db0d` |

**License note.** The compiled work is released under CC0 1.0 by its author, so
it may be redistributed for any purpose with no conditions. The source file
page additionally credits the embedded Creative Commons license symbols (©
Creative Commons) and the complementary Openmoji icons (CC BY-SA 4.0); this
fixture redistributes the work verbatim, unmodified, with full attribution
recorded here, which satisfies those upstream terms as well.

### Verifying the checksum

```sh
shasum -a 256 fixtures/cc_licenses_mapping.pdf
# → 6012f1c07704f27014737da1585dd7780e215ae6e6df27a3804d4aacfa80db0d
```

`tests/pdf_fixture.rs` enforces the same digest against the committed bytes
(`include_bytes!`) and asserts it appears in this note, mirroring how
`tests/pdfium_provenance.rs` pins the PDFium binary digests against the
Dockerfile. If the fixture is ever intentionally replaced, update the bytes,
the `FIXTURE_SHA256` constant in that test, and this table together.
