use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Password must contain between 12 and 128 bytes")]
    Password,
    #[error("Cryptographic operation failed")]
    Crypto,
}
pub fn hash_password(password: &str) -> Result<String, Error> {
    if !(12..=128).contains(&password.len()) {
        return Err(Error::Password);
    }
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|v| v.to_string())
        .map_err(|_| Error::Crypto)
}
pub fn verify_password(password: &str, hash: &str) -> bool {
    if password.len() > 128 {
        return false;
    }
    PasswordHash::new(hash).is_ok_and(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
}
pub fn token(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes))
}
pub fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}
pub fn constant_eq(a: &str, b: &str) -> bool {
    bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}
pub fn sign(secret: &str, timestamp: &str, body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).expect("HMAC accepts arbitrary keys");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}
pub fn verify_signature(secret: &str, timestamp: &str, body: &[u8], signature: &str) -> bool {
    let Ok(signature) = hex::decode(signature) else {
        return false;
    };
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).expect("HMAC accepts arbitrary keys");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    mac.verify_slice(&signature).is_ok()
}
/// GitHub-compatible `sha256=<hex>` signature over the raw body.
pub fn verify_body_signature(secret: &str, body: &[u8], header: &str) -> bool {
    let Some(signature) = header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(signature) = hex::decode(signature) else {
        return false;
    };
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).expect("HMAC accepts arbitrary keys");
    mac.update(body);
    mac.verify_slice(&signature).is_ok()
}
pub fn encrypt(key_hex: &str, value: &str) -> Result<String, Error> {
    let key = hex::decode(key_hex).map_err(|_| Error::Crypto)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| Error::Crypto)?;
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce), value.as_bytes())
        .map_err(|_| Error::Crypto)?;
    let mut output = nonce.to_vec();
    output.extend(encrypted);
    Ok(URL_SAFE_NO_PAD.encode(output))
}
pub fn decrypt(key_hex: &str, value: &str) -> Result<String, Error> {
    let key = hex::decode(key_hex).map_err(|_| Error::Crypto)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| Error::Crypto)?;
    let encoded = URL_SAFE_NO_PAD.decode(value).map_err(|_| Error::Crypto)?;
    if encoded.len() < 28 {
        return Err(Error::Crypto);
    }
    let bytes = cipher
        .decrypt(Nonce::from_slice(&encoded[..12]), &encoded[12..])
        .map_err(|_| Error::Crypto)?;
    String::from_utf8(bytes).map_err(|_| Error::Crypto)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn passwords_and_random_tokens() {
        let hash = hash_password("a long correct password").unwrap();
        assert!(verify_password("a long correct password", &hash));
        assert!(!verify_password("wrong", &hash));
        assert_ne!(token("ozy_"), token("ozy_"));
    }
    #[test]
    fn signatures_and_encryption_reject_tampering() {
        let key = "11".repeat(32);
        let encrypted = encrypt(&key, "test-secret").unwrap();
        assert_eq!(decrypt(&key, &encrypted).unwrap(), "test-secret");
        assert!(decrypt(&"22".repeat(32), &encrypted).is_err());
        let sig = sign("secret", "123", b"{}");
        assert!(verify_signature("secret", "123", b"{}", &sig));
        assert!(!verify_signature("secret", "123", b"{ }", &sig));
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(b"secret").unwrap();
        mac.update(b"{}");
        let github = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        assert!(verify_body_signature("secret", b"{}", &github));
        assert!(!verify_body_signature("secret", b"{ }", &github));
        assert!(!verify_body_signature("secret", b"{}", "sha1=00"));
    }
}
