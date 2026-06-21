//! A fast, std-only hasher (FNV-1a) for the interpreter's hot maps.
//!
//! The default `HashMap` hasher (SipHash) is DoS-resistant but relatively slow,
//! and the interpreter hashes short variable-name keys on *every* variable read
//! and write — the dominant per-iteration cost in tight loops. FNV-1a is a
//! tiny, fast, non-cryptographic hash that is a good fit for the small,
//! trusted, short-string keys used by scope frames (it is NOT used for any
//! attacker-controlled or network-facing map).
//!
//! Zero external crates: this implements `Hasher` + `BuildHasher` directly.

use std::hash::{BuildHasherDefault, Hasher};

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a hasher.
pub struct FnvHasher(u64);

impl Default for FnvHasher {
    #[inline]
    fn default() -> Self {
        FnvHasher(FNV_OFFSET)
    }
}

impl Hasher for FnvHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut hash = self.0;
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        self.0 = hash;
    }
}

/// `BuildHasher` that produces [`FnvHasher`]s. Use as the hasher type parameter
/// of a `HashMap` for fast hashing of short, trusted string keys.
pub type FnvBuildHasher = BuildHasherDefault<FnvHasher>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn fnv_map_roundtrips() {
        let mut m: HashMap<String, i32, FnvBuildHasher> = HashMap::default();
        m.insert("total".to_string(), 1);
        m.insert("i".to_string(), 2);
        assert_eq!(m.get("total"), Some(&1));
        assert_eq!(m.get("i"), Some(&2));
        assert_eq!(m.get("missing"), None);
        *m.get_mut("total").unwrap() += 41;
        assert_eq!(m.get("total"), Some(&42));
    }

    #[test]
    fn distinct_keys_mostly_distinct_hashes() {
        // Sanity: different short keys should not all collide to one bucket.
        let mut h1 = FnvHasher::default();
        h1.write(b"total");
        let mut h2 = FnvHasher::default();
        h2.write(b"i");
        assert_ne!(h1.finish(), h2.finish());
    }
}
