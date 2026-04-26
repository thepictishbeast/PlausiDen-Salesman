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
| External compromise | Salesman runs as dedicated unprivileged user; no SSH-as-salesman; secrets via systemd `LoadCredential=` + on-disk encryption |
| Reply injection (HTML, URL parse, etc.) | Reply ingestion strips HTML, validates encoding, never executes inline scripts |
| Supply chain | `cargo audit` + `cargo deny` in CI; vendored deps where critical; no SaaS LLM dependency |
| Operator mistake | Per-batch send-cap (default 50/batch, 200/day); confirmation prompt CLI-side; pre-merge audit on schema changes |
| Spam / blacklist | Per-domain throttle, SPF/DKIM/DMARC enforced, monitor delivery + bounce rate, auto-pause on >5% bounce |
| PII leakage in logs | `tracing` filters scrub email addresses + names from log lines; structured-logging contracts |

## Reporting a vulnerability

Email security@plausiden.com (PGP key TBD) — do NOT open a public issue.

## AVP-2 tier targets

- v0.1: Tier 1 (existence proof + dependency audit)
- v0.3: Tier 2 (failure resilience under fault injection)
- v0.5: Tier 3 (full adversarial security pass including reply-injection fuzz)
