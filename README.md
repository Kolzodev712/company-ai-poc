# company-ai-poc

A local Rust proof of concept for four reusable workflow pieces: governance, a security broker, a tooling gateway, and a runtime. The business scenario is one example, not a workflow for every company.

An investigator receives a diagnostic event for `demo-service-a`, reads one policy-approved snapshot, and writes a local report. The report separates recorded facts, hypotheses, and missing information. That structure does not prove the diagnosis is true.

cursor-hub is a read-only reference. This project does not modify it. Recorded source hashes live in `governance/bundle.json`.

## Architecture

One local Rust process. Four responsibilities: governance, a security broker, a tooling gateway, and a runtime. The example is a diagnostic investigation of `demo-service-a`. It is not a platform, a sandbox, or a proof that a diagnosis is true. More detail is in [docs/architecture.md](docs/architecture.md), [docs/adapters.md](docs/adapters.md), and [docs/adr/0002-rust-runtime.md](docs/adr/0002-rust-runtime.md). cursor-hub is read-only reference material. It is not imported and it is not started.

### 1. System context

```mermaid
flowchart LR
    operator["Operator"]
    cli["company-ai CLI"]
    process["One Rust process"]
    files["Operator files<br/>bundle, skill, config, fixtures"]
    hub["cursor-hub checkout<br/>read-only provenance"]
    sqlite["SQLite state file"]
    reports["JSON and Markdown reports"]
    ollama["Ollama HTTP API<br/>only with --live"]
    s3["AWS S3 GetObject<br/>only with --live"]

    operator --> cli
    cli --> process
    files --> process
    hub -.->|"hash check on validate"| process
    process --> sqlite
    process --> reports
    process -.->|"explicit opt-in"| ollama
    process -.->|"explicit opt-in"| s3
```

The solid arrows are the default fixture path. The dashed arrows do not run unless the operator names that live adapter and passes `--live`. A missing live setting is a setup error. The process does not switch to fixtures.

### 2. Module map

```mermaid
flowchart TB
    subgraph crate["crate company-ai"]
        main["main.rs"]
        cli["cli.rs"]
        config["config.rs"]
        gov["governance.rs"]
        runtime["runtime.rs"]
        workers["workers/<br/>scripted.rs · ollama.rs"]
        gateway["gateway.rs"]
        broker["broker.rs"]
        adapters["adapters/<br/>fixture.rs · s3.rs"]
        store["store.rs"]
        report["report.rs"]
        model["model.rs<br/>closed Serde contracts"]
        io["runtime_io.rs<br/>Tokio only for live I/O"]
    end

    main --> cli
    cli --> config
    cli --> gov
    cli --> runtime
    runtime --> workers
    runtime --> gateway
    runtime --> store
    runtime --> report
    gateway --> broker
    gateway --> adapters
    adapters --> io
    gov --> model
    broker --> model
    gateway --> model
```

| Module | Owns | Does not own |
| --- | --- | --- |
| Governance | Load and check the versioned bundle, skill hash, fixture containment, hub provenance | Permission decisions during a run |
| Security broker | One deny-by-default decision and, when allowed, one `AuthorizedRead` | Credentials, cloud IAM, reusable tokens |
| Tooling gateway | Validate, authorize, enforce limits, call the adapter, record the attempt | Direct model or provider calls from the worker |
| Runtime | Event admission, worker loop, budgets, completion checks | Provider SDK types |
| Workers | Propose `diagnostics.read` or `submit_report` | Executing either one |
| Adapters | Read the authorized target, or return a typed failure | Choosing a different bucket, key, path, or credential |
| Store | Transactional SQLite history | Replay of model text or provider calls |

### 3. Trust boundaries

