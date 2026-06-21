//! Public hashing/encoding helpers exposed to Ran via the `std::crypto` module.
//!
//! Built on the in-tree SHA-256 (FIPS 180-4). HMAC follows RFC 2104; Base64 is
//! standard (RFC 4648) with `=` padding. Pure Rust, no external dependencies.

use super::sha256::sha256;

const HMAC_BLOCK: usize = 64; // SHA-256 block size in bytes

/// HMAC-SHA256(key, message) -> 32-byte tag.
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    // Normalize the key to one block.
    let mut k = [0u8; HMAC_BLOCK];
    if key.len() > HMAC_BLOCK {
        let digest = sha256(key);
        k[..32].copy_from_slice(&digest);
    } else {
        k[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; HMAC_BLOCK];
    let mut opad = [0x5cu8; HMAC_BLOCK];
    for i in 0..HMAC_BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }

    // inner = sha256(ipad || msg)
    let mut inner = Vec::with_capacity(HMAC_BLOCK + msg.len());
    inner.extend_from_slice(&ipad);
    inner.extend_from_slice(msg);
    let inner_digest = sha256(&inner);

    // outer = sha256(opad || inner_digest)
    let mut outer = Vec::with_capacity(HMAC_BLOCK + 32);
    outer.extend_from_slice(&opad);
    outer.extend_from_slice(&inner_digest);
    sha256(&outer)
}

/// Lowercase hex encoding of arbitrary bytes.
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard Base64 encoding (RFC 4648) with `=` padding.
pub fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[((n >> 18) & 63) as usize] as char);
        out.push(B64[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Standard Base64 decoding. Returns None on invalid input.
pub fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let clean: Vec<u8> = input.bytes().filter(|&b| b != b'\n' && b != b'\r').collect();
    let mut out = Vec::new();
    for chunk in clean.chunks(4) {
        if chunk.len() < 2 {
            return None;
        }
        let c0 = val(chunk[0])?;
        let c1 = val(chunk[1])?;
        out.push(((c0 << 2) | (c1 >> 4)) as u8);
        if chunk.len() >= 3 && chunk[2] != b'=' {
            let c2 = val(chunk[2])?;
            out.push((((c1 & 15) << 4) | (c2 >> 2)) as u8);
            if chunk.len() >= 4 && chunk[3] != b'=' {
                let c3 = val(chunk[3])?;
                out.push((((c2 & 3) << 6) | c3) as u8);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::super::sha256::sha256_hex;
    use super::*;

    #[test]
    fn hmac_rfc4231_case2() {
        // RFC 4231 Test Case 2.
        let tag = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex_encode(&tag),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn sha256_known() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn base64_roundtrip() {
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("Zm9vYmFy").unwrap(), b"foobar");
    }

    #[test]
    fn hex_encodes() {
        assert_eq!(hex_encode(&[0x00, 0xff, 0x10]), "00ff10");
    }
}
