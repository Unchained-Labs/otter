const sections = [
  'Getting Started',
  'Architecture',
  'Queue & Workers',
  'Runtime Isolation',
  'API Endpoints',
  'Operations',
]

const highlights = [
  {
    title: 'Rust orchestration core',
    description: 'Strongly typed queueing, scheduling, and execution lifecycle built for scale.',
  },
  {
    title: 'Streaming-first runtime',
    description: 'SSE output chunks and job events support responsive frontend monitoring.',
  },
  {
    title: 'Workspace isolation model',
    description: 'Per-workspace execution boundaries keep automation safer and easier to reason about.',
  },
]

function App() {
  return (
    <main className="min-h-screen bg-docs-bg text-docs-text">
      <div className="mx-auto grid max-w-7xl grid-cols-1 gap-6 px-4 py-6 lg:grid-cols-[260px_minmax(0,1fr)]">
        <aside className="h-fit rounded-2xl border border-docs-border bg-docs-panel p-5 shadow-glow lg:sticky lg:top-6">
          <p className="text-xs font-semibold uppercase tracking-[0.2em] text-docs-accentSoft">otter docs</p>
          <h1 className="mt-2 text-2xl font-semibold">Live Documentation</h1>
          <nav className="mt-6 space-y-2">
            {sections.map((section) => (
              <a
                key={section}
                href="#"
                className="block rounded-lg border border-transparent px-3 py-2 text-sm text-docs-muted transition hover:border-docs-border hover:bg-docs-panelSoft hover:text-docs-text"
              >
                {section}
              </a>
            ))}
          </nav>
          <a
            href="https://github.com/Unchained-Labs/otter"
            target="_blank"
            rel="noreferrer"
            className="mt-6 inline-flex rounded-lg bg-docs-accent px-3 py-2 text-sm font-semibold text-white transition hover:opacity-90"
          >
            View Repository
          </a>
        </aside>

        <section className="rounded-2xl border border-docs-border bg-docs-panel p-8 shadow-glow">
          <p className="text-xs font-semibold uppercase tracking-[0.2em] text-docs-accentSoft">GitBook-style landing</p>
          <h2 className="mt-3 text-4xl font-semibold leading-tight">Orchestrate AI Jobs with Confidence</h2>
          <p className="mt-4 max-w-3xl text-base leading-7 text-docs-muted">
            otter coordinates prompts, workers, and runtime workspaces into a cohesive execution
            platform. This docs webapp is built with Vite, Node.js, React, TypeScript, and Tailwind CSS
            to provide a modern, extensible technical documentation experience.
          </p>

          <div className="mt-8 grid grid-cols-1 gap-4 md:grid-cols-3">
            {highlights.map((item) => (
              <article key={item.title} className="rounded-xl border border-docs-border bg-docs-panelSoft p-4">
                <h3 className="text-lg font-semibold">{item.title}</h3>
                <p className="mt-2 text-sm leading-6 text-docs-muted">{item.description}</p>
              </article>
            ))}
          </div>

          <div className="mt-10 rounded-xl border border-docs-border bg-docs-panelSoft p-5">
            <h3 className="text-xl font-semibold">Quick commands</h3>
            <pre className="mt-3 overflow-auto rounded-lg bg-docs-bg p-4 text-sm text-docs-accentSoft">
{`npm install
npm run dev
npm run build
npm run preview`}
            </pre>
          </div>
        </section>
      </div>
    </main>
  )
}

export default App
