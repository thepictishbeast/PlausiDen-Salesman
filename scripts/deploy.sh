#!/usr/bin/env bash
# scripts/deploy.sh — one-command deploy to OpenClaw VPS.
#
# Steps:
#   1. tar source (excluding target/, .git/)
#   2. scp to VPS:/tmp
#   3. ssh: extract → cargo build --release -p salesman-cli
#   4. install /opt/salesman/bin/salesman (atomic via rename)
#   5. systemctl restart salesman-* units that are active
#
# Idempotent. Safe to re-run. Requires `ssh openclaw` working.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
HOST="openclaw"
# shellcheck disable=SC2034  # readable config; the remote heredoc uses literal paths
REMOTE_BUILD="/opt/salesman/build/PlausiDen-Salesman"
# shellcheck disable=SC2034  # readable config; the remote heredoc uses literal paths
REMOTE_BIN="/opt/salesman/bin/salesman"

echo "==> packaging source"
TAR="/tmp/salesman-deploy-$(date +%s).tar.gz"
tar --exclude='target' --exclude='.git' --exclude='Cargo.lock.bak' \
    -C "$(dirname "$REPO_DIR")" \
    -czf "$TAR" "$(basename "$REPO_DIR")"
echo "    $(du -h "$TAR" | cut -f1)"

echo "==> uploading"
scp -q "$TAR" "$HOST:/tmp/salesman-deploy.tar.gz"
rm -f "$TAR"

echo "==> remote build + install"
ssh "$HOST" bash <<'REMOTE'
set -euo pipefail
sudo -u salesman bash -lc '
  source ~/.cargo/env
  rm -rf /tmp/salesman-deploy
  mkdir -p /tmp/salesman-deploy
  tar -xzf /tmp/salesman-deploy.tar.gz -C /tmp/salesman-deploy
  rsync -a --delete \
        --exclude="target/" \
        /tmp/salesman-deploy/PlausiDen-Salesman/ \
        /opt/salesman/build/PlausiDen-Salesman/
  cd /opt/salesman/build/PlausiDen-Salesman
  cargo build --release -p salesman-cli 2>&1 | tail -3
  install -m 755 target/release/salesman /tmp/salesman-new
  mv /tmp/salesman-new /opt/salesman/bin/salesman
  /opt/salesman/bin/salesman --version
'
rm -f /tmp/salesman-deploy.tar.gz
echo "==> restarting active salesman units"
for u in $(systemctl list-units --state=active --no-pager --no-legend "salesman-*" | awk "{print \$1}"); do
  echo "    restart $u"
  sudo systemctl restart "$u"
done
REMOTE

echo "==> done"
