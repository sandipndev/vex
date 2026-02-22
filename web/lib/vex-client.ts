import * as net from "node:net";
import * as tls from "node:tls";
import type { AuthToken, Command, Response } from "./types";

const DEFAULT_PORT = 7422;

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

async function readExact(
  socket: tls.TLSSocket,
  n: number
): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    let received = 0;

    const onData = (chunk: Buffer) => {
      chunks.push(chunk);
      received += chunk.length;
      if (received >= n) {
        socket.removeListener("data", onData);
        socket.removeListener("error", onError);
        socket.removeListener("close", onClose);
        const full = Buffer.concat(chunks);
        resolve(full.subarray(0, n));
        // If we read more than n, push back the excess
        if (full.length > n) {
          socket.unshift(full.subarray(n));
        }
      }
    };

    const onError = (err: Error) => {
      socket.removeListener("data", onData);
      socket.removeListener("close", onClose);
      reject(err);
    };

    const onClose = () => {
      socket.removeListener("data", onData);
      socket.removeListener("error", onError);
      reject(new Error("Connection closed while reading"));
    };

    socket.on("data", onData);
    socket.on("error", onError);
    socket.on("close", onClose);
  });
}

async function recvFrame<T>(socket: tls.TLSSocket): Promise<T> {
  const header = await readExact(socket, 4);
  const len = header.readUInt32BE(0);
  const body = await readExact(socket, len);
  return JSON.parse(body.toString("utf-8")) as T;
}

export async function executeVexCommand(
  host: string,
  tokenId: string,
  tokenSecret: string,
  command: Command
): Promise<Response> {
  const { hostname, port } = parseHostPort(host);

  const socket = await new Promise<tls.TLSSocket>((resolve, reject) => {
    const sock = tls.connect(
      {
        host: hostname,
        port,
        rejectUnauthorized: false, // Self-signed certs (TOFU model)
      },
      () => resolve(sock)
    );
    sock.on("error", reject);
  });

  try {
    // Pause stream for manual reading
    socket.pause();

    // Send auth token
    const auth: AuthToken = {
      token_id: tokenId,
      token_secret: tokenSecret,
    };
    socket.write(encodeFrame(auth));

    // Read auth response
    const authResponse = await recvFrame<Response>(socket);
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
    socket.write(encodeFrame(command));

    // Read command response
    const response = await recvFrame<Response>(socket);
    return response;
  } finally {
    socket.destroy();
  }
}
