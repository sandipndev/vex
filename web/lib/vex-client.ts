import * as tls from "node:tls";
import type { AuthToken, Command, Response } from "./types";

const DEFAULT_PORT = 7422;
const TIMEOUT_MS = 10_000;

function parseHostPort(host: string): { hostname: string; port: number } {
  const lastColon = host.lastIndexOf(":");
  if (lastColon === -1) {
    return { hostname: host, port: DEFAULT_PORT };
  }
  const portStr = host.slice(lastColon + 1);
  const port = parseInt(portStr, 10);
  if (isNaN(port)) {
    return { hostname: host, port: DEFAULT_PORT };
  }
  return { hostname: host.slice(0, lastColon), port };
}

function encodeFrame(data: unknown): Buffer {
  const body = Buffer.from(JSON.stringify(data), "utf-8");
  const header = Buffer.alloc(4);
  header.writeUInt32BE(body.length, 0);
  return Buffer.concat([header, body]);
}

function writeFrame(
  socket: tls.TLSSocket,
  data: unknown
): Promise<void> {
  return new Promise((resolve, reject) => {
    socket.write(encodeFrame(data), (err) => {
      if (err) reject(err);
      else resolve();
    });
  });
}

class FrameReader {
  private buf = Buffer.alloc(0);
  private waiting: { resolve: () => void; reject: (e: Error) => void } | null =
    null;

  constructor(private socket: tls.TLSSocket) {
    socket.on("data", (chunk: Buffer) => {
      this.buf = Buffer.concat([this.buf, chunk]);
      if (this.waiting) {
        const w = this.waiting;
        this.waiting = null;
        w.resolve();
      }
    });
    socket.on("error", (err) => {
      if (this.waiting) {
        const w = this.waiting;
        this.waiting = null;
        w.reject(err);
      }
    });
    socket.on("close", () => {
      if (this.waiting) {
        const w = this.waiting;
        this.waiting = null;
        w.reject(new Error("Connection closed while reading"));
      }
    });
  }

  private waitForData(): Promise<void> {
    return new Promise((resolve, reject) => {
      this.waiting = { resolve, reject };
    });
  }

  async readFrame<T>(): Promise<T> {
    // Read 4-byte length header
    while (this.buf.length < 4) {
      await this.waitForData();
    }
    const len = this.buf.readUInt32BE(0);

    // Read full body
    while (this.buf.length < 4 + len) {
      await this.waitForData();
    }

    const body = this.buf.subarray(4, 4 + len);
    this.buf = this.buf.subarray(4 + len);
    return JSON.parse(body.toString("utf-8")) as T;
  }
}

function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error(`Operation timed out after ${ms}ms`)),
      ms
    );
    promise.then(
      (v) => {
        clearTimeout(timer);
        resolve(v);
      },
      (e) => {
        clearTimeout(timer);
        reject(e);
      }
    );
  });
}

export async function executeVexCommand(
  host: string,
  tokenId: string,
  tokenSecret: string,
  command: Command
): Promise<Response> {
  const { hostname, port } = parseHostPort(host);

  const socket = await withTimeout(
    new Promise<tls.TLSSocket>((resolve, reject) => {
      const sock = tls.connect(
        {
          host: hostname,
          port,
          rejectUnauthorized: false, // Self-signed certs (TOFU model)
        },
        () => resolve(sock)
      );
      sock.on("error", reject);
    }),
    TIMEOUT_MS
  );

  try {
    const reader = new FrameReader(socket);

    // Send auth token
    const auth: AuthToken = {
      token_id: tokenId,
      token_secret: tokenSecret,
    };
    await writeFrame(socket, auth);

    // Read auth response
    const authResponse = await withTimeout(
      reader.readFrame<Response>(),
      TIMEOUT_MS
    );
    if (authResponse.type === "Error") {
      const err = authResponse.data;
      const msg =
        "code" in err && err.code === "Unauthorized"
          ? "Authentication failed"
          : `Server error: ${JSON.stringify(err)}`;
      throw new Error(msg);
    }
    if (authResponse.type !== "Pong") {
      throw new Error(`Unexpected auth response: ${authResponse.type}`);
    }

    // Send command
    await writeFrame(socket, command);

    // Read command response
    return await withTimeout(reader.readFrame<Response>(), TIMEOUT_MS);
  } finally {
    socket.destroy();
  }
}
