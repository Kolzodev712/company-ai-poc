## Related issue

Fixes #

## Problem

## Implementation summary

## Impact on high-scrutiny areas

- [ ] None / docs-only
- [ ] Touches governance validation, authorization, provider credentials, tool execution, evidence integrity, budgets, event admission, replay, or live provider access (call out below)

## Limitations and docs

- [ ] `docs/limitations.md`, README, or CHANGELOG updated if behavior or claims changed
- [ ] `governance/bundle.json` provenance hashes are unchanged (or the change is intentional and explained)

## Verification (local, no live providers)

Commands run:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

Results:

## AI assistance

- [ ] None
- [ ] Yes. I reviewed the change and can explain it: (describe)

## Known limitations

## Breaking changes

- [ ] None
- [ ] Yes. Describe migration or compatibility notes
