use std::path::PathBuf;

/// Resolves the VEX_HOME directory.
///
/// Order of precedence:
/// 1. `VEX_HOME` environment variable (with `~` expansion)
/// 2. `~/.vex`
pub fn vex_home() -> PathBuf {
    if let Ok(raw) = std::env::var("VEX_HOME") {
        if let Some(rest) = raw.strip_prefix("~/")
            && let Some(home) = dirs::home_dir()
        {
            return home.join(rest);
        }
        return PathBuf::from(raw);
    }
    dirs::home_dir()
        .map(|h| h.join(".vex"))
        .unwrap_or_else(|| PathBuf::from("/tmp/.vex"))
}
