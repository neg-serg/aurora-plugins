//! The wire's one binary form: base64, spelled out because a plugin has no dependencies.
//!
//! Two payloads are bytes rather than numbers: a waterfall picture (18 KB of levels, which JSON
//! would spell as 100 KB of decimals) and the scope window (2048 little-endian `f32`). Both cross
//! a pipe between two processes on one machine, so the alphabet is the standard one and the
//! padding is the standard `=`; nothing here is URL-safe, and nothing is compressed.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode bytes as base64 with `=` padding.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Decode base64, padding optional (a writer may leave it off; a reader has the length already).
///
/// `None` for a character outside the alphabet, for a length that cannot be a payload (one more
/// than a multiple of four), and for a malformed final quantum — a broken picture is skipped by
/// the caller, never half-drawn.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 2);
    let mut quantum = [0u8; 4];
    let mut filled = 0;
    for &byte in bytes {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        quantum[filled] = value;
        filled += 1;
        if filled == 4 {
            let n = (u32::from(quantum[0]) << 18)
                | (u32::from(quantum[1]) << 12)
                | (u32::from(quantum[2]) << 6)
                | u32::from(quantum[3]);
            out.push((n >> 16) as u8);
            out.push((n >> 8) as u8);
            out.push(n as u8);
            filled = 0;
        }
    }
    match filled {
        0 => {}
        2 => {
            let n = (u32::from(quantum[0]) << 18) | (u32::from(quantum[1]) << 12);
            out.push((n >> 16) as u8);
        }
        3 => {
            let n = (u32::from(quantum[0]) << 18)
                | (u32::from(quantum[1]) << 12)
                | (u32::from(quantum[2]) << 6);
            out.push((n >> 16) as u8);
            out.push((n >> 8) as u8);
        }
        // One leftover character cannot be a payload.
        _ => return None,
    }
    Some(out)
}

/// A slice of `f32` as the wire carries it: little-endian, four bytes each.
pub fn encode_samples(samples: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    encode(&bytes)
}

/// The other half of [`encode_samples`]. A payload whose length is not a multiple of four is
/// refused rather than padded with zeros: a sample that was never sent is not silence.
pub fn decode_samples(text: &str) -> Option<Vec<f32>> {
    let bytes = decode(text)?;
    if bytes.len() % 4 != 0 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::{decode, decode_samples, encode, encode_samples};

    /// The reference vectors every base64 implementation is checked against.
    #[test]
    fn the_reference_vectors_round_trip() {
        let cases: [(&[u8], &str); 7] = [
            (b"", ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ];
        for (bytes, text) in cases {
            assert_eq!(encode(bytes), text);
            assert_eq!(decode(text).as_deref(), Some(bytes));
        }
    }

    #[test]
    fn the_padding_may_be_left_off_and_a_broken_quantum_is_refused() {
        assert_eq!(decode("Zg").as_deref(), Some(&b"f"[..]));
        assert_eq!(decode("Zm8").as_deref(), Some(&b"fo"[..]));
        assert_eq!(decode("Z"), None);
        assert_eq!(decode("Zg$="), None);
        assert_eq!(decode("Zg==").as_deref(), Some(&b"f"[..]));
    }

    #[test]
    fn a_sample_window_survives_the_round_trip() {
        let samples: Vec<f32> = vec![0.0, 0.5, -0.25, 1.0, -1.0, 0.125];
        let text = encode_samples(&samples);
        assert_eq!(decode_samples(&text), Some(samples));
        // Three bytes are not a sample.
        assert_eq!(decode_samples("Zm9v"), None);
    }
}
