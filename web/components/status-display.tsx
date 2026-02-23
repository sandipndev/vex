"use client";

import { useCallback, useEffect, useState } from "react";
import { useConnection } from "./connection-provider";
import type { RepoInfo, WorkstreamInfo } from "@/lib/types";

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
  const { daemonStatus, credentials, disconnect, sendCommand } = useConnection();
  const [repos, setRepos] = useState<RepoInfo[]>([]);
  const [workstreams, setWorkstreams] = useState<Record<string, WorkstreamInfo[]>>({});
  const [newWsName, setNewWsName] = useState<Record<string, string>>({});

  const fetchWorkstreams = useCallback(
    async (repoList: RepoInfo[]) => {
      const ws: Record<string, WorkstreamInfo[]> = {};
      for (const repo of repoList) {
        try {
          const res = await sendCommand({
            type: "WorkstreamList",
            data: { repo_name: repo.name },
          });
          if (res.type === "Workstreams") {
            ws[repo.name] = res.data;
          }
        } catch {
          // ignore
        }
      }
      setWorkstreams(ws);
    },
    [sendCommand],
  );

  const fetchRepos = useCallback(async () => {
    try {
      const res = await sendCommand({ type: "RepoList" });
      if (res.type === "Repos") {
        setRepos(res.data);
        await fetchWorkstreams(res.data);
      }
    } catch {
      // ignore — repos are supplemental info
    }
  }, [sendCommand, fetchWorkstreams]);

  const createWorkstream = useCallback(
    async (repoName: string) => {
      const name = newWsName[repoName]?.trim();
      if (!name) return;
      try {
        await sendCommand({
          type: "WorkstreamCreate",
          data: { repo_name: repoName, name },
        });
        setNewWsName((prev) => ({ ...prev, [repoName]: "" }));
        await fetchRepos();
      } catch {
        // ignore
      }
    },
    [sendCommand, newWsName, fetchRepos],
  );

  const deleteWorkstream = useCallback(
    async (repoName: string, wsName: string) => {
      try {
        await sendCommand({
          type: "WorkstreamDelete",
          data: { repo_name: repoName, name: wsName },
        });
        await fetchRepos();
      } catch {
        // ignore
      }
    },
    [sendCommand, fetchRepos],
  );

  useEffect(() => {
    if (daemonStatus && credentials) {
      fetchRepos();
    }
  }, [daemonStatus, credentials, fetchRepos]);

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
              vexd v{daemonStatus.version}
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

      <div data-cy="repos-section" className="border border-neutral-700 rounded p-6 space-y-4">
        <h2 className="text-lg font-semibold text-white">Repositories</h2>
        {repos.length === 0 ? (
          <p data-cy="repos-empty" className="text-sm text-neutral-400">
            No repositories registered
          </p>
        ) : (
          <div className="space-y-4 text-sm">
            {repos.map((r) => (
              <div key={r.name} data-cy="repo-item" className="space-y-2">
                <div className="flex justify-between">
                  <span className="text-white font-mono">{r.name}</span>
                  <span className="text-neutral-400 font-mono truncate ml-4">{r.path}</span>
                </div>
                <div className="ml-4 space-y-1">
                  {(workstreams[r.name] ?? []).map((ws) => (
                    <div
                      key={ws.name}
                      data-cy="workstream-item"
                      className="flex items-center justify-between"
                    >
                      <span className="text-neutral-300 font-mono text-xs">
                        {ws.name}
                      </span>
                      <button
                        data-cy="workstream-delete"
                        onClick={() => deleteWorkstream(r.name, ws.name)}
                        className="text-xs text-neutral-500 hover:text-red-400 transition-colors"
                      >
                        delete
                      </button>
                    </div>
                  ))}
                  <form
                    data-cy="workstream-create-form"
                    className="flex gap-2 mt-1"
                    onSubmit={(e) => {
                      e.preventDefault();
                      createWorkstream(r.name);
                    }}
                  >
                    <input
                      data-cy="workstream-name-input"
                      type="text"
                      placeholder="workstream name"
                      value={newWsName[r.name] ?? ""}
                      onChange={(e) =>
                        setNewWsName((prev) => ({
                          ...prev,
                          [r.name]: e.target.value,
                        }))
                      }
                      className="flex-1 bg-transparent border border-neutral-700 rounded px-2 py-1 text-xs text-white placeholder-neutral-500 focus:outline-none focus:border-neutral-500"
                    />
                    <button
                      data-cy="workstream-create-button"
                      type="submit"
                      className="text-xs text-neutral-400 hover:text-white border border-neutral-700 rounded px-2 py-1 transition-colors"
                    >
                      Create
                    </button>
                  </form>
                </div>
              </div>
            ))}
          </div>
        )}
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
