# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

## [1.1.0] - 2026-08-28

### Added
- Persistent runtime app registry (workspace + job runtime instances) plus shutdown-all endpoint.
- Guardrails to ensure runtime `working_directory` and job `project_path` point to self-contained compose folders; workspace root marker file is created during workspace creation.
- Token accounting and delivery metrics, with an eval suite covering them.

### Fixed
- Prompt-cache traffic is counted in tokens and cost. It was being dropped, so
  runs that leaned on the cache under-reported both.
- Jobs dependencies endpoint added to the server.
- Docker builds now use Rust 1.86 to match transitive dependency MSRV.
- **The Docker build works again.** The Mistral Vibe installer exits non-zero
  when its install directory is not already on `PATH` — it installs
  successfully, prints an error telling you to add the directory, and returns
  1, which failed the `RUN` and so the whole image. `PATH` is now set before
  the install, and `vibe --version` runs in the same layer so a genuinely
  missing binary fails at build rather than at first job.

## [1.0.0] - 2026-04-01

### Added
- Job orchestration service with queueing, scheduling, and status tracking.
- Task dependencies and hold/resume controls for workflow coordination.
- Runtime metadata and endpoints to start/stop task-specific run commands.
- Voice prompt ingestion via STT gateway integration.

### Changed
- Hardened voice transcription path with request timeouts to prevent indefinite hangs.
