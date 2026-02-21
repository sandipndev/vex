use anyhow::Result;
use chrono::{DateTime, Utc};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_to_bytes(hex: &str) -> Result<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        anyhow::bail!("odd-length hex string");
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(Into::into))
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub token_id: String,
    /// blake3 hash of the 32-byte secret, hex-encoded; never stored plaintext
    pub token_secret_hash: String,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_seen: Option<DateTime<Utc>>,
}

pub struct TokenStore {
    path: PathBuf,
    tokens: Vec<Token>,
}

impl TokenStore {
    pub fn load(path: PathBuf) -> Result<Self> {
        if path.exists() {
            let data = std::fs::read_to_string(&path)?;
            let tokens: Vec<Token> = serde_json::from_str(&data)?;
            Ok(Self { path, tokens })
        } else {
            Ok(Self { path, tokens: vec![] })
        }
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(&self.tokens)?;
        std::fs::write(&self.path, data)?;
        Ok(())
    }

    /// Generate a new token. Returns the Token metadata and the plaintext secret.
    pub fn generate(
        &mut self,
        label: Option<String>,
        expire_secs: Option<u64>,
    ) -> Result<(Token, String)> {
        // token_id: tok_<6 hex chars> from 3 random bytes
        let mut id_bytes = [0u8; 3];
        OsRng.fill_bytes(&mut id_bytes);
        let token_id = format!("tok_{}", bytes_to_hex(&id_bytes));

        // token_secret: 64 hex chars (32 random bytes)
        let mut secret_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut secret_bytes);
        let token_secret = bytes_to_hex(&secret_bytes);

        // Store blake3 hash of the raw bytes
        let hash = blake3::hash(&secret_bytes);
        let token_secret_hash = bytes_to_hex(hash.as_bytes());

        let expires_at = expire_secs
            .map(|s| Utc::now() + chrono::Duration::seconds(s as i64));

        let token = Token {
            token_id,
            token_secret_hash,
            label,
            created_at: Utc::now(),
            expires_at,
            last_seen: None,
        };

        self.tokens.push(token.clone());
        self.save()?;
        Ok((token, token_secret))
    }

    /// Validate a presented secret against the stored hash. Updates last_seen on success.
    pub fn validate(&mut self, token_id: &str, secret: &str) -> bool {
        let secret_bytes = match hex_to_bytes(secret) {
            Ok(b) => b,
            Err(_) => return false,
        };
        let hash = blake3::hash(&secret_bytes);
        let presented_hash = bytes_to_hex(hash.as_bytes());

        if let Some(token) = self.tokens.iter_mut().find(|t| t.token_id == token_id) {
            if let Some(expires_at) = token.expires_at
                && Utc::now() > expires_at
            {
                return false;
            }
            if token.token_secret_hash == presented_hash {
                token.last_seen = Some(Utc::now());
                let _ = self.save();
                return true;
            }
        }
        false
    }

    pub fn list(&self) -> &[Token] {
        &self.tokens
    }

    pub fn revoke(&mut self, token_id: &str) -> bool {
        let before = self.tokens.len();
        self.tokens.retain(|t| t.token_id != token_id);
        let removed = self.tokens.len() < before;
        if removed {
            let _ = self.save();
        }
        removed
    }

    pub fn revoke_all(&mut self) -> usize {
        let count = self.tokens.len();
        self.tokens.clear();
        let _ = self.save();
        count
    }
}
