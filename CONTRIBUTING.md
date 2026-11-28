# Contributing

Thank you for your interest in `company-ai-poc`, a Rust reference implementation of governed AI workflows (explicit authorization, bounded execution, auditable evidence). It is a reference and credibility demo, not a production platform. Contributions that keep it small, accurate, and testable are welcome.

## Ground rules

- Prefer small, focused pull requests with tests for functional changes.
- Do not commit secrets, AWS credentials or profiles, personal `.env` files, `live.toml`, SQLite state files, or private copies of the governance bundle.
- Do not claim production readiness, cryptographic identity, tamper-proof audit, or true diagnosis. Keep the wording in [docs/limitations.md](docs/limitations.md) accurate when behavior changes.
- AI-assisted patches are welcome only if you reviewed them and can explain the change. Large generated diffs without explanation may be rejected.
- Do not change the provenance hashes in `governance/bundle.json` as a side effect. Updating the recorded [cursor-hub](https://github.com/Kolzodev712/cursor-hub) commit or hashes is a deliberate, separately reviewed change.
- Do not add GitHub Actions or other hosted CI in a feature PR. Verification is local (below).

## High-scrutiny areas

Open an issue or discussion and agree on the design **before** changing any of these. Expect closer review and a request for tests that show denial and failure paths:

- Governance bundle loading, validation, and provenance checks (`src/governance.rs`)
- Authorization decisions and `AuthorizedRead` (`src/broker.rs`)
- Provider credentials and profile handling (`src/adapters/s3.rs`, `src/config.rs`)
- Tool validation and execution (`src/gateway.rs`, `src/adapters/`)
- Evidence integrity, storage, and report citation (`src/store.rs`, `src/report.rs`)
- Budgets and timeouts (`src/runtime.rs`)
- Event admission, deduplication, and conflict handling (`src/runtime.rs`, `src/store.rs`)
- Replay and inspect behavior (`src/store.rs`, `src/cli.rs`)
- Live provider access, including Ollama and S3 (`src/workers/ollama.rs`, `src/adapters/s3.rs`, `--live`)

A change in these areas must not widen what a worker can do, must keep deny-by-default, and must not fall back silently from a live adapter to fixtures.

## Getting started

1. Install Rust **1.94.1 or newer** (see `rust-version` in `Cargo.toml`).
2. Fork the repository and branch from `main`.
3. Build and run the demo:

   ```bash
   cargo build --locked
   cargo run --locked -- demo --output var/demo
   ```

4. Propose non-trivial or architectural work in an issue first. See [ROADMAP.md](ROADMAP.md).

`cursor-hub` is optional. Only `validate --hub` needs a sibling checkout of [Kolzodev712/cursor-hub](https://github.com/Kolzodev712/cursor-hub) at the commit recorded in `governance/bundle.json`.

## Local verification

Run before opening a pull request:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

No live provider tests are required for default pull requests. Do not run live Ollama or AWS calls as part of a normal PR. The live smoke test is `#[ignore]`d on purpose; an ignored test is not a passed live call. If your change touches live adapters, say what you verified and what you could not.

## Architecture pointers

- [README.md](README.md): quickstart, three behaviors, limitations
- [docs/architecture.md](docs/architecture.md) and [docs/architecture-chart.md](docs/architecture-chart.md): trust boundaries and diagrams
- [docs/adapters.md](docs/adapters.md): extension points
- [docs/limitations.md](docs/limitations.md): what a passing run does and does not show
- [docs/adr/](docs/adr/): decisions, including the move to Rust

## Pull requests

- Describe the problem, the change, and the verification commands you ran (the PR template asks for this).
- Note any effect on authorization, evidence, budgets, or limitations text.
- Keep unrelated formatting and refactors out of the diff.

## License of contributions

This project is licensed under the [Apache License, Version 2.0](LICENSE). Unless you state otherwise, any contribution you intentionally submit for inclusion is licensed under Apache-2.0, without additional terms or conditions, as described in Section 5 of the license. Copyright © 2026 T.L.P. Arc Entertainment Ltd.; maintainer Nikola Kolev.

## Conduct and security

Participation is governed by the [Code of Conduct](CODE_OF_CONDUCT.md). Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md), not in public issues.