```mermaid
flowchart TB
    subgraph trusted["Trusted operator and runtime"]
        bundle["Governance bundle"]
        cfg["Config: identity, endpoint, profile, region"]
        rt["Runtime-assigned identity, workflow, run id"]
        broker["Broker resolves the alias"]
        permit["AuthorizedRead<br/>exact target and limits"]
    end

    subgraph untrusted["Untrusted data"]
        worker["Worker output"]
        payload["Provider document and log text"]
        reporttext["Model observations, hypotheses, missing list"]
    end

    bundle --> broker
    cfg --> rt
    rt --> broker
    worker -->|"closed parse, unknown fields rejected"| broker
    broker --> permit
    permit --> adapter["Adapter read"]
    adapter --> payload
    payload -->|"stored as evidence, not as instructions"| reporttext
```

| Boundary | What crosses it | What cannot cross it |
| --- | --- | --- |
| Worker to gateway | A JSON proposal or final report | Identity, approval, policy, budgets, bucket, key, endpoint, role, credentials |
| Broker to adapter | `AuthorizedRead` for one alias | A later request, a broader prefix, another credential |
| Provider to runtime | Bounded diagnostic JSON | Policy rewrites, skill rewrites, budget increases |
| Operator to process | Bundle, skill, config, `--live` | A model-supplied replacement for those files |

Configured names such as `investigator` and `auditor` are runtime labels. They are not cryptographic workload identity. A content hash, an S3 ETag, or a fixture path is recorded and is not authentication.

### 4. Type narrowing on one tool call

```mermaid
flowchart LR
    raw["Untrusted JSON value"]
    parsed{"Closed parse"}
    unknown["Rejected field or unknown tool<br/>adapter not called"]
    proposal["Validated proposal<br/>diagnostics.read plus alias"]
    decision{"Broker"}
    deny["Denied<br/>zero adapter calls"]
    permit["AuthorizedRead"]
    read["Adapter reads that target only"]
    evidence["Evidence row for this run"]

    raw --> parsed
    parsed -->|extra field or bad shape| unknown
    parsed -->|known operation| proposal
    proposal --> decision
    decision -->|default| deny
    decision -->|identity, operation, and alias match| permit
    permit --> read
    read --> evidence
```

`AuthorizedRead` is constructed only in the broker. The adapter trait accepts that type, not a raw alias and not a model-supplied bucket or key. The compile-time type does not replace the runtime checks: a denied alias, a spoofed identity field, and a bucket override are still tested.

### 5. Successful fixture sequence

```mermaid
sequenceDiagram
    actor Operator
    participant CLI
    participant Runtime
    participant Store
    participant Worker
    participant Gateway
    participant Broker
    participant Fixture

    Operator->>CLI: demo or run, scripted plus fixture
    CLI->>Runtime: handle_event
    Runtime->>Store: admit event in a transaction
    alt same id and same payload
        Store-->>Runtime: existing run, no new work
    else same id and different payload
        Store-->>Runtime: conflict
    else new event
        Store-->>Runtime: run status running
        Runtime->>Worker: mission, skill, event, tool list
        Worker-->>Runtime: diagnostics.read alias
        Runtime->>Gateway: proposal
        Gateway->>Broker: identity from runtime, alias from proposal
        Broker-->>Gateway: AuthorizedRead or deny
        alt denied
            Gateway-->>Runtime: denial, no fixture read
        else allowed
            Gateway->>Store: provider attempt started
            Gateway->>Fixture: read authorized path only
            Fixture-->>Gateway: bounded diagnostic or typed failure
            Gateway->>Store: evidence and attempt outcome
        end
        Worker-->>Runtime: submit_report
        Runtime->>Runtime: same-run evidence and required read
        Runtime->>Store: succeeded or failed, report paths
    end
```

`submit_report` stays inside the runtime. It is not a provider operation and it does not pass through the gateway.

### 6. Run lifecycle

```mermaid
stateDiagram-v2
    [*] --> Running: new event admitted
    Running --> Succeeded: required evidence and valid same-run report
    Running --> Failed: denial without evidence, bad output, timeout, budget, or provider error
    Running --> Interrupted: process stopped before finish, seen on later inspect
    Succeeded --> Succeeded: same event id and payload
    Failed --> Failed: same event id and payload
    Interrupted --> Interrupted: same event id does not resume
```

