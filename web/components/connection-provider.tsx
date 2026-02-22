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
  VexApiResponse,
} from "@/lib/types";

interface Credentials {
  host: string;
  token_id: string;
  token_secret: string;
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

      const res = await fetch("/api/vex", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          host: credentials.host,
          token_id: credentials.token_id,
          token_secret: credentials.token_secret,
          command,
        }),
      });

      const data: VexApiResponse = await res.json();
      if (!data.ok || !data.response) {
        throw new Error(data.error || "Request failed");
      }
      return data.response;
    },
    [credentials]
  );

  const connectWithCreds = async (creds: Credentials) => {
    setLoading(true);
    setError(null);

    try {
      const res = await fetch("/api/vex", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          host: creds.host,
          token_id: creds.token_id,
          token_secret: creds.token_secret,
          command: { type: "Status" },
        }),
      });

      const data: VexApiResponse = await res.json();
      if (!data.ok || !data.response) {
        throw new Error(data.error || "Connection failed");
      }

      if (data.response.type === "DaemonStatus") {
        setDaemonStatus(data.response.data);
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
