"use client";

import Link from "next/link";
import { ConnectionProvider, useConnection } from "@/components/connection-provider";
import { ConnectForm } from "@/components/connect-form";
import { StatusDisplay } from "@/components/status-display";

function AppContent() {
  const { connected, loading } = useConnection();

  return (
    <div className="min-h-screen flex flex-col">
      <header className="border-b border-neutral-800 px-6 py-4">
        <div className="max-w-5xl mx-auto flex items-center justify-between">
          <Link
            href="/"
            className="text-lg font-bold tracking-tight hover:text-neutral-300 transition-colors"
          >
            vex
          </Link>
          <span className="text-sm text-neutral-500">
            {connected ? "Connected" : "Disconnected"}
          </span>
        </div>
      </header>

      <main className="flex-1 flex items-center justify-center px-6 py-16">
        {!connected && !loading && (
          <div className="text-center space-y-8">
            <div className="space-y-2">
              <h1 className="text-2xl font-bold">Connect to vexd</h1>
              <p className="text-sm text-neutral-400">
                Enter your daemon host and pairing token to get started.
              </p>
            </div>
            <ConnectForm />
          </div>
        )}

        {loading && (
          <p className="text-neutral-400">Connecting...</p>
        )}

        {connected && <StatusDisplay />}
      </main>
    </div>
  );
}

export default function AppPage() {
  return (
    <ConnectionProvider>
      <AppContent />
    </ConnectionProvider>
  );
}
