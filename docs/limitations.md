# Limitations and hub follow-ups

## Verification

Requires Rust 1.94.1 or newer (the highest `rust-version` among the locked dependencies, set by the AWS SDK stack).

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo run --locked -- demo --output var/demo
```

## What a passing fixture run shows

The scripted worker and fixture adapter complete the example investigation without network access. Policy denies the forbidden alias before any file read. Duplicate events do not create a second run. Reports cite evidence stored for that run.

## What it does not show

- A scripted fixture run does not show that a model participated or that AWS was reachable.
- An Ollama run against fixtures shows model participation only. It does not show a live service.
- A scripted run against S3 shows the GetObject integration only. It does not show a model.
- Both together show that pair for this workflow. They still do not show that a diagnosis is true, that the object is authentic, or that a production incident was monitored.
- Fixture identities are not cryptographic authentication. The broker cannot grant a permission the AWS profile lacks.
- Hashes detect accidental change of files we re-read. They do not stop a person who can write the SQLite file or the fixture directory.
- Duration limits are checked between steps, and each HTTP or SDK call receives a finite timeout. A stuck extension that ignores its timeout is not forcibly cancelled. `num_predict` is sent to Ollama; this program does not measure remote tokens.
- botocore retries are disabled. The runtime's single transient retry is counted. Other hidden retries are not claimed to be impossible inside a future dependency.
- Interrupted runs are not resumed. Mid-run policy revocation and multi-process scheduling are not implemented.
- Replay does not re-execute the model.
- One real provider was integrated. Fixture versus S3 shows the interface. It does not show interoperability between two live providers.

## Proposed cursor-hub follow-ups

These are design-only proposals. They were not applied to cursor-hub, and this project does not depend on them.

1. Publish a tiny import-safe helper for canonical JSON and SHA-256 fingerprints that does not import the simulation reducer, so a companion can depend on it instead of reimplementing the idea.
2. Mark skill files with a machine-readable audience, `cursor-design` or `runtime-excerpt`, so a runtime does not have to guess which prose is safe to put in a worker prompt.
3. Add a machine-readable epistemic status on simulation contracts stating that fixture approvals and `VERIFIED` are not authentication or live evidence. The prose already says this; a field would let companions test it.

The hub's offline reducer should stay simulation-only. This POC is the separate live-adapter implementation its execution-boundary note asks for, and only for one read operation.
