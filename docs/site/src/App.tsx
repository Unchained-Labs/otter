type DocSection = {
  id: string
  title: string
  summary: string
  bullets: string[]
}

const navSections: DocSection[] = [
  {
    id: 'overview',
    title: 'Overview',
    summary: 'Otter is the Rust orchestration plane for queueing, execution, and streaming job lifecycle.',
    bullets: [
      'axum HTTP API server + worker separation',
      'Redis queue transport with PostgreSQL source of truth',
      'SSE event stream for live frontend status',
    ],
  },
  {
    id: 'execution-flow',
    title: 'Execution Flow',
    summary: 'Prompt intake and worker execution are persisted with explicit state transitions.',
    bullets: [
      'POST /v1/prompts enqueues jobs',
      'Workers claim queued jobs atomically',
      'Events and output chunks are persisted and streamed',
    ],
  },
  {
    id: 'runtime',
    title: 'Runtime Isolation',
    summary: 'Workspace execution runs in trusted boundaries with per-workspace operational controls.',
    bullets: [
      'Workspace shell sessions preserve cwd state',
      'Runtime start/stop/restart endpoints are available',
      'Logs and preview URL updates are first-class API features',
    ],
  },
  {
    id: 'ops',
    title: 'Operations',
    summary: 'Deployment and runbook procedures are documented for local and NUC environments.',
    bullets: [
      'Compose-based startup and health checks',
      'Operations runbook for backups and recovery',
      'Trust model guidance for allowed roots',
    ],
  },
]

const references = [
  { label: 'API Reference', href: '../api.md' },
  { label: 'Architecture', href: '../architecture.md' },
  { label: 'Prompt-to-Result Flow', href: '../prompt-to-result-flow.md' },
  { label: 'Runbook', href: '../runbook.md' },
  { label: 'Workspace Trust Model', href: '../workspace-trust-model.md' },
  { label: 'Operations NUC', href: '../operations-nuc.md' },
]

function App() {
  return (
    <main className="min-h-screen bg-rust-bg text-rust-text">
      <div className="border-b border-rust-border bg-rust-panel/95">
        <div className="mx-auto flex max-w-7xl items-center justify-between px-4 py-4">
          <a href="#overview" className="text-lg font-semibold tracking-wide text-rust-accentSoft">
            otter docs
          </a>
          <nav className="flex flex-wrap gap-2">
            {navSections.map((section) => (
              <a
                key={section.id}
                href={`#${section.id}`}
                className="rounded-md border border-rust-border px-3 py-1.5 text-sm text-rust-muted transition hover:bg-rust-panelSoft hover:text-rust-text"
              >
                {section.title}
              </a>
            ))}
          </nav>
        </div>
      </div>

      <div className="mx-auto grid max-w-7xl grid-cols-1 gap-6 px-4 py-6 lg:grid-cols-[260px_minmax(0,1fr)]">
        <aside className="h-fit rounded-2xl border border-rust-border bg-rust-panel p-5 shadow-glow lg:sticky lg:top-6">
          <p className="text-xs font-semibold uppercase tracking-[0.2em] text-rust-accentSoft">Sections</p>
          <div className="mt-4 space-y-2">
            {navSections.map((section) => (
              <a
                key={section.id}
                href={`#${section.id}`}
                className="block rounded-lg border border-transparent px-3 py-2 text-sm text-rust-muted transition hover:border-rust-border hover:bg-rust-panelSoft hover:text-rust-text"
              >
                {section.title}
              </a>
            ))}
          </div>
          <a
            href="https://github.com/Unchained-Labs/otter"
            target="_blank"
            rel="noreferrer"
            className="mt-6 inline-flex rounded-lg bg-rust-accent px-3 py-2 text-sm font-semibold text-white transition hover:opacity-90"
          >
            GitHub Repository
          </a>
        </aside>

        <section className="space-y-4">
          {navSections.map((section) => (
            <article
              key={section.id}
              id={section.id}
              className="scroll-mt-24 rounded-2xl border border-rust-border bg-rust-panel p-6 shadow-glow"
            >
              <h2 className="text-2xl font-semibold">{section.title}</h2>
              <p className="mt-2 text-rust-muted">{section.summary}</p>
              <ul className="mt-4 grid gap-2">
                {section.bullets.map((bullet) => (
                  <li key={bullet} className="rounded-lg border border-rust-border bg-rust-panelSoft px-3 py-2 text-sm">
                    {bullet}
                  </li>
                ))}
              </ul>
            </article>
          ))}

          <article id="references" className="scroll-mt-24 rounded-2xl border border-rust-border bg-rust-panel p-6 shadow-glow">
            <h2 className="text-2xl font-semibold">Merged Existing Documentation</h2>
            <p className="mt-2 text-rust-muted">
              Existing markdown docs remain in <code>docs/</code> and are linked here directly.
            </p>
            <ul className="mt-4 grid gap-2">
              {references.map((ref) => (
                <li key={ref.href}>
                  <a
                    href={ref.href}
                    target="_blank"
                    rel="noreferrer"
                    className="block rounded-lg border border-rust-border bg-rust-panelSoft px-3 py-2 text-sm text-rust-accentSoft hover:text-rust-text"
                  >
                    {ref.label}
                  </a>
                </li>
              ))}
            </ul>
          </article>
        </section>
      </div>
    </main>
  )
}

export default App
