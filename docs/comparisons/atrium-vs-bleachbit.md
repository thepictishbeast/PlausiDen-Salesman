# PlausiDen Atrium vs BleachBit for Linux disk cleanup

> **Status: SEED — owner-review required.** This page acknowledges
> the BleachBit lineage of Atrium openly; honest framing is the
> credibility hook.

## Summary

BleachBit is the Linux disk-cleanup tool. PlausiDen Atrium is a
fork-and-extend of BleachBit's cleaner engine, with the cleaner
catalog driven by JSON contracts emitted by sister PlausiDen
products (Tidy, AppGuard) so the catalog grows as those products
discover new cleanup opportunities. We did not build a competitor
from scratch; we extended a great open-source tool.

## Feature matrix

| Dimension | PlausiDen Atrium | BleachBit |
|---|---|---|
| Deployment | Self-hosted; bundled with PlausiDen suite | Standalone, system-wide |
| Cleaner catalog | Static + dynamic (JSON contracts from Tidy/AppGuard) | Static |
| GUI | Sovereign-design system; dark/light | GTK |
| Approval model | Plan → approve per cleaner | Per-cleaner toggle, then run |
| Audit trail | Cryptographic receipts | Verbose log, not signed |
| OS support | Linux primary (TODO: Windows?) | Linux + Windows |

## Where Atrium wins

- **Dynamic cleaner catalog.** Tidy and AppGuard discover new
  cleanup opportunities (new caches, new app data) and emit
  JSON-described cleaners that Atrium picks up. The catalog grows
  as the ecosystem evolves.
- **Approval-first UX.** Atrium produces a plan you read before
  anything runs. BleachBit's "preview" mode is similar but optional.
- **Signed receipts.** Audit trail you can replay and verify
  cryptographically.

## Where BleachBit wins

- **Cross-platform from day one.** BleachBit ships on Windows;
  Atrium is Linux-first.
- **Maturity.** BleachBit's cleaner database is older and broader.
- **Standalone.** No PlausiDen suite required. If you just want one
  tool, BleachBit is cleaner.

## Best fit

**Pick Atrium if:** you're already in the PlausiDen ecosystem (Tidy
+ AppGuard contributing to the cleaner catalog); you want signed
audit receipts; you value the approval-first model.

**Pick BleachBit if:** you want a single-tool cleanup utility
(especially on Windows); you don't need cryptographic receipts; you
prefer the standalone independence.

## Sources

> **TODO:** link to BleachBit upstream (open-source license — the
> credit is mandatory, not optional). Link to PlausiDen-Atrium repo.

*Last reviewed: TODO. PlausiDen Atrium is forked from BleachBit;
upstream credit + license compliance are honored at every release.*
