# Workspace Trust Model

## Why Isolation

Otter executes prompts in arbitrary project workspaces. To prevent trust leakage across projects and avoid mutating global user trust config, each workspace uses a dedicated `VIBE_HOME` directory.

## Isolation Strategy

- Workspace registration validates canonical path and optional allowlist roots.
- Each workspace gets its own isolated home:
  - `<OTTER_VIBE_HOME_BASE>/<workspace_id>/`
- Otter writes `trusted_folders.toml` in that isolated home with the workspace path.
- Vibe execution is always invoked with:
  - `--workdir <workspace_root>`
  - `VIBE_HOME=<isolated_home>`
- Prompt intake supports two modes:
  - explicit `workspace_id` from client
  - implicit default workspace via `OTTER_DEFAULT_WORKSPACE_PATH` when `workspace_id` is omitted

## Guardrails

- Reject non-existent root paths.
- Reject paths not under configured allowlist (`OTTER_ALLOWED_ROOTS`) when set.
- Never rely on global `~/.vibe` for workspace trust.

## Operational Notes

- Rotate/clean isolated homes only when corresponding workspace is retired.
- Back up isolated homes together with PostgreSQL if auditability is required.
- For single-repo setups, configure `OTTER_DEFAULT_WORKSPACE_PATH` and let clients omit `workspace_id`.
