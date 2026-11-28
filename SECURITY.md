# Security

## Supported versions

Security fixes apply to the latest `main` branch of this public repository. There is no separate private support channel.

## Reporting a vulnerability

Use **GitHub private vulnerability reporting** (Security Advisories):

https://github.com/Kolzodev712/company-ai-poc/security/advisories/new

Do not open public issues that include credentials, AWS profile contents, or private data.

This is reference software. The maintainer triages reports as capacity allows; there is no guaranteed response time (no SLA).

## Secrets

- Never commit AWS credentials, profile files, access keys, `live.toml`, SQLite state files, or private copies of the governance bundle containing real bucket names.
- Rotate any credential that may have been exposed.

## Scope notes

- The default demo is scripted and fixture-based. It makes no network or cloud calls.
- This project is not a sandbox for arbitrary code. Runtime identity labels are not cryptographic identity, and the SQLite audit record is not tamper-proof. See [docs/limitations.md](docs/limitations.md).
- Live Ollama and S3 access is opt-in with `--live`. The scoped S3 profile, not this program, is the cloud authorization boundary.
