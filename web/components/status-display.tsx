"use client";

import { useConnection } from "./connection-provider";

function formatUptime(secs: number): string {
  const days = Math.floor(secs / 86400);
  const hours = Math.floor((secs % 86400) / 3600);
  const minutes = Math.floor((secs % 3600) / 60);
  const s = secs % 60;

  const parts: string[] = [];
  if (days > 0) parts.push(`${days}d`);
  if (hours > 0) parts.push(`${hours}h`);
  if (minutes > 0) parts.push(`${minutes}m`);
  parts.push(`${s}s`);
  return parts.join(" ");
}

export function StatusDisplay() {
  const { daemonStatus, credentials, disconnect } = useConnection();

  if (!daemonStatus || !credentials) return null;

  return (
    <div className="w-full max-w-md space-y-6">
      <div className="border border-neutral-700 rounded p-6 space-y-4">
        <div className="flex items-center justify-between">
          <h2 className="text-lg font-semibold text-white">Connected</h2>
          <span className="inline-block w-2 h-2 rounded-full bg-green-400" />
        </div>

        <div className="space-y-3 text-sm">
          <div className="flex justify-between">
            <span className="text-neutral-400">Host</span>
            <span data-cy="status-host" className="text-white font-mono">
              {credentials.host}
            </span>
          </div>
          <div className="flex justify-between">
            <span className="text-neutral-400">Version</span>
            <span data-cy="status-version" className="text-white font-mono">
              {daemonStatus.version}
            </span>
          </div>
          <div className="flex justify-between">
            <span className="text-neutral-400">Uptime</span>
            <span data-cy="status-uptime" className="text-white font-mono">
              {formatUptime(daemonStatus.uptime_secs)}
            </span>
          </div>
          <div className="flex justify-between">
            <span className="text-neutral-400">Clients</span>
            <span data-cy="status-clients" className="text-white font-mono">
              {daemonStatus.connected_clients}
            </span>
          </div>
        </div>
      </div>

      <button
        data-cy="disconnect-button"
        onClick={disconnect}
        className="w-full py-2 px-4 border border-neutral-700 text-neutral-400 rounded hover:border-red-400 hover:text-red-400 transition-colors"
      >
        Disconnect
      </button>
    </div>
  );
}
