# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added
- Persistent runtime app registry (workspace + job runtime instances) plus shutdown-all endpoint.

## [1.0.0] - 2026-04-01

### Added
- Job orchestration service with queueing, scheduling, and status tracking.
- Task dependencies and hold/resume controls for workflow coordination.
- Runtime metadata and endpoints to start/stop task-specific run commands.
- Voice prompt ingestion via STT gateway integration.

### Changed
- Hardened voice transcription path with request timeouts to prevent indefinite hangs.
