//! RFC 6238 TOTP (SHA-1, 6 digits, 30 s) for req.hebnix.com's app token.

use hmac::{Hmac, Mac};
use sha1::Sha1;
use std::time::{SystemTime, UNIX_EPOCH};

const STEP_SECONDS: u64 = 30;
const DIGITS: u32 = 6;

/// The secret baked in at build time; empty when the build had none.
fn embedded_secret() -> &'static str {
    env!("HEBNIX_REQ_KEY")
}

/// Current token, or None when this build carries no secret.
pub fn current_token() -> Option<String> {
    let secret = embedded_secret();
    if secret.is_empty() {
        return None;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    token_at(secret, now)
}

fn base32_decode(input: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut buffer = 0u32;
    let mut out = Vec::new();
    for c in input.bytes().filter(|c| *c != b'=') {
        let value = match c.to_ascii_uppercase() {
            c @ b'A'..=b'Z' => c - b'A',
            c @ b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        buffer = (buffer << 5) | u32::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn token_at(secret: &str, unix_seconds: u64) -> Option<String> {
    let key = base32_decode(secret)?;
    let counter = (unix_seconds / STEP_SECONDS).to_be_bytes();
    let mut mac = Hmac::<Sha1>::new_from_slice(&key).ok()?;
    mac.update(&counter);
    let digest = mac.finalize().into_bytes();
    let offset = (digest[19] & 0x0f) as usize;
    let binary = u32::from_be_bytes([
        digest[offset] & 0x7f,
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]);
    Some(format!(
        "{:0width$}",
        binary % 10u32.pow(DIGITS),
        width = DIGITS as usize
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6238 appendix B, SHA-1 secret "12345678901234567890", 8 digits
    // there; the 6-digit value is the last six digits of the same code.
    const RFC_SECRET: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    #[test]
    fn matches_rfc_6238_vectors() {
        assert_eq!(token_at(RFC_SECRET, 59).as_deref(), Some("287082"));
        assert_eq!(token_at(RFC_SECRET, 1111111109).as_deref(), Some("081804"));
        assert_eq!(token_at(RFC_SECRET, 20000000000).as_deref(), Some("353130"));
    }

    #[test]
    fn rejects_non_base32_secrets() {
        assert_eq!(token_at("not base32!", 0), None);
    }
}
