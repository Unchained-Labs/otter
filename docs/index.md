---
layout: home

hero:
  name: "otter"
  text: "Rust Orchestration Platform for Prompt-Driven Execution"
  tagline: "Queue-aware, event-streamed, workspace-isolated execution control plane."
  actions:
    - theme: brand
      text: Get Started
      link: /tutorials/getting-started
    - theme: alt
      text: API Reference
      link: /api
    - theme: alt
      text: Architecture
      link: /architecture

features:
  - title: Durable Job Lifecycle
    details: Persisted state transitions provide reliable retry, pause/resume, and cancellation semantics.
  - title: Queue and Priority Control
    details: Explicit queue ranking and update endpoints keep orchestration behavior transparent.
  - title: Real-Time Event Streaming
    details: SSE streams lifecycle and output chunks for live operator and UI feedback.
  - title: Workspace Trust Boundaries
    details: Isolated execution contexts reduce blast radius and preserve control-plane integrity.
  - title: Runtime Operations APIs
    details: Start, stop, restart, logs, and shell channels support advanced execution workflows.
  - title: Production Operations Model
    details: Runbooks and NUC deployment guidance support reliable long-running environments.
---

## Product Context

otter provides the orchestration layer in the stack:

- `seal` is the operator-facing UI.
- `otter` handles orchestration, queueing, runtime, and lifecycle state.
- `lavoix` is used for speech workflows where voice prompt ingestion is enabled.

## Explore the Documentation

- [Architecture](/architecture)
- [Prompt-to-Result Flow](/prompt-to-result-flow)
- [REST API](/api)
- [Runbook](/runbook)
