//! MD5, because `\pdfmdfivesum` is a pdfTeX primitive and packages branch on it.
//!
//! RFC 1321. Not a security primitive here and not used as one: pdfTeX exposes
//! it so a document can tell whether a file changed between runs, and
//! `rerunfilecheck` — which `hyperref` loads — aborts its own loading when the
//! primitive is missing, taking `hyperref` down with it.
//!
//! Sixty lines and no dependency, which is the whole reason it is written out
//! rather than pulled in: `aitex-engine` has none and is `no_std`.

/// The four `T[i] = floor(2^32 × |sin(i+1)|)` constants, RFC 1321 §3.4.
const T: [u32; 64] = [
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

/// Left-rotation amounts, one per step, four per round.
const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// The 16 bytes of `md5(bytes)`.
pub fn digest(bytes: &[u8]) -> [u8; 16] {
    let mut h: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];

    // The message, padded: a `0x80` byte, zeros to 56 mod 64, then the original
    // length in *bits*, little-endian. Building it costs one copy of the input,
    // which for the sizes a document hashes is not worth avoiding.
    let mut msg = alloc::vec::Vec::with_capacity(bytes.len() + 72);
    msg.extend_from_slice(bytes);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    let bits = (bytes.len() as u64).wrapping_mul(8);
    msg.extend_from_slice(&bits.to_le_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            let b = &chunk[i * 4..i * 4 + 4];
            *w = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        }
        let [mut a, mut b, mut c, mut d] = h;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(T[i])
                    .wrapping_add(m[g])
                    .rotate_left(S[i]),
            );
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
    }

    let mut out = [0u8; 16];
    for (i, w) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    out
}

/// `md5(bytes)` as pdfTeX prints it: 32 **uppercase** hex digits.
///
/// The case is not cosmetic. `rerunfilecheck` compares the sum against literals
/// it ships — `A94A2480D3289E625EEA47CD1B285758` for an empty Unix `.aux` — so
/// a lowercase digest would report every file as changed, every run.
pub fn hex(bytes: &[u8]) -> alloc::string::String {
    let d = digest(bytes);
    let mut s = alloc::string::String::with_capacity(32);
    for b in d {
        for nyb in [b >> 4, b & 0xf] {
            s.push(
                char::from_digit(u32::from(nyb), 16)
                    .unwrap_or('0')
                    .to_ascii_uppercase(),
            );
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 1321 §A.5's own test suite.
    #[test]
    fn the_rfc_test_vectors() {
        assert_eq!(hex(b""), "D41D8CD98F00B204E9800998ECF8427E");
        assert_eq!(hex(b"a"), "0CC175B9C0F1B6A831C399E269772661");
        assert_eq!(hex(b"abc"), "900150983CD24FB0D6963F7D28E17F72");
        assert_eq!(hex(b"message digest"), "F96B697D7CB7938D525A2F31AAF161D0");
        assert_eq!(
            hex(b"abcdefghijklmnopqrstuvwxyz"),
            "C3FCD3D76192E4007DFB496CCA67E13B"
        );
        assert_eq!(
            hex(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            ),
            "57EDF4A22BE3C955AC49DA2E2107B67A"
        );
    }

    /// `rerunfilecheck` carries this literal for an empty `.aux` on Unix — a
    /// newline — and compares against it every run.
    #[test]
    fn an_empty_unix_aux_matches_what_rerunfilecheck_expects() {
        assert_eq!(hex(b"\\relax \n"), "A94A2480D3289E625EEA47CD1B285758");
    }

    /// Padding is the part that is easy to get wrong, and it changes shape at
    /// 55, 56 and 64 bytes: a message that exactly fills a block needs a whole
    /// extra one for the length.
    #[test]
    fn the_block_boundaries_pad_correctly() {
        assert_eq!(hex(&[b'x'; 55]), "04364420E25C512FD958A70738AA8F72");
        assert_eq!(hex(&[b'x'; 56]), "668A72D5BA17F08E62DABCAFAD6DB14B");
        assert_eq!(hex(&[b'x'; 64]), "C1BB4F81D892B2D57947682AEB252456");
    }
}
