# PlausiDen Tidy vs CCleaner for individual cleanup

> **Status: SEED — owner-review required.** Replace placeholders.

## Summary

CCleaner has been the default disk-cleanup tool on Windows for two
decades. PlausiDen Tidy is a sovereign, never-deletes-without-asking
alternative built around the principle that the user — not the
software — decides what goes. Tidy generates plans; you approve
each step.

## Feature matrix

| Dimension | PlausiDen Tidy | CCleaner |
|---|---|---|
| Default behaviour | Generate plan → wait for human approval | Run cleanup |
| Telemetry | None | Anonymous usage stats (opt-out) |
| Bundled offers | None | Avast antivirus historically bundled |
| Open source | TODO confirm Tidy licensing | Closed-source |
| Per-cleaner audit log | Yes (cryptographic receipt) | No |
| Backup before delete | Always (configurable) | Optional, off by default |

## Where Tidy wins

- **Never deletes without an approve step.** The 2017 CCleaner
  malware incident (supply-chain compromise) is exactly the failure
  mode Tidy is designed against: even if the binary is malicious,
  it can't act without a human signature on the plan.
- **Sovereign.** No cloud account. No telemetry. No bundled offers.
- **Auditable.** Every action produces a signed receipt; you can
  prove what ran and when.

## Where CCleaner wins

- **Maturity + recognition.** CCleaner has 20+ years of pattern
  database for Windows cruft. Tidy's database is younger and
  primarily Linux-focused.
- **Familiar UX for Windows users.** Tidy's plan-first model has a
  learning curve.
- **Piriform ecosystem.** Speccy, Defraggler, Recuva integrate well.

## Best fit

**Pick Tidy if:** you value approval-before-action and sovereignty
over convenience; you're on Linux primarily; you want auditable
cleanup actions.

**Pick CCleaner if:** you want one-click cleanup on Windows with
broad pattern coverage and you trust the supply chain (post-2017
Avast acquisition + improvements).

## Sources

> **TODO:** verify Tidy's actual feature set. Cite the 2017
> CCleaner incident only if relevant + with the right framing
> (illustrative, not accusatory).

*Last reviewed: TODO. PlausiDen has no commercial relationship with
Avast/Piriform.*
