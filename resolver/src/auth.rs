use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::http::{HeaderMap, header::COOKIE};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, AeadCore, OsRng, Payload},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

const SESSION_COOKIE: &str = "__Host-movi-session";

#[derive(Clone)]
pub struct Tokens(XChaCha20Poly1305);

#[derive(Serialize, Deserialize)]
struct Claims<T> {
    expires: u64,
    value: T,
}

impl Tokens {
    pub fn new(secret: &str) -> Option<Self> {
        let bytes = URL_SAFE_NO_PAD.decode(secret).ok()?;
        Some(Self(XChaCha20Poly1305::new_from_slice(&bytes).ok()?))
    }

    pub fn seal<T: Serialize>(&self, kind: &[u8], value: T, seconds: u64) -> String {
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let message = serde_json::to_vec(&Claims {
            expires: now() + seconds,
            value,
        })
        .expect("application claims must be serializable");
        let ciphertext = self
            .0
            .encrypt(
                &nonce,
                Payload {
                    msg: &message,
                    aad: kind,
                },
            )
            .expect("application claims fit within the cipher limit");
        let mut token = nonce.to_vec();
        token.extend(ciphertext);
        URL_SAFE_NO_PAD.encode(token)
    }

    pub fn open<T: DeserializeOwned>(&self, kind: &[u8], token: &str) -> Option<T> {
        if token.len() > 16_384 {
            return None;
        }
        let bytes = URL_SAFE_NO_PAD.decode(token).ok()?;
        let (nonce, ciphertext) = bytes.split_at_checked(24)?;
        let message = self
            .0
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad: kind,
                },
            )
            .ok()?;
        let claims: Claims<T> = serde_json::from_slice(&message).ok()?;
        (claims.expires > now()).then_some(claims.value)
    }

    pub fn session(&self, headers: &HeaderMap) -> Option<String> {
        let cookies = headers.get(COOKIE)?.to_str().ok()?;
        let value = cookies.split(';').find_map(|cookie| {
            let (name, value) = cookie.trim().split_once('=')?;
            (name == SESSION_COOKIE).then_some(value)
        })?;
        self.open(b"session", value)
    }
}

pub fn cookie(token: &str, max_age: u64) -> String {
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age={max_age}"
    )
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn check_password(users: &HashMap<String, String>, username: &str, password: &str) -> bool {
    let Some(hash) = users.get(username).or_else(|| users.values().next()) else {
        return false;
    };
    let valid = PasswordHash::new(hash).ok().is_some_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    });
    valid && users.contains_key(username)
}

pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| error.to_string())
}
