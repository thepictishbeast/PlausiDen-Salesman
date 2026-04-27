# Salesman systemd units

These templates run Salesman on a Debian/Ubuntu VPS. They assume:

- `/usr/local/bin/salesman` and `/usr/local/bin/salesman-api` are installed
- A `salesman` user/group exists (created by `scripts/deploy.sh`)
- `/etc/salesman.env` exists (mode 0640, owner `root:salesman`)
- `/opt/salesman/{data,logs}` are world-readable / owner-writable

## Install

```sh
sudo cp deploy/systemd/*.service deploy/systemd/*.timer /etc/systemd/system/
sudo systemctl daemon-reload

# Long-running web API:
sudo systemctl enable --now salesman-api.service

# Periodic background jobs:
sudo systemctl enable --now salesman-inbox-poll.timer
sudo systemctl enable --now salesman-classify.timer
sudo systemctl enable --now salesman-audit-chain.timer
```

## Verify

```sh
systemctl status salesman-api
systemctl list-timers 'salesman-*'
journalctl -u salesman-inbox-poll.service --since="10 min ago"
journalctl -u salesman-audit-chain.service --since="2 days ago"
```

## Hardening

Every unit applies a defence-in-depth lockdown:

- `NoNewPrivileges=true` — no setuid escalation
- `ProtectSystem=strict` — `/usr` and friends are read-only
- `ProtectHome=true` — home dirs are inaccessible
- `PrivateTmp=true` — per-service `/tmp`
- `PrivateDevices=true` — only `/dev/null` and friends
- `ProtectKernelTunables/Modules/ControlGroups` — no kernel poking
- `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6` — no raw / netlink
- `RestrictNamespaces=true` — can't call unshare
- `LockPersonality=true` — frozen exec personality
- `MemoryDenyWriteExecute=true` — W^X
- `RestrictRealtime=true` — no SCHED_FIFO
- `SystemCallArchitectures=native` — only the host's arch
- `ReadWritePaths=/opt/salesman/{data,logs}` — only these are writable

Adjust `ReadWritePaths` if you mount data elsewhere.

## Audit-chain timer

The daily `audit-chain` walks the entire receipt hash chain and exits
non-zero on the first break. A failing run shows up red in
`systemctl status` and in `journalctl`. Hook a monitoring system
(Sentinel, Prometheus alertmanager, or a Slack webhook) to alert on
unit-failure.

If the chain is intact, you have a daily, automated, cryptographic
proof that nothing has been inserted, deleted, or altered in the
audit log since the previous run.
