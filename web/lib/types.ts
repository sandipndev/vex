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
  | { type: "ProjectRegister"; data: { name: string; repo: string; path: string } }
  | { type: "ProjectUnregister"; data: { name: string } }
  | { type: "ProjectList" }
  | { type: "WorkstreamCreate"; data: { project_name: string; name: string } }
  | { type: "WorkstreamList"; data: { project_name: string } }
  | { type: "WorkstreamDelete"; data: { project_name: string; name: string } }
  | { type: "ShellCreate"; data: { project_name: string; workstream_name: string } }
  | { type: "ShellList"; data: { project_name: string; workstream_name: string } }
  | { type: "ShellDelete"; data: { project_name: string; workstream_name: string; shell_id: string } };

export type Response =
  | { type: "Pong" }
  | { type: "Ok" }
  | { type: "DaemonStatus"; data: DaemonStatus }
  | { type: "ClientInfo"; data: ClientInfo }
  | { type: "Pair"; data: PairPayload }
  | { type: "PairedClient"; data: PairedClient }
  | { type: "PairedClients"; data: PairedClient[] }
  | { type: "Revoked"; data: number }
  | { type: "Project"; data: ProjectInfo }
  | { type: "Projects"; data: ProjectInfo[] }
  | { type: "Workstream"; data: WorkstreamInfo }
  | { type: "Workstreams"; data: WorkstreamInfo[] }
  | { type: "Shell"; data: ShellInfo }
  | { type: "Shells"; data: ShellInfo[] }
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

export interface ProjectInfo {
  name: string;
  repo: string;
  path: string;
}

export interface WorkstreamInfo {
  name: string;
  project_name: string;
  shell_count: number;
}

export interface ShellInfo {
  id: string;
  project_name: string;
  workstream_name: string;
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
