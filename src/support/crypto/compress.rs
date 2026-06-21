//! Lightweight LZ77-style compression - pure Rust, zero dependencies.
//!
//! Used to compress source code before encryption so compiled binaries
//! stay small. The format is a simple byte stream of tokens:
//!   - Literal:  0x00 <byte>
//!   - Match:    0x01 <offset:u16 BE> <length:u8>
//!
//! This is not the most efficient scheme, but it is simple, correct, and
//! removes a meaningful fraction of redundancy in typical source code.

const WINDOW_SIZE: usize = 4096;
const MIN_MATCH: usize = 4;
const MAX_MATCH: usize = 255;

/// Compress a byte slice. Returns the compressed stream.
pub fn compress(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut pos = 0;

    while pos < input.len() {
        let (best_len, best_offset) = find_longest_match(input, pos);

        if best_len >= MIN_MATCH {
            // Emit a match token
            out.push(0x01);
            let offset = best_offset as u16;
            out.push((offset >> 8) as u8);
            out.push((offset & 0xFF) as u8);
            out.push(best_len as u8);
            pos += best_len;
        } else {
            // Emit a literal token
            out.push(0x00);
            out.push(input[pos]);
            pos += 1;
        }
    }

    out
}

/// Decompress a stream produced by `compress`.
pub fn decompress(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 2);
    let mut pos = 0;

    while pos < input.len() {
        let tag = input[pos];
        pos += 1;

        match tag {
            0x00 => {
                // Literal
                if pos < input.len() {
                    out.push(input[pos]);
                    pos += 1;
                }
            }
            0x01 => {
                // Match: offset(u16) + length(u8)
                if pos + 2 < input.len() {
                    let offset = ((input[pos] as usize) << 8) | (input[pos + 1] as usize);
                    let length = input[pos + 2] as usize;
                    pos += 3;

                    let start = out.len().saturating_sub(offset);
                    for i in 0..length {
                        if start + i < out.len() {
                            let byte = out[start + i];
                            out.push(byte);
                        }
                    }
                }
            }
            _ => break,
        }
    }

    out
}

/// Find the longest match for the data at `pos` within the sliding window.
fn find_longest_match(input: &[u8], pos: usize) -> (usize, usize) {
    let window_start = pos.saturating_sub(WINDOW_SIZE);
    let max_len = (input.len() - pos).min(MAX_MATCH);

    if max_len < MIN_MATCH {
        return (0, 0);
    }

    let mut best_len = 0;
    let mut best_offset = 0;

    let mut search = window_start;
    while search < pos {
        let mut len = 0;
        while len < max_len && input[search + len] == input[pos + len] {
            len += 1;
        }
        if len > best_len {
            best_len = len;
            best_offset = pos - search;
            if len == max_len {
                break;
            }
        }
        search += 1;
    }

    (best_len, best_offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip_simple() {
        let input = b"hello hello hello world world";
        let compressed = compress(input);
        let decompressed = decompress(&compressed);
        assert_eq!(&decompressed[..], &input[..]);
    }

    #[test]
    fn test_roundtrip_source() {
        let input = b"fn main() {\n    echo \"Hello, World!\"\n    echo \"Hello, World!\"\n}";
        let compressed = compress(input);
        let decompressed = decompress(&compressed);
        assert_eq!(&decompressed[..], &input[..]);
    }

    #[test]
    fn test_roundtrip_empty() {
        let input = b"";
        let compressed = compress(input);
        let decompressed = decompress(&compressed);
        assert_eq!(decompressed.len(), 0);
    }

    #[test]
    fn test_compresses_repetitive() {
        let input = vec![b'a'; 1000];
        let compressed = compress(&input);
        // Highly repetitive data should compress significantly
        assert!(compressed.len() < input.len() / 2);
        let decompressed = decompress(&compressed);
        assert_eq!(decompressed, input);
    }
}
