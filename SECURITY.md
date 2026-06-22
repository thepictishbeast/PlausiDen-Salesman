# SECURITY.md — PlausiDen-Salesman

## Threat model

Salesman handles:
- Prospect contact data (PII; protected by GDPR / CASL / CAN-SPAM)
- Email send credentials (SMTP password, DKIM private key)
- Receipt-signing key (Ed25519 private key)
- Reply content (privileged — recipient's words back to us)

Adversaries:
- External attackers compromising the VPS
- Malicious recipients trying to inject content via reply ingestion
- Compromised dependency (supply-chain) injecting send-redirect or PII exfiltration
- Insider mistake (operator typo sends 10k messages instead of 10)

## Mitigations

| Threat | Mitigation |
|---|---|
| External compromise | Salesman runs as dedicated unprivileged user; no SSH-as-salesman; secrets via EnvironmentFile `/etc/salesman.env` (mode 0640, `root:salesman`) — see [docs/DEPLOYMENT_GUIDE.md](docs/DEPLOYMENT_GUIDE.md) / [docs/OPERATOR_HANDBOOK.md](docs/OPERATOR_HANDBOOK.md) |
| Reply injection (HTML, URL parse, etc.) | Reply ingestion is parse-only (mail-parser); HTML is never rendered or executed, and only the decoded plain-text body is read by the classifier/DSN detector. |
| Supply chain | `cargo audit` + `cargo deny` in CI; vendored deps where critical. Drafting/reply use SaaS Claude/Gemini behind a PII redaction boundary (see note below) |
| Operator mistake | Per-invocation send-cap default 25 (`--max-batch`), further limited by a sender-warmup curve (5/10/25/100 by domain age); confirmation prompt CLI-side; pre-merge audit on schema changes |
| Spam / blacklist | Per-domain throttle, SPF/DKIM/DMARC enforced, monitor delivery + bounce rate, auto-skip (soft-quarantine) a domain with ≥3 hard bounces in 24h (configurable via `--domain-quarantine-threshold`) |
| PII leakage in logs | Log call sites avoid emitting prospect PII; redaction is applied before SaaS-LLM calls and redaction telemetry is counts-only (see docs/PII_REDACTION_BOUNDARY.md) |

### PII redaction boundary

Drafting and reply handling call SaaS LLMs (Claude/Gemini), but prospect PII
(email, phone, company name, homepage) is redacted before the call and
rehydrated after, so PII does not leave the box in the clear. Residual
free-text names (e.g. a person named in a `description`) are an accepted v1
limitation. Local-only LFI is deferred (ADR-0003). See
[docs/PII_REDACTION_BOUNDARY.md](docs/PII_REDACTION_BOUNDARY.md).

## Reporting a vulnerability

Email security@plausiden.com (PGP key TBD) — do NOT open a public issue.

## AVP-2 tier targets

- v0.1: Tier 1 (existence proof + dependency audit)
- v0.3: Tier 2 (failure resilience under fault injection)
- v0.5: Tier 3 (full adversarial security pass including reply-injection fuzz)
