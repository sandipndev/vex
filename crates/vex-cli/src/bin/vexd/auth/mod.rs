use anyhow::Result;
use chrono::{DateTime, Utc};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use subtle::ConstantTimeEq;

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
            Ok(Self {
                path,
                tokens: vec![],
            })
        }
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(&self.tokens)?;
        std::fs::write(&self.path, data)?;
        // Restrict tokens file to owner-only (contains secret hashes)
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
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

        let expires_at = expire_secs.map(|s| Utc::now() + chrono::Duration::seconds(s as i64));

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
        // Compute the blake3 hash of the presented secret as raw bytes.
        let presented_hash: [u8; 32] = *blake3::hash(&secret_bytes).as_bytes();

        if let Some(token) = self.tokens.iter_mut().find(|t| t.token_id == token_id) {
            if let Some(expires_at) = token.expires_at
                && Utc::now() > expires_at
            {
                return false;
            }
            // Decode the stored hex hash and compare in constant time to
            // prevent timing-based secret oracle attacks.
            let stored_bytes = match hex_to_bytes(&token.token_secret_hash) {
                Ok(b) if b.len() == 32 => b,
                _ => return false,
            };
            if bool::from(presented_hash.ct_eq(&stored_bytes)) {
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

#[cfg(test)]
mod tests {
    use super::TokenStore;

    fn make_store() -> (tempfile::TempDir, TokenStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let store = TokenStore::load(path).unwrap();
        (dir, store)
    }

    #[test]
    fn token_id_and_secret_format() {
        let (_dir, mut store) = make_store();
        let (token, secret) = store.generate(None, None).unwrap();
        // token_id: "tok_" + 6 hex chars = 10 chars
        assert!(token.token_id.starts_with("tok_"));
        assert_eq!(token.token_id.len(), 10);
        // secret: 64 hex chars (32 bytes)
        assert_eq!(secret.len(), 64);
        assert!(secret.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn validate_correct_secret() {
        let (_dir, mut store) = make_store();
        let (token, secret) = store.generate(None, None).unwrap();
        assert!(store.validate(&token.token_id, &secret));
    }

    #[test]
    fn validate_wrong_secret() {
        let (_dir, mut store) = make_store();
        let (token, _) = store.generate(None, None).unwrap();
        let wrong = "a".repeat(64);
        assert!(!store.validate(&token.token_id, &wrong));
    }

    #[test]
    fn validate_unknown_token_id() {
        let (_dir, mut store) = make_store();
        store.generate(None, None).unwrap();
        assert!(!store.validate("tok_000000", &"b".repeat(64)));
    }

    #[test]
    fn revoke_removes_token() {
        let (_dir, mut store) = make_store();
        let (token, secret) = store.generate(None, None).unwrap();
        assert!(store.validate(&token.token_id, &secret));
        assert!(store.revoke(&token.token_id));
        assert!(!store.validate(&token.token_id, &secret));
        assert!(!store.revoke(&token.token_id)); // already gone
    }

    #[test]
    fn revoke_all_clears_everything() {
        let (_dir, mut store) = make_store();
        store.generate(None, None).unwrap();
        store.generate(None, None).unwrap();
        assert_eq!(store.list().len(), 2);
        assert_eq!(store.revoke_all(), 2);
        assert!(store.list().is_empty());
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let (token_id, secret) = {
            let mut store = TokenStore::load(path.clone()).unwrap();
            let (token, secret) = store.generate(Some("laptop".to_string()), None).unwrap();
            (token.token_id, secret)
        };
        // Reload from disk
        let mut store2 = TokenStore::load(path).unwrap();
        assert_eq!(store2.list().len(), 1);
        assert_eq!(store2.list()[0].label.as_deref(), Some("laptop"));
        assert!(store2.validate(&token_id, &secret));
    }

    #[test]
    fn multiple_tokens_validate_independently() {
        let (_dir, mut store) = make_store();
        let (tok1, sec1) = store.generate(None, None).unwrap();
        let (tok2, sec2) = store.generate(None, None).unwrap();
        assert!(store.validate(&tok1.token_id, &sec1));
        assert!(store.validate(&tok2.token_id, &sec2));
        // Cross-validation must fail
        assert!(!store.validate(&tok1.token_id, &sec2));
        assert!(!store.validate(&tok2.token_id, &sec1));
    }
}
