// SHA-256 for the storage archive's digests, over `sha2`.
//
// Two callers that cannot share a crate: `storage::integrity`, which digests the
// canonical JSON of a benchmark document, and `build.rs`, which digests
// `Cargo.lock` before the crate it belongs to exists. The build script pulls
// this file in with `include!`, so the file carries no inner doc comments and
// no `mod` wrapper, and `sha2` is listed under `[build-dependencies]` as well as
// `[dependencies]` for that second caller.
//
// This was 197 lines of hand-written SHA-256 for one session, and the reason it
// was written no longer holds. It was written because `[dependencies]` was not
// this lane's to edit; that has been reconciled, and checking rather than
// assuming turned up the thing that settles it: `sha2` 0.11 is ALREADY a normal
// dependency of `libviprs`, which is a normal path dependency of this crate, so
// it is compiled into the graph of every binary here today. Promoting it from
// dev-dependency to dependency adds no crates to anything. That leaves
// hand-written hashing in the one module whose entire job is to be trusted,
// bought for nothing.
//
// The `[build-dependencies]` entry does compile `sha2` a second time for the
// host, which is the whole cost and is worth paying.
//
// The known-answer tests below are unchanged. They are pointed at whatever
// ships, and they earned their keep: written against the hand-rolled version,
// they found that the published FIPS vectors alone never reach past two blocks.

use sha2::{Digest, Sha256};

/// SHA-256 of `bytes`, lowercase hex, no prefix.
///
/// The `sha256:` prefix the archive stores belongs to the caller: it is part of
/// the digest *format* the document declares, not part of the hash.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha256(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(char::from_digit((byte >> 4) as u32, 16).expect("nibble is < 16"));
        out.push(char::from_digit((byte & 0x0f) as u32, 16).expect("nibble is < 16"));
    }
    out
}

/// SHA-256 of `bytes` as raw octets.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(Sha256::digest(bytes).as_slice());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // RED against any arithmetic slip in the compression function. These are the
    // FIPS 180-4 worked examples plus the empty-string digest, and they are the
    // reason a hand-written hash is defensible at all: the answer is published,
    // so "it compiles" is not what this rests on.
    #[test]
    fn the_published_vectors_come_out_right() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    // RED against a chaining bug past two blocks, and against anything that
    // treats a byte as signed.
    //
    // Writing out the gap the other vectors leave, because a hand-rolled hash is
    // only as good as what its vectors reach. The three published examples and
    // the boundary walk above top out at TWO blocks and are all ASCII, so
    // between them they never exercise the `h` chaining across a third block,
    // never put a byte above 0x7f through the word packing, and never push the
    // length field past what fits in 32 bits. Documents digested here are tens
    // of kilobytes, which is hundreds of blocks, so the first of those was a
    // real hole rather than a theoretical one.
    //
    // Still not covered, and stated so nobody assumes otherwise: a message at or
    // beyond 2^32 bits, which is 512 MiB, where a length field truncated to 32
    // bits would go wrong and no document this suite produces would reach.
    // Expected values from `sha256sum` and Python's `hashlib` in an arm64
    // container, which agreed with each other.
    #[test]
    fn long_and_high_bit_messages_come_out_right() {
        // 1000 bytes: 16 blocks, so the chaining runs fourteen times past the
        // point the boundary walk stops.
        assert_eq!(
            sha256_hex(&vec![b'a'; 1000]),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
        // 100000 bytes: 1563 blocks, and a length field past 16 bits.
        assert_eq!(
            sha256_hex(&vec![b'a'; 100_000]),
            "6d1cf22d7cc09b085dfc25ee1a1f3ae0265804c607bc2074ad253bcc82fd81ee"
        );
        // Every byte value, four times over: 1024 bytes in which three quarters
        // of the words carry a high bit somewhere.
        let all_bytes: Vec<u8> = (0..=255u8).cycle().take(1024).collect();
        assert_eq!(
            sha256_hex(&all_bytes),
            "785b0751fc2c53dc14a4ce3d800e69ef9ce1009eb327ccf458afe09c242c26c9"
        );
    }

    // RED against a padding bug that only shows up at a block boundary, which is
    // the single most likely way to get this wrong and the one the three vectors
    // above (0, 3 and 56 bytes) do not reach. 55, 56, 63, 64 and 65 bytes walk
    // the boundary from "length fits in this block" to "needs a whole extra one".
    #[test]
    fn padding_is_right_across_the_block_boundary() {
        let cases: [(usize, &str); 5] = [
            (
                55,
                "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318",
            ),
            (
                56,
                "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a",
            ),
            (
                63,
                "7d3e74a05d7db15bce4ad9ec0658ea98e3f06eeecf16b4c6fff2da457ddc2f34",
            ),
            (
                64,
                "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb",
            ),
            (
                65,
                "635361c48bb9eab14198e76ea8ab7f1a41685d6ad62aa9146d301d4f17eb0ae0",
            ),
        ];
        for (len, expected) in cases {
            let message = vec![b'a'; len];
            assert_eq!(sha256_hex(&message), expected, "{len} bytes of 'a'");
        }
    }
}
