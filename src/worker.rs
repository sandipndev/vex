use std::collections::HashSet;
use std::sync::mpsc;
use std::thread;

use crate::github;
use crate::repo;
use crate::tmux;

/// Data sent from worker → TUI for each workstream.
pub struct WorkstreamData {
    pub repo_name: String,
    pub branch: String,
    pub session: String,
    pub active: bool,
    pub repo_path: String,
}

pub enum WorkerRequest {
    RefreshWorkstreams,
    CapturePane { session: String, window: String },
    LoadPrCache { repo_paths: Vec<String> },
    FetchPrDetails { repo_path: String, pr_number: u64 },
    ListBranches { repo_path: String },
    GitFetch { repo_paths: Vec<String> },
    ListPrs { repo_path: String },
    Shutdown,
}

pub enum WorkerResponse {
    WorkstreamsRefreshed {
        items: Vec<WorkstreamData>,
    },
    PaneCaptured {
        session: String,
        window: String,
        content: String,
    },
    PrCacheLoaded {
        entries: Vec<(String, u64)>,
    },
    PrDetailsFetched {
        pr_number: u64,
        content: String,
    },
    BranchesListed {
        branches: Vec<String>,
    },
    GitFetchCompleted,
    PrsListed {
        repo_path: String,
        prs: Vec<github::PrListEntry>,
    },
}

/// Coarsened key for deduplication — ignores volatile params.
#[derive(Hash, Eq, PartialEq, Clone)]
enum RequestKey {
    RefreshWorkstreams,
    CapturePane,
    LoadPrCache,
    FetchPrDetails,
    ListBranches,
    GitFetch,
    ListPrs,
}

impl WorkerRequest {
    fn key(&self) -> Option<RequestKey> {
        match self {
            WorkerRequest::RefreshWorkstreams => Some(RequestKey::RefreshWorkstreams),
            WorkerRequest::CapturePane { .. } => Some(RequestKey::CapturePane),
            WorkerRequest::LoadPrCache { .. } => Some(RequestKey::LoadPrCache),
            WorkerRequest::FetchPrDetails { .. } => Some(RequestKey::FetchPrDetails),
            WorkerRequest::ListBranches { .. } => Some(RequestKey::ListBranches),
            WorkerRequest::GitFetch { .. } => Some(RequestKey::GitFetch),
            WorkerRequest::ListPrs { .. } => Some(RequestKey::ListPrs),
            WorkerRequest::Shutdown => None,
        }
    }
}

impl WorkerResponse {
    fn key(&self) -> RequestKey {
        match self {
            WorkerResponse::WorkstreamsRefreshed { .. } => RequestKey::RefreshWorkstreams,
            WorkerResponse::PaneCaptured { .. } => RequestKey::CapturePane,
            WorkerResponse::PrCacheLoaded { .. } => RequestKey::LoadPrCache,
            WorkerResponse::PrDetailsFetched { .. } => RequestKey::FetchPrDetails,
            WorkerResponse::BranchesListed { .. } => RequestKey::ListBranches,
            WorkerResponse::GitFetchCompleted => RequestKey::GitFetch,
            WorkerResponse::PrsListed { .. } => RequestKey::ListPrs,
        }
    }
}

pub struct Worker {
    req_tx: mpsc::Sender<WorkerRequest>,
    resp_rx: mpsc::Receiver<WorkerResponse>,
    handle: Option<thread::JoinHandle<()>>,
    in_flight: HashSet<RequestKey>,
}

impl Worker {
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = mpsc::channel::<WorkerRequest>();
        let (resp_tx, resp_rx) = mpsc::channel::<WorkerResponse>();

        let handle = thread::spawn(move || worker_loop(req_rx, resp_tx));

        Worker {
            req_tx,
            resp_rx,
            handle: Some(handle),
            in_flight: HashSet::new(),
        }
    }

    /// Send a request to the worker. Returns false if the request was
    /// suppressed by deduplication (same category already in-flight).
    pub fn send(&mut self, req: WorkerRequest) -> bool {
        if let Some(key) = req.key()
            && !self.in_flight.insert(key)
        {
            return false; // already in-flight
        }
        // If the worker thread has exited, the send will fail — ignore it.
        let _ = self.req_tx.send(req);
        true
    }

    /// Drain all pending responses from the worker.
    pub fn try_recv_all(&mut self) -> Vec<WorkerResponse> {
        let mut responses = Vec::new();
        while let Ok(resp) = self.resp_rx.try_recv() {
            self.in_flight.remove(&resp.key());
            responses.push(resp);
        }
        responses
    }

    /// Send Shutdown and join the worker thread.
    pub fn shutdown(self) {
        let _ = self.req_tx.send(WorkerRequest::Shutdown);
        if let Some(handle) = self.handle {
            let _ = handle.join();
        }
    }
}

fn worker_loop(rx: mpsc::Receiver<WorkerRequest>, tx: mpsc::Sender<WorkerResponse>) {
    while let Ok(req) = rx.recv() {
        match req {
            WorkerRequest::Shutdown => break,

            WorkerRequest::RefreshWorkstreams => {
                let repos = repo::list_repos().unwrap_or_default();
                let active_sessions = tmux::list_sessions().unwrap_or_default();

                let mut items = Vec::new();
                for repo_meta in &repos {
                    for ws in &repo_meta.workstreams {
                        let session = tmux::session_name(&repo_meta.name, &ws.branch);
                        let active = active_sessions.contains(&session);
                        items.push(WorkstreamData {
                            repo_name: repo_meta.name.clone(),
                            branch: ws.branch.clone(),
                            session,
                            active,
                            repo_path: repo_meta.path.clone(),
                        });
                    }
                }
                let _ = tx.send(WorkerResponse::WorkstreamsRefreshed { items });
            }

            WorkerRequest::CapturePane { session, window } => {
                let content = tmux::capture_pane_text(&session, &window);
                let _ = tx.send(WorkerResponse::PaneCaptured {
                    session,
                    window,
                    content,
                });
            }

            WorkerRequest::LoadPrCache { repo_paths } => {
                let mut entries = Vec::new();
                for path in &repo_paths {
                    if let Ok(prs) = github::list_prs(path) {
                        for (branch, num) in prs {
                            entries.push((format!("{path}/{branch}"), num));
                        }
                    }
                }
                let _ = tx.send(WorkerResponse::PrCacheLoaded { entries });
            }

            WorkerRequest::FetchPrDetails {
                repo_path,
                pr_number,
            } => {
                let content = match github::pr_view_full(&repo_path, pr_number) {
                    Ok(c) => c,
                    Err(e) => format!("Error fetching PR: {e}"),
                };
                let _ = tx.send(WorkerResponse::PrDetailsFetched { pr_number, content });
            }

            WorkerRequest::ListBranches { repo_path } => {
                let branches = crate::git::list_branches(&repo_path).unwrap_or_default();
                let _ = tx.send(WorkerResponse::BranchesListed { branches });
            }

            WorkerRequest::GitFetch { repo_paths } => {
                for path in &repo_paths {
                    let _ = crate::git::fetch(path);
                }
                let _ = tx.send(WorkerResponse::GitFetchCompleted);
            }

            WorkerRequest::ListPrs { repo_path } => {
                let prs = github::list_prs_detailed(&repo_path).unwrap_or_default();
                let _ = tx.send(WorkerResponse::PrsListed { repo_path, prs });
            }
        }
    }
}
