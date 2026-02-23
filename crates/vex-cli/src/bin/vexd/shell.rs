use std::collections::HashMap;
use std::sync::Arc;

use crate::tmux;

/// A single shell entry backed by a tmux window.
#[derive(Debug, Clone)]
pub struct ShellEntry {
    pub id: String,
    pub tmux_window_index: u32,
}

/// In-memory store for ephemeral shells keyed by (project, workstream).
pub struct ShellStore {
    shells: HashMap<(String, String), Vec<ShellEntry>>,
    counters: HashMap<(String, String), u32>,
}

impl ShellStore {
    pub fn new() -> Self {
        Self {
            shells: HashMap::new(),
            counters: HashMap::new(),
        }
    }

    /// Add a shell entry and return its assigned ID.
    pub fn add(&mut self, project: &str, workstream: &str, tmux_window_index: u32) -> String {
        let key = (project.to_string(), workstream.to_string());
        let counter = self.counters.entry(key.clone()).or_insert(0);
        *counter += 1;
        let id = format!("shell_{counter}");
        let entry = ShellEntry {
            id: id.clone(),
            tmux_window_index,
        };
        self.shells.entry(key).or_default().push(entry);
        id
    }

    /// List all shells for a (project, workstream) pair.
    pub fn list(&self, project: &str, workstream: &str) -> Vec<ShellEntry> {
        let key = (project.to_string(), workstream.to_string());
        self.shells.get(&key).cloned().unwrap_or_default()
    }

    /// Remove a specific shell by ID. Returns the removed entry if found.
    pub fn remove(
        &mut self,
        project: &str,
        workstream: &str,
        shell_id: &str,
    ) -> Option<ShellEntry> {
        let key = (project.to_string(), workstream.to_string());
        let entries = self.shells.get_mut(&key)?;
        let pos = entries.iter().position(|e| e.id == shell_id)?;
        let removed = entries.remove(pos);
        if entries.is_empty() {
            self.shells.remove(&key);
        }
        Some(removed)
    }

    /// Remove all shells for a (project, workstream) pair.
    pub fn remove_all(&mut self, project: &str, workstream: &str) {
        let key = (project.to_string(), workstream.to_string());
        self.shells.remove(&key);
    }

    /// Reconcile in-memory state against live tmux window indices.
    /// Removes any shells whose tmux_window_index is not in `live_indices`.
    pub fn reconcile(&mut self, project: &str, workstream: &str, live_indices: &[u32]) {
        let key = (project.to_string(), workstream.to_string());
        if let Some(entries) = self.shells.get_mut(&key) {
            entries.retain(|e| live_indices.contains(&e.tmux_window_index));
            if entries.is_empty() {
                self.shells.remove(&key);
            }
        }
    }

    /// Check whether a (project, workstream) has any shells.
    pub fn has_shells(&self, project: &str, workstream: &str) -> bool {
        let key = (project.to_string(), workstream.to_string());
        self.shells
            .get(&key)
            .is_some_and(|entries| !entries.is_empty())
    }

    /// Return all (project, workstream) keys that have at least one shell.
    pub fn active_workstreams(&self) -> Vec<(String, String)> {
        self.shells
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, _)| k.clone())
            .collect()
    }
}

/// Background task that polls tmux every 3 seconds to reconcile shell state.
pub async fn shell_monitor(state: Arc<crate::state::AppState>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        let workstreams = {
            let store = state.shell_store.lock().await;
            store.active_workstreams()
        };

        for (project, workstream) in workstreams {
            let session = tmux::session_name(&project, &workstream);

            if !tmux::has_session(&session) {
                let mut store = state.shell_store.lock().await;
                store.remove_all(&project, &workstream);
                continue;
            }

            match tmux::list_windows(&session) {
                Ok(live_indices) => {
                    let mut store = state.shell_store.lock().await;
                    store.reconcile(&project, &workstream, &live_indices);
                }
                Err(e) => {
                    tracing::warn!("shell_monitor: failed to list windows for {session}: {e}");
                }
            }
        }
    }
}
