#!/usr/bin/env bash
# salesman-doctor-watch.sh — fire a webhook when salesman doctor
# transitions RED → GREEN (or YELLOW → GREEN).
#
# Designed to run on a 5-min cron / systemd timer DURING the
# bootstrap window, so the operator gets pinged the moment env is
# fully wired and first send is unblocked.
#
# Once the system is green steady-state, disable this — the
# salesman-failure-alert@.service template handles ongoing
# regressions per-unit.
#
# Usage (cron / timer):
#   SALESMAN_ALERT_WEBHOOK_URL=https://hooks.slack.com/... \
#     ./salesman-doctor-watch.sh
#
# State file: /var/lib/salesman/doctor-state (last verdict).

set -euo pipefail

STATE_FILE="${SALESMAN_DOCTOR_STATE:-/var/lib/salesman/doctor-state}"
WEBHOOK="${SALESMAN_ALERT_WEBHOOK_URL:-}"
SALESMAN="${SALESMAN_BIN:-salesman}"

mkdir -p "$(dirname "$STATE_FILE")" 2>/dev/null || true

# Capture doctor output + exit code. Doctor exits non-zero on RED.
out="$($SALESMAN doctor 2>&1)" || true
verdict_line="$(echo "$out" | grep -E "^VERDICT:" | tail -1)"

if [[ "$verdict_line" =~ GREEN ]]; then
  current="GREEN"
elif [[ "$verdict_line" =~ YELLOW ]]; then
  current="YELLOW"
else
  current="RED"
fi

last="UNKNOWN"
if [[ -f "$STATE_FILE" ]]; then
  last=$(cat "$STATE_FILE")
fi
echo "$current" > "$STATE_FILE"

if [[ "$last" != "$current" && "$current" == "GREEN" ]]; then
  msg="salesman doctor is now GREEN (was $last). First real send is unblocked. Re-run \`salesman doctor\` to verify, then \`salesman send-pending --campaign foo --for-real --confirm-typed\`."
  echo "$msg"
  if [[ -n "$WEBHOOK" ]]; then
    payload=$(printf '{"text":"%s"}' "$(echo "$msg" | sed 's/"/\\"/g')")
    curl -sS --max-time 10 -X POST -H 'Content-Type: application/json' \
      -d "$payload" "$WEBHOOK" >/dev/null || echo "(webhook post failed)"
  fi
elif [[ "$last" != "$current" ]]; then
  echo "doctor: $last → $current (no notification — only GREEN transition pings)"
else
  echo "doctor: $current (unchanged from last run)"
fi
