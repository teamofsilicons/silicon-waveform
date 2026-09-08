//! Authenticated encryption for retrievable application secrets.

use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead as _, AeadCore as _, KeyInit as _, OsRng, Payload},
};
use hmac::{Hmac, Mac as _};
use secrecy::{ExposeSecret as _, SecretString};
use sha2::Sha256;
use zeroize::Zeroizing;

/// Encryption key held outside PostgreSQL. Associated data binds every secret
/// to its plane, owner and purpose, preventing encrypted-row substitution.
pub struct Vault {
    key: Zeroizing<[u8; 32]>,
}

impl Vault {
    /// Parses a 32-byte lowercase hex deployment key without retaining its text.
    pub fn new(encoded: &SecretString) -> Result<Self, &'static str> {
        let raw = encoded.expose_secret().as_bytes();
        if raw.len() != 64 {
            return Err("encryption key must be 64 hexadecimal characters");
        }
        let mut key = Zeroizing::new([0_u8; 32]);
        for (index, pair) in raw.as_chunks::<2>().0.iter().enumerate() {
            let digit = |byte: u8| match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                _ => None,
            };
            key[index] = digit(pair[0]).ok_or("invalid encryption key")? * 16
                + digit(pair[1]).ok_or("invalid encryption key")?;
        }
        Ok(Self { key })
    }

    /// Encrypts a secret with a fresh random 192-bit nonce.
    pub fn seal(&self, value: &str, context: &str) -> Result<Vec<u8>, &'static str> {
        let cipher = XChaCha20Poly1305::new((&*self.key).into());
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let mut stored = nonce.to_vec();
        stored.extend(
            cipher
                .encrypt(
                    &nonce,
                    Payload {
                        msg: value.as_bytes(),
                        aad: context.as_bytes(),
                    },
                )
                .map_err(|_| "secret encryption failed")?,
        );
        Ok(stored)
    }

    /// Decrypts a secret only when its original storage context matches.
    pub fn open(&self, stored: &[u8], context: &str) -> Result<SecretString, &'static str> {
        if stored.len() < 40 {
            return Err("invalid encrypted secret");
        }
        let cipher = XChaCha20Poly1305::new((&*self.key).into());
        let plain = Zeroizing::new(
            cipher
                .decrypt(
                    XNonce::from_slice(&stored[..24]),
                    Payload {
                        msg: &stored[24..],
                        aad: context.as_bytes(),
                    },
                )
                .map_err(|_| "secret decryption failed")?,
        );
        let text = std::str::from_utf8(&plain).map_err(|_| "invalid encrypted secret")?;
        Ok(SecretString::from(text.to_owned()))
    }

    /// Creates an indexed digest without disclosing secret material to the DB.
    pub fn digest(&self, value: &str, purpose: &str) -> Result<Vec<u8>, &'static str> {
        let mut mac = <Hmac<Sha256> as hmac::Mac>::new_from_slice(&*self.key)
            .map_err(|_| "secret digest failed")?;
        mac.update(purpose.as_bytes());
        mac.update(b"\0");
        mac.update(value.as_bytes());
        Ok(mac.finalize().into_bytes().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ciphertext_is_randomized_and_bound_to_its_owner() -> Result<(), &'static str> {
        let vault = Vault::new(&SecretString::from("19".repeat(32)))?;
        let first = vault.seal("provider-secret", "plane/actor/openai")?;
        let second = vault.seal("provider-secret", "plane/actor/openai")?;
        assert_ne!(first, second);
        assert_eq!(
            vault.open(&first, "plane/actor/openai")?.expose_secret(),
            "provider-secret"
        );
        assert!(vault.open(&first, "different/actor/openai").is_err());
        let other = Vault::new(&SecretString::from("20".repeat(32)))?;
        assert!(other.open(&first, "plane/actor/openai").is_err());
        assert_ne!(vault.digest("key", "root")?, vault.digest("key", "job")?);
        Ok(())
    }
}
