# Roadmap

This is a reference implementation, not a platform. This page describes how work is categorized. It gives no dates and no delivery commitments. Items move only when there is demand and a design that fits the limits in [docs/limitations.md](docs/limitations.md).

## Available

What exists now is in [CHANGELOG.md](CHANGELOG.md) under `0.1.0`: governance bundle validation, deny-by-default broker, tooling gateway, bounded runtime, SQLite history, replay, scripted and Ollama workers, fixture and S3 read adapters, and structural reports.

## Accepted contribution opportunities

Published issues (start here):

| Issue | Labels |
|-------|--------|
| [#1 — Fixture-demo onboarding polish](https://github.com/Kolzodev712/company-ai-poc/issues/1) | good first issue, documentation |
| [#2 — `inspect` / report reading examples](https://github.com/Kolzodev712/company-ai-poc/issues/2) | help wanted, documentation |
| [#3 — Untested broker deny reasons](https://github.com/Kolzodev712/company-ai-poc/issues/3) | help wanted, testing |

Additional drafts (budget exhaustion, exact size-limit fixture, additional adapter design) are ready to publish via `finish-remaining.sh` if activation was interrupted before they were created.

Open an issue to discuss before starting high-scrutiny work; see [CONTRIBUTING.md](CONTRIBUTING.md).

## Maintainer-led

Work in the high-scrutiny areas listed in [CONTRIBUTING.md](CONTRIBUTING.md) (governance validation, authorization, credentials, tool execution, evidence integrity, budgets, event admission, replay, live provider access) is led by the maintainer. External patches there are possible, but only after a design is agreed.

## Experimental design

Design-only. Nothing here is implemented, and none of it is a promise.

- Recovery of interrupted runs. Today an interrupted run is marked and not resumed; a new attempt needs a new event id.
- Additional read adapters beyond fixture and S3.
- Stronger workload identity than configured runtime labels.
- Tamper-evident evidence storage beyond the current recorded hashes.
- `cursor-hub` follow-ups listed in [docs/limitations.md](docs/limitations.md). These are proposals for another repository and have not been applied.

## Demand-driven

Further changes happen only if people actually use this reference and ask for them. Distributed workers, a scheduler, multi-tenant operation, and a general agent platform are not planned.
