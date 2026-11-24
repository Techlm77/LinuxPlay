//! Passwords and tokens: hashing what must be kept, and making what must be
//! unguessable.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Bytes of randomness in a session token.
const TOKEN_BYTES: usize = 32;
/// Bytes of randomness in a public id (device, invite, tunnel).
const ID_BYTES: usize = 12;
const SALT_BYTES: usize = 16;

fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

/// A new secret token, as text.
pub fn new_token() -> String {
    URL_SAFE_NO_PAD.encode(random::<TOKEN_BYTES>())
}

/// A new unguessable id, as text that is safe in a URL.
pub fn new_id() -> String {
    URL_SAFE_NO_PAD.encode(random::<ID_BYTES>())
}

/// What is stored for a token. Tokens are random and long, so a plain hash
/// is enough: a stolen database doesn't yield tokens that work.
pub fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Hash a password for storage with Argon2id. Deliberately slow: call it off
/// the async threads.
pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::encode_b64(&random::<SALT_BYTES>())?;
    Ok(Argon2::default().hash_password(password.as_bytes(), &salt)?.to_string())
}

/// Does `password` match the stored hash? As slow as hashing.
pub fn verify_password(password: &str, stored: &str) -> bool {
    PasswordHash::new(stored).is_ok_and(|hash| Argon2::default().verify_password(password.as_bytes(), &hash).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_verifies_only_against_its_own_hash() {
        let stored = hash_password("correct horse battery").unwrap();
        assert!(stored.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery", &stored));
        assert!(!verify_password("correct horse batterz", &stored));
        assert!(!verify_password("anything", "not a hash"));
        // Salted: the same password hashes differently each time.
        assert_ne!(stored, hash_password("correct horse battery").unwrap());
    }

    #[test]
    fn tokens_and_ids_are_unique_and_url_safe() {
        let (a, b) = (new_token(), new_token());
        assert_ne!(a, b);
        assert!(a.len() >= 43 && a.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
        assert_ne!(new_id(), new_id());
        assert_eq!(token_hash(&a), token_hash(&a));
        assert_ne!(token_hash(&a), token_hash(&b));
    }
}
