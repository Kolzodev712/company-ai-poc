# company-ai-poc

A local Rust proof of concept for four reusable workflow pieces: governance, a security broker, a tooling gateway, and a runtime. The business scenario is one example, not a workflow for every company.

An investigator receives a diagnostic event for `demo-service-a`, reads one policy-approved snapshot, and writes a local report. The report separates recorded facts, hypotheses, and missing information. That structure does not prove the diagnosis is true.

cursor-hub is a read-only reference. This project does not modify it. Recorded source hashes live in `governance/bundle.json`.

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

The profile needs `s3:GetObject` on that object only. See `docs/adapters.md`. This program does not create buckets, upload objects, or change IAM. The S3 client uses that named profile's credential file, not an ambient default account, and it does not fall back to fixtures.

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
| `docs/architecture.md` | Trust boundaries |
| `docs/adapters.md` | Extension points |
| `docs/adr/` | Decisions, including the move to Rust |

This process is not a sandbox. The gateway blocks the one business tool it exposes. It does not contain a shell, an evaluator, or a model that can ignore the operating-system privileges of this process.
