use std::collections::HashSet;
use std::sync::mpsc;
use std::thread;

use chrono::{DateTime, Utc};

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
    pub last_accessed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

pub enum WorkerRequest {
    RefreshWorkstreams,
    CapturePane { session: String, window: String },
    LoadPrCache { repo_paths: Vec<String> },
    FetchPrStructured { repo_path: String, pr_number: u64 },
    ListBranches { repo_path: String },
    GitFetch { repo_paths: Vec<String> },
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
    PrStructuredFetched {
        pr_number: u64,
        data: Result<String, String>,
    },
    BranchesListed {
        branches: Vec<String>,
    },
    GitFetchCompleted,
}

/// Coarsened key for deduplication — ignores volatile params.
#[derive(Hash, Eq, PartialEq, Clone)]
enum RequestKey {
    RefreshWorkstreams,
    CapturePane,
    LoadPrCache,
    FetchPrStructured,
    ListBranches,
    GitFetch,
}

impl WorkerRequest {
    fn key(&self) -> Option<RequestKey> {
        match self {
            WorkerRequest::RefreshWorkstreams => Some(RequestKey::RefreshWorkstreams),
            WorkerRequest::CapturePane { .. } => Some(RequestKey::CapturePane),
            WorkerRequest::LoadPrCache { .. } => Some(RequestKey::LoadPrCache),
            WorkerRequest::FetchPrStructured { .. } => Some(RequestKey::FetchPrStructured),
            WorkerRequest::ListBranches { .. } => Some(RequestKey::ListBranches),
            WorkerRequest::GitFetch { .. } => Some(RequestKey::GitFetch),
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
            WorkerResponse::PrStructuredFetched { .. } => RequestKey::FetchPrStructured,
            WorkerResponse::BranchesListed { .. } => RequestKey::ListBranches,
            WorkerResponse::GitFetchCompleted => RequestKey::GitFetch,
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
                            last_accessed_at: ws.last_accessed_at,
                            created_at: ws.created_at,
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

            WorkerRequest::FetchPrStructured {
                repo_path,
                pr_number,
            } => {
                let data = match github::pr_view_structured(&repo_path, pr_number) {
                    Ok((view, checks)) => {
                        match serde_json::to_string(&serde_json::json!({
                            "title": view.title,
                            "number": view.number,
                            "body": view.body,
                            "url": view.url,
                            "state": view.state,
                            "comments": view.comments.iter().map(|c| serde_json::json!({
                                "author": c.author.login,
                                "body": c.body,
                                "created_at": c.created_at,
                            })).collect::<Vec<_>>(),
                            "reviews": view.reviews.iter().map(|r| serde_json::json!({
                                "author": r.author.login,
                                "body": r.body,
                                "state": r.state,
                                "created_at": r.created_at,
                            })).collect::<Vec<_>>(),
                            "checks_passed": checks.iter().filter(|c| c.conclusion == "SUCCESS" || c.conclusion == "success").count(),
                            "checks_total": checks.len(),
                        })) {
                            Ok(json) => Ok(json),
                            Err(e) => Err(format!("JSON serialization error: {e}")),
                        }
                    }
                    Err(e) => Err(format!("Error fetching PR: {e}")),
                };
                let _ = tx.send(WorkerResponse::PrStructuredFetched { pr_number, data });
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
        }
    }
}
