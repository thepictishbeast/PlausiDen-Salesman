# 0005 — VPS access is SSH key-only; password disabled
> (decided)

## Context

The OpenClaw VPS (45.77.217.37) was originally provisioned with
SSH password auth enabled. The auth log showed continuous brute-
force attempts from random IPs (visible in `journalctl -u ssh`).
A weak-or-leaked root password on a public IPv4 is a credible
compromise vector.

The VPS hosts: Postgres (with the prospect database), the salesman
binary, the signing key for receipts (Ed25519), and SMTP credentials
(when configured). Compromise is ruinous.

## Decision

SSH on the VPS accepts only Ed25519 public-key authentication.
PasswordAuthentication is disabled in `/etc/ssh/sshd_config.d/`.
PermitRootLogin is set to `prohibit-password` (root login allowed
ONLY with key, not password).

Two separate keys are installed in authorized_keys for both `root`
and `salesman` accounts:

  - `id_openclaw` — the daily-driver key (no passphrase for ops
    convenience; protected by laptop disk encryption)
  - `id_openclaw_recovery` — separate key, separate file. Backup
    in case the primary is lost.

The Vultr web console + the system root password (still in
`/etc/shadow`) are the disaster-recovery path of last resort.

## Consequences

- ✅ Brute-force attacks against the SSH service produce zero
  successful auths regardless of password strength.
- ✅ Two keys means losing one doesn't lock out access.
- ✅ Vultr console is the always-available out-of-band path.
- ⚠️  If both private keys are lost AND the root password is
  forgotten, recovery requires Vultr support (rebuild from
  snapshot). Snapshots are taken weekly per the snapshot-reminder
  timer.
- ❌ We do NOT ship a third "rescue" key (e.g. an Anthropic-held
  emergency key). Adds attack surface for marginal recovery
  benefit.

## Alternatives considered

- **Password auth + fail2ban** — somewhat mitigates brute-force,
  but a leaked password still works. Lost because key-only is
  strictly better.
- **Hardware key (FIDO2) on every laptop** — gold standard, but
  the owner doesn't have one set up. Re-evaluate when they do.
- **Bastion host in front** — overkill for one-VPS deployment.

## Status

`decided 2026-04-26 by claude-code session`

## References

- `/root/.claude/projects/-/memory/project_openclaw_vps_access.md`
  (operator reference)
- `/etc/ssh/sshd_config.d/50-cloud-init.conf` on the VPS
- `~/.ssh/config` on laptop (openclaw / openclaw-salesman aliases)
