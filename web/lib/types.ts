// TypeScript mirrors of crates/vex-cli/src/proto/mod.rs

export const DEFAULT_HTTP_PORT = 7423;

// ── Wire types ──────────────────────────────────────────────────────────────

export type Command =
  | { type: "Status" }
  | { type: "Whoami" }
  | { type: "PairCreate"; data: { label?: string; expire_secs?: number } }
  | { type: "PairList" }
  | { type: "PairRevoke"; data: { id: string } }
  | { type: "PairRevokeAll" }
  | { type: "RepoRegister"; data: { name: string; path: string } }
  | { type: "RepoUnregister"; data: { name: string } }
  | { type: "RepoList" };

export type Response =
  | { type: "Pong" }
  | { type: "Ok" }
  | { type: "DaemonStatus"; data: DaemonStatus }
  | { type: "ClientInfo"; data: ClientInfo }
  | { type: "Pair"; data: PairPayload }
  | { type: "PairedClient"; data: PairedClient }
  | { type: "PairedClients"; data: PairedClient[] }
  | { type: "Revoked"; data: number }
  | { type: "Repo"; data: RepoInfo }
  | { type: "Repos"; data: RepoInfo[] }
  | { type: "Error"; data: VexProtoError };

export interface DaemonStatus {
  uptime_secs: number;
  connected_clients: number;
  version: string;
}

export interface PairPayload {
  token_id: string;
  token_secret: string;
  host?: string;
}

export interface PairedClient {
  token_id: string;
  label?: string;
  created_at: string;
  expires_at?: string;
  last_seen?: string;
}

export interface RepoInfo {
  name: string;
  path: string;
}

export interface ClientInfo {
  token_id?: string;
  is_local: boolean;
}

export type VexProtoError =
  | { code: "Unauthorized" }
  | { code: "LocalOnly" }
  | { code: "NotFound" }
  | { code: "Internal"; message: string };
