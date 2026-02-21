use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Instant,
};
use tokio::sync::Mutex;

use crate::auth::TokenStore;

pub struct AppState {
    pub start_time: Instant,
    pub client_counter: AtomicU32,
    pub token_store: Arc<Mutex<TokenStore>>,
    pub vexd_dir: PathBuf,
}

impl AppState {
    pub fn new(vexd_dir: PathBuf, token_store: TokenStore) -> Arc<Self> {
        Arc::new(Self {
            start_time: Instant::now(),
            client_counter: AtomicU32::new(0),
            token_store: Arc::new(Mutex::new(token_store)),
            vexd_dir,
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
        self.vexd_dir.join("vexd.sock")
    }
}
