# company-ai-poc

A small, local proof of concept for four reusable workflow pieces: governance, a security broker, a tooling gateway, and a runtime. The business scenario is one example, not a workflow for every company.

An investigator receives a diagnostic event for `demo-service-a`, reads one policy-approved snapshot, and writes a local report. The report separates recorded facts, hypotheses, and missing information. That structure does not prove the diagnosis is true.

cursor-hub is a read-only reference. This project does not import it, install its packs, or change it. Recorded source hashes live in `governance/bundle.json`.

## Install

Python 3.11 or newer. From this directory:

```bash
python3 -m venv .venv
. .venv/bin/activate
python -m pip install -e '.[dev]'
```

`boto3` is included in the `dev` and `live` extras so the S3 adapter can be imported. Fixture runs do not call AWS or Ollama.

## Fixture demo

```bash
company-ai validate --bundle governance/bundle.json --fixture-dir governance/fixtures --config examples/config.example.toml --hub ../cursor-hub
company-ai demo --output var/demo
company-ai inspect --database var/demo/state.sqlite --run RUN_ID
company-ai replay --database var/demo/state.sqlite --run RUN_ID
company-ai export --database var/demo/state.sqlite --run RUN_ID
```

`demo` prints `run_id:`. Use that value as `RUN_ID`.

`company-ai demo` always uses the scripted worker and the fixture service. It does not read `live.toml` and it does not use the network. Running it again with the same event returns the original run and does not read the fixture again.

Reports are `var/demo/reports/<run_id>.json` and `.md`. The verification copy is `examples/demo-run/run_149738b2408b4b7a85ea2344f17aa83d.md`. Paths inside a config file are relative to the working directory, so run commands from this directory.

## Tests

```bash
python -m pytest -q
```

Live smoke tests are skipped unless you set `COMPANY_AI_LIVE=1` and provide `live.toml`. A skip is not a passed live call.

## Other commands

Run one event. Worker and service are independent. Live calls also need `--live`.

```bash
company-ai run --config examples/config.example.toml --event examples/events/diagnostic-demo-service-a.json --worker scripted --service fixture
```

```bash
company-ai run --config live.toml --event examples/events/diagnostic-demo-service-a.json --worker ollama --service fixture --live
company-ai run --config live.toml --event examples/events/diagnostic-demo-service-a.json --worker scripted --service s3 --live
company-ai run --config live.toml --event examples/events/diagnostic-demo-service-a.json --worker ollama --service s3 --live
```

Check live settings without contacting a provider:

```bash
company-ai live check --config live.toml --event examples/events/diagnostic-demo-service-a.json
```

Bounded smoke tests, only after `live check` is ready:

```bash
company-ai live smoke --config live.toml --target ollama
company-ai live smoke --config live.toml --target s3 --resource demo-service-a
```

Exit codes: `0` completed investigation or a valid check, `1` recorded failure, `2` setup, validation, or event conflict.

## Configure a live run

Copy `examples/live.example.toml` to `live.toml` (gitignored). Set:

- `worker.mode` to `ollama` and `worker.model` to a model already installed in Ollama. The default endpoint is `http://127.0.0.1:11434`. A non-local endpoint also requires `worker.allow_remote = true`.
- `service.mode` to `s3`, `service.s3.profile` to an AWS profile name, and `service.s3.region` to that profile's region.
- In a private copy of the bundle, replace `YOUR_BUCKET` and the object key for the alias you will read. Do not commit account identifiers.

The profile needs `s3:GetObject` on that object only. See `docs/adapters.md`. This program does not create buckets, upload objects, or change IAM.

Missing live settings are an error. The run does not silently switch to fixtures.

## Restart an interrupted run

`company-ai inspect` marks a run left in `running` as `interrupted` and does not call a provider. Submitting the same event id returns that run. Start a deliberate new attempt with a new event id. Replay reads history and does not call a model or a provider; it does not reproduce model text.

## Layout

| Path | Role |
| --- | --- |
| `src/company_ai/governance.py` | Bundle loading and provenance checks |
| `src/company_ai/broker.py` | Deny-by-default decisions |
| `src/company_ai/gateway.py` | The only `diagnostics.read` path |
| `src/company_ai/runtime.py` | Bounded event loop and completion checks |
| `governance/` | Mission, skill, policy, fixtures, cursor-hub provenance |
| `docs/architecture.md` | Trust boundaries |
| `docs/adapters.md` | How to add an event source, adapter, or model backend |
| `docs/adr/0001-architecture.md` | Decisions |
| `docs/limitations.md` | What this does not prove, and hub follow-ups |

This process is not a sandbox. The gateway blocks the one business tool it exposes. It does not contain arbitrary Python, shell, or a model that can ignore the operating-system privileges of this process.
