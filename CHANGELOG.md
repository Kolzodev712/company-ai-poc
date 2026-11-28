# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Documentation

- Add a verified fixture-demo walkthrough with run ID capture, output examples, and first-run troubleshooting.

## [0.1.0] - 2026-11-28

First public reference-implementation release. A local Rust crate, `company-ai`, with `publish = false`.

### Added

- Governance: a versioned bundle (mission, workflow, policy, skill, aliases) with schema, skill-hash, and fixture-containment checks. Optional `validate --hub` verifies recorded SHA-256 provenance against a sibling [cursor-hub](https://github.com/Kolzodev712/cursor-hub) checkout.
- Security broker: one deny-by-default decision per proposal, using a runtime-assigned identity, producing a single `AuthorizedRead` for one target when allowed.
- Tooling gateway: closed parsing of worker proposals (unknown fields and tools rejected), authorization, size limits, one transient retry counted against the provider budget, and recording of each attempt.
- Runtime: event admission with same-id, same-payload reuse and conflict rejection; a bounded worker loop; persisted budgets for model turns, tool proposals, provider attempts, duration, and sizes; completion checks that require same-run evidence.
- Workers: a scripted worker (default) and an opt-in Ollama worker.
- Adapters: fixture files by default and an opt-in AWS S3 `GetObject` adapter using a named profile, with SDK retries disabled.
- Store: transactional SQLite history of events, runs, steps, decisions, and evidence.
- Reports: JSON and Markdown reports separating observations, hypotheses, and missing information, citing evidence from the same run.
- CLI: `demo`, `run`, `validate`, `inspect`, `replay`, `export`, `live check`, and `live smoke`. Exit codes: `0` completed, `1` recorded failure, `2` setup, validation, or event conflict.
- Documentation: architecture notes and diagrams, adapter extension points, limitations, and ADRs.
- Community files: Apache-2.0 `LICENSE`, `NOTICE`, contributing guide, security policy, code of conduct, roadmap, and issue and pull request templates.
- Provenance pin refreshed to cursor-hub `14b375bbc77c0dfb15d03cb26f1231c3b5d5d0da` after that repository's public-adoption documentation release. Only the hub `README.md` content hash changed among recorded sources; methodological roles are unchanged.

### Limitations

See [docs/limitations.md](docs/limitations.md). In short:

- The default worker is scripted, and the default service is fixtures. A passing default run shows no model participation and no cloud call.
- Runtime identity labels are not cryptographic workload identity. Scoped application authorization is not cloud IAM.
- The SQLite record is not tamper-proof. Reports are structural and do not prove a diagnosis is true.
- Replay reads recorded history and does not re-execute the model or a provider.
- One local process. No distributed workers, no resume of interrupted runs, no mid-run policy revocation.
- Live Ollama and S3 paths are opt-in, are not exercised by default tests, and one real provider is integrated.
- Requires Rust 1.94.1 or newer, set by the locked dependency graph.

[Unreleased]: https://github.com/Kolzodev712/company-ai-poc/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Kolzodev712/company-ai-poc/releases/tag/v0.1.0
