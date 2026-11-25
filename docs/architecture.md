# Architecture and trust boundaries

One local process hosts four modules. The example workflow investigates a synthetic diagnostic snapshot. It is not a general agent platform.

```text
event JSON
    -> runtime assigns identity, workflow, run id
    -> worker sees mission, skill, event, tool descriptions, same-run evidence
    -> worker proposes diagnostics.read or submit_report
    -> gateway validates, broker decides, adapter reads only the authorized target
    -> runtime accepts a report only when same-run evidence and the required read exist
    -> SQLite history and local JSON/Markdown reports
```

## Who is trusted

| Actor | Trusted for | Not trusted for |
| --- | --- | --- |
| Operator files (bundle, config, fixture directory) | Mission, skill text, allow-lists, alias map, limits, endpoints, AWS profile name | Being rewritten by a model or a provider payload |
| Runtime | Assigning identity, workflow, and run id; enforcing budgets | Treating a model's "done" as completion |
| Security broker | One deny-by-default decision for one normalized request | Granting cloud permissions, minting credentials, or approving a later request |
| Tooling gateway | Calling the registered adapter only after a permit, with the authorized target | Accepting bucket, key, endpoint, or role values from the model |
| Service adapter | Reading that target, or returning a typed failure | Widening the target or choosing another credential |
| Worker and provider text | Proposals and untrusted data | Identity, policy, budgets, credentials, or tool execution |

Configured identities such as `investigator` and `auditor` are labels the runtime puts on a run. They are not cryptographic workload identity. A field named `identity` in model output is rejected.

Provider credentials, when a live adapter is used, come from the operator's AWS profile or the Ollama endpoint the operator named. The broker cannot grant an S3 permission the profile does not already have. Credential values are not placed in worker context, reports, or log lines by these paths. A content hash, an S3 ETag, or a fixture path does not authenticate the document.

## What each module owns

Governance loads one versioned bundle: mission, workflow, the investigation skill, the `diagnostics.read` argument schema, per-identity allow-lists, bundle-wide limits, resource aliases, and cursor-hub provenance. Rust pack rules are developer guidance for this repository. They are not loaded into the worker and they are not executed as policy. The implementation is one Rust process; see ADR 0002.

The broker allows a request only when the runtime identity has that operation and alias. It resolves the alias with the bundle for the selected provider (`fixture` or `s3`) and returns the exact target plus the limits that apply to that call. A decision is stored and then discarded as authority. The next call is evaluated again.

The gateway is transport-neutral. MCP is not implemented. For every proposal it validates the closed schema, rejects unknown fields, asks the broker, records the decision, and calls the adapter only if the decision is allow. Denials do not start a provider attempt. Transient read failures retry inside the same persisted provider budget. Authorization failures are not retried by the gateway.

The runtime admits an event in a SQLite transaction. The same id and payload returns the existing run. The same id and a different payload is a conflict. The loop is capped by model turns, tool proposals (including denials), provider attempts, duration, request timeout, input size, and result size. `submit_report` is a completion message, not a provider tool. Completion requires the required `diagnostics.read` evidence for the event's alias and a report that cites only evidence from that run.

## State

The state file is a regular, non-symlink SQLite database marked with this application's id. Inspection of a `running` row from a dead process marks it `interrupted`. Replay opens the file read-only. Neither inspection nor replay calls a model or a provider. A new attempt is a new event id. This is single-process local state, not distributed exactly-once delivery. Two commands should not share a database at the same time; same-event admission is transactional so the second caller observes the first run instead of starting another.

## Modes

| Selection | What a success shows |
| --- | --- |
| Scripted worker, fixture service | The workflow, policy, and report path. No model and no cloud call. |
| Real model, fixture service | The configured model took part. The service data is still a fixture. |
| Scripted worker, live S3 | The AWS read path worked for that object. The worker is not a model. |
| Real model and live S3 | That pair completed this workflow. It still does not prove an incident diagnosis. |

`diagnostics.read` is provider-neutral. Fixture and S3 adapters both return the same result shape. That does not prove two real providers interoperate.

## Out of scope

SSO, distributed workers, a policy language, mutating cloud actions, automatic remediation, a web UI, and mid-run revocation. The process is not a sandbox for arbitrary code.
