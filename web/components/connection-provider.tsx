"use client";

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
} from "react";
import type {
  Command,
  DaemonStatus,
  Response,
} from "@/lib/types";
import { DEFAULT_HTTP_PORT } from "@/lib/types";

interface Credentials {
  host: string;
  pairing: string;
}

interface ConnectionState {
  credentials: Credentials | null;
  connected: boolean;
  loading: boolean;
  error: string | null;
  daemonStatus: DaemonStatus | null;
  connect: (creds: Credentials) => Promise<void>;
  disconnect: () => void;
  sendCommand: (command: Command) => Promise<Response>;
}

const ConnectionContext = createContext<ConnectionState | null>(null);

const STORAGE_KEY = "vex-credentials";

function buildUrl(host: string): string {
  const lastColon = host.lastIndexOf(":");
  let hostname: string;
  let port: number;

  if (lastColon === -1) {
    hostname = host;
    port = DEFAULT_HTTP_PORT;
  } else {
    const portStr = host.slice(lastColon + 1);
    const parsed = parseInt(portStr, 10);
    if (isNaN(parsed)) {
      hostname = host;
      port = DEFAULT_HTTP_PORT;
    } else {
      hostname = host.slice(0, lastColon);
      port = parsed;
    }
  }

  return `https://${hostname}:${port}/api/command`;
}

export function ConnectionProvider({
  children,
}: {
  children: React.ReactNode;
}) {
  const [credentials, setCredentials] = useState<Credentials | null>(null);
  const [connected, setConnected] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [daemonStatus, setDaemonStatus] = useState<DaemonStatus | null>(null);

  // Load saved credentials on mount
  useEffect(() => {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved) {
      try {
        const creds: Credentials = JSON.parse(saved);
        // Auto-reconnect with saved credentials
        connectWithCreds(creds);
      } catch {
        localStorage.removeItem(STORAGE_KEY);
      }
    }
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const sendCommand = useCallback(
    async (command: Command): Promise<Response> => {
      if (!credentials) {
        throw new Error("Not connected");
      }

      const url = buildUrl(credentials.host);
      const res = await fetch(url, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "Authorization": `Bearer ${credentials.pairing}`,
        },
        body: JSON.stringify({ command }),
      });

      if (res.status === 401) {
        throw new Error("Authentication failed");
      }

      const data: Response = await res.json();
      if (data.type === "Error") {
        const err = data.data;
        throw new Error(
          "code" in err && err.code === "Internal" ? err.message : err.code
        );
      }
      return data;
    },
    [credentials]
  );

  const connectWithCreds = async (creds: Credentials) => {
    setLoading(true);
    setError(null);

    try {
      const url = buildUrl(creds.host);
      const res = await fetch(url, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "Authorization": `Bearer ${creds.pairing}`,
        },
        body: JSON.stringify({ command: { type: "Status" } }),
      });

      if (res.status === 401) {
        throw new Error("Authentication failed");
      }

      const data: Response = await res.json();
      if (data.type === "Error") {
        const err = data.data;
        throw new Error(
          "code" in err && err.code === "Internal" ? err.message : err.code
        );
      }

      if (data.type === "DaemonStatus") {
        setDaemonStatus(data.data);
      }

      setCredentials(creds);
      setConnected(true);
      localStorage.setItem(STORAGE_KEY, JSON.stringify(creds));
    } catch (err) {
      const msg = err instanceof Error ? err.message : "Connection failed";
      setError(msg);
      setConnected(false);
      setDaemonStatus(null);
      localStorage.removeItem(STORAGE_KEY);
    } finally {
      setLoading(false);
    }
  };

  const connect = async (creds: Credentials) => {
    await connectWithCreds(creds);
  };

  const disconnect = () => {
    setCredentials(null);
    setConnected(false);
    setDaemonStatus(null);
    setError(null);
    localStorage.removeItem(STORAGE_KEY);
  };

  return (
    <ConnectionContext.Provider
      value={{
        credentials,
        connected,
        loading,
        error,
        daemonStatus,
        connect,
        disconnect,
        sendCommand,
      }}
    >
      {children}
    </ConnectionContext.Provider>
  );
}

export function useConnection() {
  const ctx = useContext(ConnectionContext);
  if (!ctx) {
    throw new Error("useConnection must be used within ConnectionProvider");
  }
  return ctx;
}
