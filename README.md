# Vex

> Parallel, asynchronous, multi-agent work stream orchestration — with first-class tmux and GitHub integration.

---

## Why Vex

Modern AI agent workflows are bottlenecked by synchronous attention. You have to watch an agent finish before moving to the next task, which wastes your cognitive capacity. Vex exists to let you drive multiple parallel agent work streams asynchronously, from any device — with tmux and GitHub as first-class primitives, not afterthoughts.

The core insight: a work stream is not a single agent. It is a coordinated group of agents working in parallel toward a shared goal, managed by a coordinator agent that delegates, aggregates, and resolves conflicts. Vex is the system that orchestrates all of it.

---

## Mental Model

```
Vex
├── Work Stream A  (coordinator + N sub-agents)
│   ├── Agent 1 — writing feature code
│   ├── Agent 2 — writing tests
│   └── Agent 3 — updating docs
│
├── Work Stream B  (coordinator + N sub-agents)
│   ├── Agent 1 — investigating bug
│   └── Agent 2 — searching codebase for related issues
│
└── Work Stream C ...
```

You spawn work streams, give them goals, and context-switch freely between them. Vex handles the rest — running agents in parallel tmux sessions, pushing to GitHub, notifying you on mobile when attention is needed, and exposing everything through a web interface you can access from anywhere.

---

## Feature Requirements

### 1. Multi-Work-Stream Management

- Spawn and manage multiple isolated agent work streams simultaneously
- Each work stream runs in its own named tmux session, managed automatically by Vex — no manual tmux setup required
- Work stream state is fully persistent and survives terminal disconnects, SSH drops, and machine reboots
- Attach to or detach from any work stream at will without interrupting agent execution
- Each work stream has a unique name, a goal description, a status, and a full audit log
- Work streams can be paused, resumed, cancelled, or cloned

### 2. Intra-Work-Stream Agent Parallelism

- Each work stream supports multiple sub-agents running concurrently toward a shared goal
- Sub-agents are role-specialized — e.g. one writes code, one writes tests, one updates documentation, simultaneously
- Each work stream has a **coordinator agent** that delegates tasks to sub-agents, tracks their progress, aggregates outputs, and resolves conflicts
- Sub-agent outputs are merged intelligently — git branches reconciled, test results collated, conflicting file edits surfaced for review
- Sub-agents within a work stream can have a dependency graph — agent C only starts when agents A and B have completed their outputs
- Resource limits per work stream — cap the number of concurrent sub-agents to control cost and system load

### 3. Orchestration Layer

- A central Vex daemon manages all active work streams and their sub-agents
- CLI and API interface to the daemon for spawning, inspecting, and controlling work streams
- Work stream pipelines — define sequential or parallel task graphs across multiple work streams
- Inter-work-stream communication — the output of one work stream can be passed as input to another
- Templated work streams for common task types: feature branch, bug fix, PR review, codebase exploration, documentation update, release preparation
- Vex can be driven programmatically via a config file (YAML or TOML) that defines work streams, their goals, their sub-agent roles, and their dependencies

### 4. tmux Integration (First-Class)

- Vex owns and manages the full tmux session lifecycle — sessions, windows, and panes map directly to work streams and sub-agents
- Named tmux sessions correspond 1:1 with work streams, browsable and attachable by name
- Each sub-agent runs in its own tmux pane or window within the work stream's session
- Automatic tmux layout management for multi-agent monitoring — split pane views, status panes, coordinator summary pane
- Vex CLI attaches intelligently to the right session based on work stream name or current context
- A dedicated tmux window provides a live overview of all active work streams and their statuses

### 5. GitHub Integration (First-Class)

- Create branches, commits, and pull requests directly from Vex without leaving the workflow
- Automated PR creation with AI-generated title, description, linked issues, and suggested reviewers
- PR review workflow — Vex pulls a PR, sub-agents analyze it in parallel (logic review, style, tests, security), and surface a structured review
- Respond to GitHub webhooks — CI failure triggers an agent to investigate and propose a fix; a new issue triggers a triage agent
- Status checks and PR merge readiness visible natively within Vex
- Support for branch protection rules — Vex is aware of required checks and will not attempt to merge until they pass
- Commit message generation from agent activity logs
- Full GitLab feature parity for all of the above

### 6. Async Notifications and Mobile Access

- Push notifications to mobile when a work stream completes, stalls, produces output, or needs a decision
- Notifications are configurable at the work stream level and the sub-agent level — e.g. notify only when the coordinator needs human input, not on every sub-agent update
- Mobile-optimized web UI to view work stream status, read agent output, and send the next prompt or instruction
- Approve or reject agent-proposed actions from mobile — e.g. confirm before opening a PR, merging a branch, or running a destructive command
- Configurable notification channels: push notification, SMS, email, Slack webhook, Discord webhook

### 7. Web Interface and Server

- A persistent server exposes all Vex functionality through a web interface accessible from any device
- Real-time streaming of agent output in a terminal-style browser view
- Start, pause, resume, or cancel any work stream or sub-agent remotely via the web UI
- Prompt input to continue a work stream or provide a decision from the browser
- Work stream history — browse past work streams, their full output logs, agent actions taken, and outcomes
- Side-by-side diff view when multiple sub-agents have modified the same files, with conflict resolution UI
- A dashboard view showing all active work streams, their statuses, sub-agent counts, and GitHub PR links

### 8. Routine Task Automation

- Built-in skills or macros for common recurring tasks: open PR, merge PR, rebase branch, run tests, summarize diff, respond to review comments, close stale issues, tag a release
- Skills are triggerable from the CLI, the web UI, or a mobile notification action
- Skills can be chained as post-completion steps in a work stream — e.g. automatically open a PR when the coding work stream completes
- Custom skills can be defined by the user as scripts or prompt templates

### 9. Observability and History

- Full audit log of every agent action, prompt sent, output received, and tool call made, per sub-agent and per work stream
- Replay or resume any work stream from a historical checkpoint
- Cost tracking per work stream: token usage, estimated cost, wall-clock time elapsed per sub-agent and in total
- Searchable logs across all work streams and agents
- Exportable work stream reports — a summary of what was done, what files were changed, what decisions were made, and what the outcome was

---

## Integrations

| Integration | Support Level |
|---|---|
| tmux | First-class — fully managed |
| GitHub | First-class — branches, PRs, webhooks, reviews |
| GitLab | Full parity with GitHub integration |
| Slack | Notifications and prompt input via slash commands |
| Discord | Notifications webhook |
| SMS / Email | Async notifications |
| Mobile browser | Responsive web UI |

---

## Non-Goals (v1)

- Vex does not manage cloud VM provisioning — agents run on the machine where Vex is installed
- Vex does not support non-git version control systems in v1
- Vex does not provide its own code editor — it operates on files in existing repositories

---

## Project Name

**Vex** — parallel agent orchestration for people who move faster than their agents.
