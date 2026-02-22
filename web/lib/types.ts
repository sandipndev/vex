// TypeScript mirrors of crates/vex-cli/src/proto/mod.rs

export const DEFAULT_TCP_PORT = 7422;

// ── Wire types ──────────────────────────────────────────────────────────────

export type Command =
  | { type: "Status" }
  | { type: "Whoami" }
  | { type: "PairCreate"; data: { label?: string; expire_secs?: number } }
  | { type: "PairList" }
  | { type: "PairRevoke"; data: { id: string } }
  | { type: "PairRevokeAll" };

export type Response =
  | { type: "Pong" }
  | { type: "Ok" }
  | { type: "DaemonStatus"; data: DaemonStatus }
  | { type: "ClientInfo"; data: ClientInfo }
  | { type: "Pair"; data: PairPayload }
  | { type: "PairedClient"; data: PairedClient }
  | { type: "PairedClients"; data: PairedClient[] }
  | { type: "Revoked"; data: number }
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

export interface ClientInfo {
  token_id?: string;
  is_local: boolean;
}

export type VexProtoError =
  | { code: "Unauthorized" }
  | { code: "LocalOnly" }
  | { code: "NotFound" }
  | { code: "Internal"; message: string };

export interface AuthToken {
  token_id: string;
  token_secret: string;
}

// ── API request/response ────────────────────────────────────────────────────

export interface VexApiRequest {
  host: string;
  token_id: string;
  token_secret: string;
  command: Command;
}

export interface VexApiResponse {
  ok: boolean;
  response?: Response;
  error?: string;
}
