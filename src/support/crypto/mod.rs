//! Cryptography module - pure Rust, zero dependencies.
//!
//! Provides:
//! - SHA-256 hashing (FIPS 180-4 compliant)
//! - ChaCha20-style stream cipher for source encryption
//! - Key derivation from passphrase
//!
//! Used to encrypt embedded source code in compiled binaries so the
//! original `.ran` source cannot be recovered via `strings` or hex dumps.

mod sha256;
mod cipher;
mod compress;
mod extra;

#[allow(unused_imports)]
pub use sha256::{sha256, sha256_hex};
pub use cipher::{encrypt, decrypt, derive_key};
pub use compress::{compress, decompress};
pub use extra::{hmac_sha256, hex_encode, base64_encode, base64_decode};
