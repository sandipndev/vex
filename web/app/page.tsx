import Link from "next/link";

const features = [
  {
    title: "Multi-stream",
    description:
      "Run dozens of parallel agent work streams. Context-switch freely without losing state.",
  },
  {
    title: "tmux-native",
    description:
      "Every agent gets its own tmux session. Attach, detach, monitor — zero setup.",
  },
  {
    title: "Remote access",
    description:
      "Drive your agents from any device. TLS-encrypted, token-authenticated, always available.",
  },
  {
    title: "GitHub-integrated",
    description:
      "Branches, PRs, reviews, CI — all orchestrated automatically by your agent swarm.",
  },
];

export default function LandingPage() {
  return (
    <div className="min-h-screen flex flex-col">
      {/* Nav */}
      <header className="border-b border-neutral-800 px-6 py-4">
        <div className="max-w-5xl mx-auto flex items-center justify-between">
          <span className="text-lg font-bold tracking-tight">vex</span>
          <Link
            href="/app"
            className="text-sm text-neutral-400 hover:text-white transition-colors"
          >
            Open App
          </Link>
        </div>
      </header>

      {/* Hero */}
      <main className="flex-1 flex flex-col items-center justify-center px-6">
        <div className="max-w-2xl text-center space-y-8 py-24">
          <h1 className="text-5xl sm:text-7xl font-bold tracking-tighter">
            Vex
          </h1>
          <p className="text-lg sm:text-xl text-neutral-400 leading-relaxed">
            Parallel agent orchestration for people who move faster than their
            agents.
          </p>
          <Link
            href="/app"
            data-cy="open-app-link"
            className="inline-block px-8 py-3 bg-white text-black font-semibold rounded hover:bg-neutral-200 transition-colors"
          >
            Open App &rarr;
          </Link>
        </div>

        {/* Feature grid */}
        <div className="max-w-4xl w-full grid grid-cols-1 sm:grid-cols-2 gap-4 pb-24">
          {features.map((f) => (
            <div
              key={f.title}
              className="border border-neutral-800 rounded p-6 hover:border-neutral-600 transition-colors"
            >
              <h3 className="font-semibold mb-2">{f.title}</h3>
              <p className="text-sm text-neutral-400 leading-relaxed">
                {f.description}
              </p>
            </div>
          ))}
        </div>
      </main>

      {/* Footer */}
      <footer className="border-t border-neutral-800 px-6 py-6">
        <div className="max-w-5xl mx-auto flex items-center justify-between text-sm text-neutral-500">
          <span>Vex</span>
          <a
            href="https://github.com/sandipndev/vex"
            target="_blank"
            rel="noopener noreferrer"
            className="hover:text-white transition-colors"
          >
            GitHub
          </a>
        </div>
      </footer>
    </div>
  );
}
