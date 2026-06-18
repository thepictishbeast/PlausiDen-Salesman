> # ⚠️ DO NOT USE — UNVERIFIED — UNSAFE ⚠️
>
> This software is **unverified and unsafe for any production use**.
> It is published publicly only for transparency, third-party audit,
> and reproducibility. Treat every commit as guilty until proven
> innocent.
>
> By using this code you accept:
> - **No warranty** of any kind, express or implied.
> - **No fitness** for any particular purpose.
> - **No guarantee** of correctness, safety, or freedom from defects.
> - **Zero liability** on the maintainer for any damages — data loss,
>   security compromise, financial loss, or any consequential damages.
>
> The code is under active engineering development per the
> [Adversarial Validation Protocol v2](https://github.com/thepictishbeast/PlausiDen-AVP-Doctrine/blob/main/AVP2_PROTOCOL.md).
> Every commit's default verdict is **STILL BROKEN**. AVP-2 requires
> a minimum of 36 verification passes before a `SHIP-DECISION:`
> annotation may be considered. **No commit in this repository has
> reached `SHIP-DECISION:` status.**

<!-- repo-label: product -->
<!-- repo-class: outreach-and-spoof-training-engine -->
<!-- repo-consumes: PlausiDen-CRM, PlausiDen-Mail, PlausiDen-AI (LFI), PlausiDen-Obs, PlausiDen-Audits, PlausiDen-AVP-Doctrine -->
<!-- repo-consumed-by: PlausiDen-Suite (distribution) -->

# PlausiDen-Salesman

Outreach + spoof-training engine for the PlausiDen ecosystem. Designed as
a sister to PlausiDen-CRM (pipeline / customer relationship state) and
PlausiDen-Mail (delivery channel).

## Why this exists

Two simultaneous needs the user has framed:

1. **Real outbound sales** — generate personalized outreach (email,
   LinkedIn message, call script) for PlausiDen's own go-to-market.
2. **Spoof-training + compliance-testing** — the same engine generates
   indistinguishable phishing / social-engineering exercises that
   defenders use to train recipients to spot manipulation.

The same primitives serve both: synthetic-but-realistic personalization,
multi-channel delivery, response tracking, audit trail. Whether the
output is "real outreach" or "training exercise" is a per-campaign tag.

## Stance

- Local-first, sovereignty-respecting, AVP-2 audited.
- Drafting/reply use SaaS Claude/Gemini, but prospect PII (email, phone, company name, homepage) is redacted before the call and rehydrated after (a redaction boundary; residual free-text names are an accepted v1 limitation). Local-only LFI is deferred — see ADR-0003 and [`docs/PII_REDACTION_BOUNDARY.md`](docs/PII_REDACTION_BOUNDARY.md).
- Every outreach is logged + attestable (PlausiDen-Obs).
- Compliance-test mode produces signed receipts: who got the test,
  what they did, when. Forms the audit trail for security-team reports.

## Status

Tier 0 has shipped (pre-go-live — no real send yet). See
[`HANDOFF.md`](HANDOFF.md) for current runtime + where we left off, and
[`ROADMAP.md`](ROADMAP.md) for what's next. **Read [`SCOPE.md`](SCOPE.md)
and [`docs/COLLABORATION.md`](docs/COLLABORATION.md) before contributing
or assigning.**

## Sister repos

- [PlausiDen-CRM](https://github.com/thepictishbeast/PlausiDen-CRM) (TBD) — pipeline + relationship state
- [PlausiDen-Mail](https://github.com/thepictishbeast/PlausiDen-Mail) (TBD) — outbound delivery channel
- [PlausiDen-AI](https://github.com/thepictishbeast/PlausiDen-AI) — LFI-driven personalization source
- [PlausiDen-Audits](https://github.com/thepictishbeast/PlausiDen-Audits) — CI-time audit gates
- [PlausiDen-Obs](https://github.com/thepictishbeast/PlausiDen-Obs) — structured logging + secrets
- [PlausiDen-AVP-Doctrine](https://github.com/thepictishbeast/PlausiDen-AVP-Doctrine) — validation tier targets
- [PlausiDen-Meta](https://github.com/thepictishbeast/PlausiDen-Meta) — ecosystem charter
- [PlausiDen-Runner](https://github.com/thepictishbeast/PlausiDen-Runner) — self-hosted CI
