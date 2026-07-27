# otter-complexity

Deterministic complexity and size scoring for natural-language build tasks, plus
an MCP server exposing it as a tool.

Given a prompt, it answers the three questions a scheduler needs:

| Field | Range | Meaning |
|---|---|---|
| `complexity` | 1–10 | How hard is this to reason about? |
| `size` | 1–10 | How much work is there? |
| `intensity` | 0–100 | The single number a queue sorts on. Lower runs sooner. |

It also returns a `band` (`trivial`/`small`/`moderate`/`large`/`epic`), an
`estimated_minutes`, a `confidence`, and the `signals` behind the score.

```rust
let assessment = otter_complexity::assess("add Stripe checkout and a webhook handler");
assert_eq!(assessment.band.as_str(), "small");
```

## Why heuristics rather than a model

Scoring runs on the enqueue path, before the work itself. A model call there
would add latency and cost to every submission, fail when the provider is down,
and return different answers for the same prompt on different days — so a job's
queue position would wobble for reasons no user could see.

These heuristics are instant, free, offline and deterministic. Every score
arrives with the signals that produced it, so a surprising queue position is
explainable rather than mysterious.

Where a model does help is refinement *after* the fact, and there is a path for
it: `TaskAssessment::with_refinement` recomputes the derived fields and records
`source: "refined"`, and Otter exposes it as `POST /v1/jobs/{id}/assessment`.

## Accuracy, honestly

This is a ranking aid, not an oracle. It is tuned so that *relative* order is
usually right — a typo fix sorts ahead of a payments integration — which is all
the scheduler needs. Treat `estimated_minutes` as an order of magnitude.

Known limits:

- It reads English keywords. A prompt in another language scores near the
  defaults, with confidence reflecting that.
- It cannot know your codebase. "Add a button" is one line in one project and a
  week in another.
- Adversarial or unusual phrasing can mislead it. That is what
  `confidence` and the refinement endpoint are for.

Run the calibration harness after touching `lexicon.rs`:

```bash
cargo run -p otter-complexity --example calibrate
```

The unit tests assert relative ordering, which catches inversions but not drift
— a change pushing everything into one band still passes them. The harness
prints the whole spread so bands and estimates can be eyeballed.

## How the score is built

1. **Lexicon floors.** Complexity and size terms in `lexicon.rs` each argue for a
   floor, not an addend, so ten mentions of "button" cannot out-vote one
   "consensus".
2. **Word-boundary matching.** Terms match whole words with common inflections.
   This matters more than it sounds: plain substring matching put "port" (as in
   porting a codebase) inside both "export" and "report", scoring a CSV export
   like a platform migration.
3. **Construction verbs.** "Build"/"create"/"implement" set a size floor, because
   making something new is reliably more work than adjusting something existing
   and the noun lexicon misses it entirely.
4. **Breadth.** Distinct system surfaces touched (frontend, backend, database,
   infra, auth, testing, docs, data) is the strongest size signal available.
5. **Deliverables and length.** Explicit list structure and prompt length nudge
   size.
6. **Ambiguity.** Vague phrasing raises complexity slightly and lowers confidence
   a lot — under-specified scope hides work.
7. **Minor-verb cap.** Applied last, overriding everything: "fix the typo in the
   kubernetes runbook" is a typo fix, not a cluster job.

## MCP server

`otter-complexity-mcp` speaks MCP over stdio (JSON-RPC 2.0, line-delimited).

```bash
cargo build -p otter-complexity --release
```

Register it with any MCP client:

```json
{
  "mcpServers": {
    "otter-complexity": { "command": "/path/to/otter-complexity-mcp" }
  }
}
```

### Tools

**`evaluate_complexity`** — score one prompt.

```json
{ "prompt": "add OAuth login", "dependency_count": 0 }
```

**`rank_tasks`** — score several and return them in execution order.

```json
{ "prompts": ["build a distributed scheduler", "fix typo in readme"] }
```

Both return human-readable `content` and machine-readable `structuredContent`.

Try it without a client:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"evaluate_complexity","arguments":{"prompt":"add OAuth login"}}}' \
  | otter-complexity-mcp
```

## Standalone by design

This crate has no Otter dependencies — it knows nothing about jobs, queues or
databases. It lives in the Otter workspace for convenience, and can be lifted
into its own repository with `git subtree split` whenever that is useful.
