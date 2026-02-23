"use client";

import { useCallback, useEffect, useState } from "react";
import { useConnection } from "./connection-provider";
import type { ProjectInfo, WorkstreamInfo } from "@/lib/types";

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
  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [workstreams, setWorkstreams] = useState<Record<string, WorkstreamInfo[]>>({});
  const [newWsName, setNewWsName] = useState<Record<string, string>>({});

  const fetchWorkstreams = useCallback(
    async (projectList: ProjectInfo[]) => {
      const ws: Record<string, WorkstreamInfo[]> = {};
      for (const project of projectList) {
        try {
          const res = await sendCommand({
            type: "WorkstreamList",
            data: { project_name: project.name },
          });
          if (res.type === "Workstreams") {
            ws[project.name] = res.data;
          }
        } catch {
          // ignore
        }
      }
      setWorkstreams(ws);
    },
    [sendCommand],
  );

  const fetchProjects = useCallback(async () => {
    try {
      const res = await sendCommand({ type: "ProjectList" });
      if (res.type === "Projects") {
        setProjects(res.data);
        await fetchWorkstreams(res.data);
      }
    } catch {
      // ignore — projects are supplemental info
    }
  }, [sendCommand, fetchWorkstreams]);

  const createWorkstream = useCallback(
    async (projectName: string) => {
      const name = newWsName[projectName]?.trim();
      if (!name) return;
      try {
        await sendCommand({
          type: "WorkstreamCreate",
          data: { project_name: projectName, name },
        });
        setNewWsName((prev) => ({ ...prev, [projectName]: "" }));
        await fetchProjects();
      } catch {
        // ignore
      }
    },
    [sendCommand, newWsName, fetchProjects],
  );

  const deleteWorkstream = useCallback(
    async (projectName: string, wsName: string) => {
      try {
        await sendCommand({
          type: "WorkstreamDelete",
          data: { project_name: projectName, name: wsName },
        });
        await fetchProjects();
      } catch {
        // ignore
      }
    },
    [sendCommand, fetchProjects],
  );

  useEffect(() => {
    if (daemonStatus && credentials) {
      fetchProjects();
    }
  }, [daemonStatus, credentials, fetchProjects]);

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

      <div data-cy="projects-section" className="border border-neutral-700 rounded p-6 space-y-4">
        <h2 className="text-lg font-semibold text-white">Projects</h2>
        {projects.length === 0 ? (
          <p data-cy="projects-empty" className="text-sm text-neutral-400">
            No projects registered
          </p>
        ) : (
          <div className="space-y-4 text-sm">
            {projects.map((p) => (
              <div key={p.name} data-cy="project-item" className="space-y-2">
                <div className="flex justify-between">
                  <span className="text-white font-mono">{p.name}</span>
                  <span className="text-neutral-400 font-mono truncate ml-4">{p.path}</span>
                </div>
                <div className="ml-4 space-y-1">
                  {(workstreams[p.name] ?? []).map((ws) => (
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
                        onClick={() => deleteWorkstream(p.name, ws.name)}
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
                      createWorkstream(p.name);
                    }}
                  >
                    <input
                      data-cy="workstream-name-input"
                      type="text"
                      placeholder="workstream name"
                      value={newWsName[p.name] ?? ""}
                      onChange={(e) =>
                        setNewWsName((prev) => ({
                          ...prev,
                          [p.name]: e.target.value,
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
