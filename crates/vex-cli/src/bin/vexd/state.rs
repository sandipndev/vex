use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Instant,
};
use tokio::sync::Mutex;

use vex_cli::user_config::UserConfig;

use crate::auth::TokenStore;
use crate::repo_store::RepoStore;

pub struct AppState {
    pub start_time: Instant,
    pub client_counter: AtomicU32,
    pub token_store: Arc<Mutex<TokenStore>>,
    pub repo_store: Arc<Mutex<RepoStore>>,
    /// `$VEX_HOME` — used to derive all file paths
    pub vex_home: PathBuf,
    pub user_config: UserConfig,
    /// AbortHandles for per-agent monitoring tasks, keyed by agent_id
    pub monitor_handles: Arc<Mutex<HashMap<String, tokio::task::AbortHandle>>>,
}

impl AppState {
    pub fn new(
        vex_home: PathBuf,
        token_store: TokenStore,
        repo_store: RepoStore,
        user_config: UserConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            start_time: Instant::now(),
            client_counter: AtomicU32::new(0),
            token_store: Arc::new(Mutex::new(token_store)),
            repo_store: Arc::new(Mutex::new(repo_store)),
            vex_home,
            user_config,
            monitor_handles: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn uptime_secs(&self) -> u64 {
        self.start_time.elapsed().as_secs()
    }

    pub fn connected_clients(&self) -> u32 {
        self.client_counter.load(Ordering::Relaxed)
    }

    pub fn increment_clients(&self) {
        self.client_counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn decrement_clients(&self) {
        self.client_counter.fetch_sub(1, Ordering::Relaxed);
    }

    /// Path to the Unix socket for this daemon instance.
    pub fn socket_path(&self) -> PathBuf {
        self.daemon_dir().join("vexd.sock")
    }

    /// `$VEX_HOME/daemon/`
    pub fn daemon_dir(&self) -> PathBuf {
        self.vex_home.join("daemon")
    }

    /// `$VEX_HOME/worktrees/`
    pub fn worktrees_dir(&self) -> PathBuf {
        self.vex_home.join("worktrees")
    }
}
