use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io::Write, path::PathBuf};

/// All details for a single named connection to a vexd daemon.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ConnectionEntry {
    /// "unix" or "tcp"
    pub transport: String,
    /// Path to Unix socket (transport = "unix")
    pub unix_socket: Option<String>,
    /// host:port (transport = "tcp")
    pub tcp_host: Option<String>,
    /// Token ID for TCP auth
    pub token_id: Option<String>,
    /// Plaintext token secret for TCP auth
    pub token_secret: Option<String>,
    /// Blake3 fingerprint of the server TLS cert (TOFU)
    pub tls_fingerprint: Option<String>,
}

/// Top-level config stored in `~/.vex/config.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// Name of the connection to use when none is specified
    pub default_connection: Option<String>,
    /// Named connections keyed by user-chosen name
    #[serde(default)]
    pub connections: HashMap<String, ConnectionEntry>,
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("cannot determine home directory")?;
        Ok(home.join(".vex").join("config.toml"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&content).context("parsing config.toml")
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self).context("serializing config")?;
        // Open with restricted permissions so the file (which contains the
        // plaintext token secret) is never world-readable.
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(&path)
            .and_then(|mut f| f.write_all(content.as_bytes()))
            .with_context(|| format!("writing {}", path.display()))
    }

    /// Return the effective connection to use given an optional explicit name.
    /// Errors if no connection is found.
    pub fn resolve<'a>(&'a self, name: Option<&str>) -> Result<(String, &'a ConnectionEntry)> {
        let key: String = match name {
            Some(n) => n.to_string(),
            None => self.default_connection.clone().ok_or_else(|| {
                anyhow::anyhow!("No default connection set. Run 'vex connect' or 'vex use <name>'.")
            })?,
        };
        let entry = self
            .connections
            .get(&key)
            .ok_or_else(|| anyhow::anyhow!("Unknown connection '{key}'"))?;
        Ok((key, entry))
    }

    /// Add or replace a named connection and optionally set it as the default.
    pub fn upsert(&mut self, name: String, entry: ConnectionEntry, set_default: bool) {
        if set_default || self.default_connection.is_none() {
            self.default_connection = Some(name.clone());
        }
        self.connections.insert(name, entry);
    }

    /// Remove a named connection. If it was the default, clear the default.
    pub fn remove(&mut self, name: &str) -> bool {
        let removed = self.connections.remove(name).is_some();
        if removed && self.default_connection.as_deref() == Some(name) {
            // Pick another one as default, or clear
            self.default_connection = self.connections.keys().next().cloned();
        }
        removed
    }

    /// Remove all connections.
    pub fn clear_all(&mut self) {
        self.connections.clear();
        self.default_connection = None;
    }
}
