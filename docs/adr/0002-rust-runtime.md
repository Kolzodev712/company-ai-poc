# ADR 0002 — Rust is the only runtime

Date: 2026-11-25
Status: accepted
Supersedes: the language choice in ADR 0001

## Decision

The companion application is a Rust binary, `company-ai`. Governance files, fixtures, the investigation skill, and the cursor-hub provenance record stay language-neutral JSON and Markdown. The Python implementation that proved the same behavior is only in Git history (`2c4af0c`). It is not a second runtime.

Rust is one crate, not a set of services. Modules are governance, the security broker, the tooling gateway, the runtime, adapters, workers, and the CLI. Tokio is used only to drive the AWS SDK and the Ollama HTTP client from that synchronous core. Rusqlite is used instead of SQLx because the state machine is synchronous and the queries are local. Serde with `deny_unknown_fields` enforces the closed contracts. Schemars derives the portable JSON Schema from those types. Reqwest calls Ollama's documented `POST /api/chat`. The AWS SDK for Rust performs `GetObject` with SDK retries disabled (`max_attempts = 1`, including the first attempt).

Compile-time types separate an untrusted worker value, a validated proposal, and an `AuthorizedRead` that only the broker can construct. Those types do not replace runtime authorization, sandboxing, or policy tests.

## Hub guidance now in force

Developer guidance is the cursor-hub Rust implementation and testing rules, recorded in the bundle provenance. The Python pack entries were removed from that record because this runtime does not consume them. cursor-hub itself is unchanged.
