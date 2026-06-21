//! Code generation module - Produces standalone, self-contained binaries.
//!
//! Strategy: Compress and obfuscate the .ran source with a stream cipher, then
//! embed the result into a stripped copy of the `ran` runtime binary.
//!
//! IMPORTANT - threat model (read before relying on this):
//!   This is OBFUSCATION, not secure encryption. The cipher key is derived
//!   from a constant compiled into the binary, so anyone with the binary and
//!   this source can recover the embedded program. It defeats casual
//!   inspection (`strings`, hexdump, basic decompilers) but NOT a determined
//!   reverse engineer. Do not embed secrets in `.ran` source and assume they
//!   are protected. For real secret protection use runtime secret management
//!   (environment variables, a secrets manager), not source embedding.
//!
//! What this DOES provide:
//!   - Source is not readable via `strings`/hexdump
//!   - Binary is stripped of debug symbols
//!   - A single self-contained executable with no external runtime dependency
//!
//! Binary layout:
//!   [stripped ran binary][compressed+obfuscated source][nonce:16][size:u64 LE][RANENCv3]

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use crate::support::crypto::{compress, decompress, decrypt, derive_key, encrypt, sha256};

/// Magic marker for compressed+encrypted format (v3)
const RAN_MAGIC_V3: &[u8; 8] = b"RANENCv3";

/// Salt for key derivation (embedded; security comes from the cipher + KDF rounds)
const KDF_SALT: &[u8] = b"ran-lang-v0.2-source-protection";

/// Compile a .ran source into a standalone encrypted native binary.
pub fn compile_standalone(source: &str, output: &str) -> bool {
    let exe_path = match env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ran: cannot locate ran binary: {}", e);
            return false;
        }
    };

    let mut binary_data = match fs::read(&exe_path) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("ran: cannot read ran binary: {}", e);
            return false;
        }
    };

    // Remove any existing embedded payload to get the clean base binary
    binary_data = strip_embedded(&binary_data);

    // Write clean base, then strip symbols
    if let Err(e) = fs::write(output, &binary_data) {
        eprintln!("ran: cannot write output binary: {}", e);
        return false;
    }
    if let Err(e) = fs::set_permissions(output, fs::Permissions::from_mode(0o755)) {
        eprintln!("ran: cannot set executable permission: {}", e);
        return false;
    }
    // Use `--` so an output name beginning with '-' isn't read as a flag.
    let _ = std::process::Command::new("strip").arg("--").arg(output).output();

    // Re-read the stripped binary
    let mut final_data = match fs::read(output) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("ran: cannot re-read stripped binary: {}", e);
            return false;
        }
    };

    // --- Compress then encrypt the source ---
    let compressed = compress(source.as_bytes());
    let key = derive_key(KDF_SALT, KDF_SALT);
    let nonce = generate_nonce(source.as_bytes());
    let ciphertext = encrypt(&compressed, &key, &nonce);

    // Append: [ciphertext][nonce:16][size:u64 LE][RANENCv3]
    let size = ciphertext.len() as u64;
    final_data.extend_from_slice(&ciphertext);
    final_data.extend_from_slice(&nonce);
    final_data.extend_from_slice(&size.to_le_bytes());
    final_data.extend_from_slice(RAN_MAGIC_V3);

    if let Err(e) = fs::write(output, &final_data) {
        eprintln!("ran: cannot write final binary: {}", e);
        return false;
    }

    true
}

/// Generate a deterministic-yet-unique nonce from the source content.
fn generate_nonce(source: &[u8]) -> [u8; 16] {
    let hash = sha256(source);
    let mut nonce = [0u8; 16];
    nonce.copy_from_slice(&hash[..16]);
    nonce
}

/// Extract, decrypt, and decompress embedded source from a compiled binary.
pub fn extract_embedded_source(binary_data: &[u8]) -> Option<String> {
    if binary_data.len() < 32 {
        return None;
    }

    // Check magic marker
    let tail = &binary_data[binary_data.len() - 8..];
    if tail != RAN_MAGIC_V3 {
        return None;
    }

    // Read size (8 bytes before magic)
    let size_start = binary_data.len() - 16;
    let size_bytes: [u8; 8] = binary_data[size_start..size_start + 8].try_into().ok()?;
    let size = u64::from_le_bytes(size_bytes) as usize;

    // Read nonce (16 bytes before size)
    let nonce_start = size_start - 16;
    let nonce: [u8; 16] = binary_data[nonce_start..nonce_start + 16].try_into().ok()?;

    // Read ciphertext (size bytes before nonce)
    let cipher_start = nonce_start.checked_sub(size)?;
    let ciphertext = &binary_data[cipher_start..nonce_start];

    // Decrypt then decompress
    let key = derive_key(KDF_SALT, KDF_SALT);
    let compressed = decrypt(ciphertext, &key, &nonce);
    let plaintext = decompress(&compressed);

    String::from_utf8(plaintext).ok()
}

/// Strip any existing embedded payload (v3 format) from binary data.
fn strip_embedded(data: &[u8]) -> Vec<u8> {
    if data.len() < 32 {
        return data.to_vec();
    }

    let tail = &data[data.len() - 8..];
    if tail != RAN_MAGIC_V3 {
        return data.to_vec();
    }

    let size_start = data.len() - 16;
    if let Ok(size_bytes) = <[u8; 8]>::try_from(&data[size_start..size_start + 8]) {
        let size = u64::from_le_bytes(size_bytes) as usize;
        // payload = ciphertext + nonce(16) + size(8) + magic(8)
        let payload_total = size + 16 + 8 + 8;
        if payload_total <= data.len() {
            return data[..data.len() - payload_total].to_vec();
        }
    }

    data.to_vec()
}
