# ADR 0001 — Company AI workflow POC

Date: 2026-11-25
Status: accepted. The Python language choice is superseded by ADR 0002. Module boundaries, trust assumptions, and non-goals still apply.

## Context

`company-ai-poc` is a consultancy reference kit: one local Python process with four logical modules (governance, security broker, tooling gateway, runtime). The example scenario is a diagnostic investigation, not a workflow every company should adopt.

cursor-hub was inspected as a read-only reference. Use a sibling checkout (for example `../cursor-hub` next to this repository) at the provenance commit recorded in `governance/bundle.json` (originally `c5328edae2ac54df38244d7a4ef9353de1553a84` at the time of this ADR; refresh the pin when hub sources change). Pass that path with `--hub` when validating provenance. The hub's system-orchestration kit is offline simulation. Its fixture approvals, receipts, and `VERIFIED` state are not live authentication or evidence.

## Decision

Ship a small runnable companion. Do not import `cursor_hub`, install its packs, or change the hub.

Reuse these ideas in companion code and record source hashes in the governance bundle:

- Closed JSON schemas and canonical SHA-256 fingerprints (`cursor_hub/orchestration/contracts.py`).
- Transactional local SQLite identity, append-before-effect recording, and read-only replay (`cursor_hub/orchestration/journal.py`).
- Separate mission, proposal, authorization, receipt, and evidence. Data inside an observation cannot change authority (`skills/system-orchestration/reference/observation-and-evidence.md`).
- A bounded read-only investigation report that separates observed facts, hypotheses, and missing information (fault-first skill and report contract).

Python implementation and testing rules from the hub are developer guidance for this repository. They are not loaded into the model prompt and they are not a permission engine.

The runtime worker receives only the mission objective, the event, the selected investigation skill, tool descriptions, and same-run evidence summaries.

## Modules

| Module | Responsibility | Trust |
| --- | --- | --- |
| Governance | Versioned bundle: mission, workflow, skill, operation schemas, allow-lists, resource aliases, limits, provenance | Operator-owned files. Models and providers cannot write them. |
| Security broker | Deny-by-default decision for one normalized request | Uses the runtime-assigned identity, workflow, and run. Authorizes the resolved target the adapter must use. |
| Tooling gateway | Validate, resolve, authorize, enforce limits, call the registered adapter, return a bounded result | The only path to `diagnostics.read`. A denial performs zero adapter calls. |
| Runtime | Admit an event, run a bounded worker loop, persist state, complete only when workflow checks pass | Assigns identity and run id. Treats worker output as untrusted proposals. |

## Defaults

- Python 3.11+ package, CLI entry point `company-ai`.
- Schema validation: JSON Schema draft 2020-12 via `jsonschema` (the same approach as the hub orchestration extra).
- State: one SQLite file per operator-selected path. Reports: local JSON and Markdown.
- Default run: scripted worker and fixture service. No network.
- Live Ollama or S3 runs only with an explicit `--live` flag and complete settings. Missing settings are a setup error. There is no fixture fallback.
- Ollama is called with the documented `POST /api/chat` HTTP API (`stream: false`, `think: false`, finite `options.num_predict`) using the standard library. The Ollama Python SDK is not used, so retries and thinking traces are not hidden.
- S3 reads use boto3/botocore `GetObject` on the alias's bucket and key, with `retries.max_attempts = 1` (the SDK total includes the first attempt) and explicit connect/read timeouts. boto3 is an optional `live` extra and is imported only when that adapter builds a client.
- First operation: `diagnostics.read` with a resource alias. No shell, HTTP tool, Python eval, or generic AWS action.

## Non-goals

Distributed execution, exactly-once delivery, mid-run revocation, cryptographic workload identity, a general policy language, MCP, mutating cloud actions, automatic remediation, and a dashboard. This process is not a sandbox for arbitrary code.

## Hub follow-ups

Recorded in `docs/limitations.md`. They are not implemented in cursor-hub.
