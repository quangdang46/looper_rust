# Changelog

All notable changes to looper_rust will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Web dashboard (React+Vite+TypeScript SPA) ported from Go original
- Dashboard API compatibility routes (flat `/api/loops`, `/api/status`)
- Takeover feature: single-PR focus mode (CLI + API + storage)
- Agent skills ported from Go (looper skill, pr-takeover skill)
- PR template and code review checklist
- Pre-commit hook and verify script
- CHANGELOG.md

### Changed
- CLI `takeover` command now functional (was hidden stub)

## [0.1.0] - 2026-08-17

### Added
- Initial release of looper_rust
- Daemon with REST API (30+ endpoints)
- CLI with all core commands
- 5 agent roles: Coordinator, Planner, Reviewer, Worker, Fixer
- 6 agent vendors: Claude, OpenAI, Gemini, Grok, DeepSeek, Custom
- SQLite storage with 5 migrations
- Webhook forwarding with routing
- Network mode (LooperNet)
- Dispatch access control (human-gated vs autonomous)
- Auto-upgrade from GitHub releases
- Cross-platform installers (Unix + Windows)