A completion status requires the workflow checks. The worker saying the investigation is done is not enough. A new attempt is a new event id. Replay opens the database read-only and performs no model or provider call. It does not reproduce model text.

Budgets that end the loop, all persisted and checked between steps:

| Budget | What counts |
| --- | --- |
| Model turns | Each worker call |
| Tool proposals | Each proposal, including denials |
| Provider attempts | Each adapter read, including one transient retry |
| Duration | Checked between steps. Each live call gets a timeout capped by time remaining |
| Input and result size | Worker context and diagnostic bytes |

Authorization failures are not retried by the gateway. A transient read failure may be tried once more, and that second attempt counts against the same provider budget. SDK retries for S3 are disabled. Ollama is asked for a finite `num_predict`. Neither is a promise that a remote process stops at that instant.

### 7. Local state

```mermaid
erDiagram
    EVENTS ||--|| RUNS : admits
    RUNS ||--|{ STEPS : records
    RUNS ||--|{ DECISIONS : records
    RUNS ||--|{ EVIDENCE : records

    EVENTS {
        string event_id PK
        string fingerprint
        string payload_json
        string received_at
    }
    RUNS {
        string run_id PK
        string event_id UK
        string status
        string identity
        string policy_version
        string bundle_fingerprint
        string worker_mode
        string service_mode
        string budget_json
    }
    STEPS {
        string step_id PK
        string run_id FK
        int seq
        string kind
        string status
    }
    DECISIONS {
        string decision_id PK
        string run_id FK
        bool allowed
        string reason
        string policy_version
        string resource_alias
    }
    EVIDENCE {
        string evidence_id PK
        string run_id FK
        string operation
        string resource_alias
        string content_sha256
        string payload_json
    }
```

The database is one regular, non-symlink file with this application's id. It is not a distributed log and it is not exactly-once delivery. Two commands should not share one database at the same time. Same-event admission is transactional, so a second caller sees the first run instead of starting another.

### 8. How a run is selected

Worker mode and service mode are independent.

```mermaid
flowchart TD
    start["cargo run"] --> demo{"demo command?"}
    demo -->|yes| fixturePath["Scripted worker and fixture files"]
    demo -->|no| flags["Config plus optional --worker and --service"]
    flags --> live{"ollama or s3 selected?"}
    live -->|no| fixturePath
    live -->|yes| opt{"--live and settings present?"}
    opt -->|no| setup["Setup error. No fixture fallback"]
    opt -->|yes| chosen["Selected worker and selected service only"]
    fixturePath --> report["Local report"]
    chosen --> report
```

| Worker | Service | What a success shows |
| --- | --- | --- |
| Scripted | Fixture | Workflow, policy, and local report. No model and no cloud call |
| Ollama | Fixture | The named model took part. The document is still a fixture |
| Scripted | S3 | GetObject for the alias's object. The worker is not a model |
| Ollama | S3 | That pair completed this workflow. It still does not prove an incident diagnosis |

### 9. Governance inputs versus worker view

```mermaid
flowchart LR
    subgraph bundle["Operator bundle"]
        mission["Mission and workflow"]
        skill["Investigation skill"]
        policy["Allow-lists and limits"]
        aliases["Alias to fixture path and S3 key"]
        provenance["cursor-hub path hashes"]
    end

    subgraph prompt["Worker context"]
        seenMission["Mission objective"]
        seenSkill["Selected skill text"]
        seenEvent["This event"]
        seenTools["diagnostics.read and submit_report"]
        seenEvidence["Same-run evidence summaries"]
    end

    mission --> seenMission
    skill --> seenSkill
    policy -.->|"not copied"| hidden["Hidden from the worker"]
    aliases -.-> hidden
    provenance -.-> hidden
```

The skill is a narrow investigation instruction adapted from cursor-hub fault-first reporting and system-orchestration evidence rules. Rust pack rules and the hub design workflow are developer guidance. They are not loaded into the worker and they are not a permission engine.

### 10. What this chart does not claim

- The process is not a sandbox for arbitrary code.
- Fixture identities are not authentication.
- A passing structural report is not a true diagnosis.
- Fixture versus S3 shows one operation contract. It does not show two live providers interoperating.
- Mid-run policy revocation, distributed workers, and automatic resume of an interrupted run are outside this version.

