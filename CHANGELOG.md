# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added — Milestone 1

- `stp-client` library: STP wire types, WebSocket + stdio transports, registration/execute
  session state machine, asset transfer (HTTP + filesystem), parameter coercion and JSON
  Schema validation, with the asset-ID grammar and auth-scoping rules enforced.
- `stp` binary with commands: `tools`, `schema`, `run`, `info`, `raw`, `watch`,
  `config` (`list`/`add`/`remove`/`set-default`/`get`/`init`/`path`/`edit`), `completions`,
  `agent-guide`, `version`.
- Named-flag parameters (`--guidance 1.0`) with a positional `prompt` (`-` = stdin), the
  `--set`/`--params` escapes, schema-driven coercion, `--dry-run` preflight, and media
  auto-upload.
- YAML config with ssh-style server aliases and token resolution
  (`--token`/`STP_TOKEN`/`token_env`/`token_command`).
- Typed exit-code taxonomy (0/1/2/3/4/5/6/130).
- `stp-mock-provider` stdio test fixture; unit, snapshot, and end-to-end test suites.

### Notes

- Config writes (`config add/remove/set-default`) round-trip the typed model with a managed
  header comment; in-place comment preservation via a rowan YAML editor is deferred until that
  ecosystem matures. `config init` writes a fully-commented self-documenting template.
- Milestone 2 (planned): `upload` (LoRA file-install flow), WebSocket end-to-end test
  coverage, and per-target release builds.
