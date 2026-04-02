# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added
- Persistent runtime app registry (workspace + job runtime instances) plus shutdown-all endpoint.
- Guardrails to ensure runtime `working_directory` and job `project_path` point to self-contained compose folders; workspace root marker file is created during workspace creation.

### Fixed
- Docker builds now use Rust 1.86 to match transitive dependency MSRV.

## [1.0.0] - 2026-04-01

### Added
- Job orchestration service with queueing, scheduling, and status tracking.
- Task dependencies and hold/resume controls for workflow coordination.
- Runtime metadata and endpoints to start/stop task-specific run commands.
- Voice prompt ingestion via STT gateway integration.

### Changed
- Hardened voice transcription path with request timeouts to prevent indefinite hangs.