## Install

Rust 1.85 or newer. From this directory:

```bash
cargo build --release
```

The debug binary is `target/debug/company-ai`. After `cargo build --release`, it is `target/release/company-ai`. The commands below use `cargo run --` so the binary does not need to be on `PATH`.

Fixture runs do not call AWS or Ollama.

## Fixture demo

```bash
cargo run -- validate --bundle governance/bundle.json --fixture-dir governance/fixtures --config examples/config.example.toml --hub ../cursor-hub
cargo run -- demo --output var/demo
cargo run -- inspect --database var/demo/state.sqlite --run RUN_ID
cargo run -- replay --database var/demo/state.sqlite --run RUN_ID
cargo run -- export --database var/demo/state.sqlite --run RUN_ID
```

`demo` prints `run_id:`. Use that value as `RUN_ID`. It always uses the scripted worker and the fixture service. It does not read `live.toml` and it does not use the network. Running it again with the same event returns the original run and does not read the fixture again.

Reports are `var/demo/reports/<run_id>.json` and `.md`. The fresh Rust verification copy is `examples/demo-run/run_7882d2217f2f473da0b3e51de0235363.md`. Paths inside a config file are relative to the working directory.

## Tests

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

The live smoke test is ignored unless you run it explicitly. An ignored test is not a passed live call.

## Other commands

```bash
cargo run -- run --config examples/config.example.toml --event examples/events/diagnostic-demo-service-a.json --worker scripted --service fixture
```

```bash
cargo run -- run --config live.toml --event examples/events/diagnostic-demo-service-a.json --worker ollama --service fixture --live
cargo run -- run --config live.toml --event examples/events/diagnostic-demo-service-a.json --worker scripted --service s3 --live
cargo run -- run --config live.toml --event examples/events/diagnostic-demo-service-a.json --worker ollama --service s3 --live
```

Check live settings without contacting a provider:

```bash
cargo run -- live check --config live.toml --event examples/events/diagnostic-demo-service-a.json
```

Bounded smoke tests, only after `live check` is ready:

```bash
cargo run -- live smoke --config live.toml --target ollama
cargo run -- live smoke --config live.toml --target s3 --resource demo-service-a
```

Exit codes: `0` completed investigation or a valid check, `1` recorded failure, `2` setup, validation, or event conflict.

## Configure a live run

Copy `examples/live.example.toml` to `live.toml` (gitignored). Set:

- `worker.mode` to `ollama` and `worker.model` to a model already installed in Ollama. The default endpoint is `http://127.0.0.1:11434`. A non-local endpoint also requires `worker.allow_remote = true`.
- `service.mode` to `s3`, `service.s3.profile` to an AWS profile name, and `service.s3.region` to that profile's region.
- In a private copy of the bundle, replace `YOUR_BUCKET` for the alias you will read. The example object key is `diagnostics/demo-service-a.json`.

The profile needs `s3:GetObject` on that object only. See [docs/adapters.md](docs/adapters.md). This program does not create buckets, upload objects, or change IAM. The S3 client uses that named profile's credential file, not an ambient default account, and it does not fall back to fixtures.

## Restart an interrupted run

`inspect` marks a run left in `running` as `interrupted` and does not call a provider. Submitting the same event id returns that run. Start a deliberate new attempt with a new event id. Replay reads history and does not call a model or a provider.

## Layout

| Path | Role |
| --- | --- |
| `src/governance.rs` | Bundle loading and provenance checks |
| `src/broker.rs` | Deny-by-default decisions and `AuthorizedRead` |
| `src/gateway.rs` | The only `diagnostics.read` path |
| `src/runtime.rs` | Bounded event loop and completion checks |
| `governance/` | Mission, skill, policy, fixtures, cursor-hub provenance |
| `docs/architecture.md` | Trust-boundary notes |
| `docs/adapters.md` | Extension points |
| `docs/adr/` | Decisions, including the move to Rust |
