// SHA-256, by hand, in `std` alone.
//
// Two callers that cannot share a crate: `storage::integrity`, which digests the
// canonical JSON of a benchmark document, and `build.rs`, which digests
// `Cargo.lock` before the crate it belongs to exists. The build script pulls
// this file in with `include!`, so the file carries no inner doc comments and
// no `mod` wrapper.
//
// Writing a hash function rather than adding `sha2` is a deliberate call I do
// not get to make casually, so the reasons in order. `sha2` is already in this
// crate as a dev-dependency and promoting it to a normal one would mean editing
// `[dependencies]`, which is not this lane's to edit. A build script cannot use
// its own crate's library anyway, so `build.rs` would need `sha2` a second time
// under `[build-dependencies]`. And the standalone reasoning cuts the other way
// too: this is a content digest for archived benchmark documents, not a
// security boundary, and its correctness is pinned below against the FIPS 180-4
// example vectors and the two digests every developer has seen before.
//
// If a later lane does add `sha2` as a normal dependency, delete this file and
// keep the known-answer tests pointed at the replacement.

/// Round constants: the first 32 bits of the fractional parts of the cube roots
/// of the first 64 primes (FIPS 180-4 §4.2.2).
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Initial hash value: the first 32 bits of the fractional parts of the square
/// roots of the first eight primes (FIPS 180-4 §5.3.3).
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

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
    let mut h = H0;

    // Padding: a 1 bit, then zeros, then the message length in bits as a
    // big-endian u64, so that the total is a whole number of 64-byte blocks.
    let mut tail = Vec::with_capacity(128);
    tail.extend_from_slice(&bytes[bytes.len() - bytes.len() % 64..]);
    tail.push(0x80);
    while tail.len() % 64 != 56 {
        tail.push(0);
    }
    tail.extend_from_slice(&(bytes.len() as u64 * 8).to_be_bytes());

    let whole_blocks = bytes.len() - bytes.len() % 64;
    for chunk in bytes[..whole_blocks]
        .chunks_exact(64)
        .chain(tail.chunks_exact(64))
    {
        compress(&mut h, chunk);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// One 64-byte block through the compression function (FIPS 180-4 §6.2.2).
fn compress(h: &mut [u32; 8], block: &[u8]) {
    debug_assert_eq!(block.len(), 64);
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([
            block[i * 4],
            block[i * 4 + 1],
            block[i * 4 + 2],
            block[i * 4 + 3],
        ]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
    let (mut e, mut f, mut g, mut hh) = (h[4], h[5], h[6], h[7]);

    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = hh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);

        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
    h[5] = h[5].wrapping_add(f);
    h[6] = h[6].wrapping_add(g);
    h[7] = h[7].wrapping_add(hh);
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
