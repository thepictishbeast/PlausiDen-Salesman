#!/usr/bin/env bash
# salesman-daily.sh — the operator's daily runbook.
#
# The CLI has 60+ subcommands; this script is the AUTHORITATIVE
# answer to "what do I run, in what order, every day?". Each
# section is one human-comprehensible phase of a sales day. Pause
# points are explicit so the operator stays in the loop on every
# real send.
#
# Usage:
#   SALESMAN_CAMPAIGN=acme-warmup SALESMAN_PRODUCT=Sentinel ./salesman-daily.sh
#   ./salesman-daily.sh --campaign acme-warmup --product Sentinel
#
# Hard rules embedded here:
#   - never auto-`send-pending --for-real`. Operator confirms.
#   - never skip the doctor preflight.
#   - dry-run approve-all first, never bulk-approve without
#     reviewing the held drafts.

set -euo pipefail

# ---------------------------------------------------------------- args
CAMPAIGN="${SALESMAN_CAMPAIGN:-}"
PRODUCT="${SALESMAN_PRODUCT:-}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --campaign) CAMPAIGN="$2"; shift 2 ;;
    --product)  PRODUCT="$2";  shift 2 ;;
    --help|-h)
      echo "Usage: $0 [--campaign NAME] [--product NAME]"
      echo "  Or set SALESMAN_CAMPAIGN / SALESMAN_PRODUCT in env."
      exit 0
      ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

if [[ -z "$CAMPAIGN" || -z "$PRODUCT" ]]; then
  echo "ERROR: --campaign and --product (or SALESMAN_CAMPAIGN / SALESMAN_PRODUCT) are required." >&2
  exit 2
fi

SALESMAN="${SALESMAN_BIN:-salesman}"

section() {
  echo
  echo "=================================================================="
  echo "$*"
  echo "=================================================================="
}

pause_for() {
  if [[ "${SALESMAN_AUTOPILOT:-0}" == "1" ]]; then
    echo "(autopilot on; skipping pause)"
    return
  fi
  echo
  read -r -p ">>> $*  press Enter to continue, Ctrl-C to abort: " _
}

# ------------------------------------------------------------- 1. doctor
section "[1/9] doctor — preflight health check"
$SALESMAN doctor

# ----------------------------------------------------- 2. trigger scan
section "[2/9] triggers scan — fresh OSINT signals (GDELT + HN)"
$SALESMAN triggers scan --campaign "$CAMPAIGN"

# ------------------------------------------------ 3. auto-draft on triggers
section "[3/9] triggers draft — auto-draft on top-5 unused triggers"
$SALESMAN triggers draft --campaign "$CAMPAIGN" --product "$PRODUCT" --top 5

# --------------------------------------------------- 4. inbox + classify
section "[4/9] inbox-poll — pull new replies from IMAP"
$SALESMAN inbox-poll || echo "(inbox-poll soft-failed; continuing)"

section "[5/9] classify-replies — classify + extract interests + inline alert"
$SALESMAN classify-replies --batch 50

# --------------------------------------------- 6. draft replies for queue
section "[6/9] draft-replies — auto-draft responses to engaged/question/objection"
$SALESMAN draft-replies --batch 25 \
  $( [[ -f samples/pricing.toml      ]] && echo "--pricing-catalog samples/pricing.toml" ) \
  $( [[ -f samples/meeting-slots.toml ]] && echo "--meeting-slots samples/meeting-slots.toml" ) \
  $( [[ -f samples/objections.toml   ]] && echo "--objections samples/objections.toml" )

# --------------------------------------------------- 7. fact-check queue
section "[7/9] fact-check — bulk fact-trace + personalization gates"
$SALESMAN fact-check --campaign "$CAMPAIGN" || echo "(fact-check flagged drafts; review with 'salesman review')"

# --------------------------------------------------- 8. approve-all preview
section "[8/9] approve-all --dry-run — preview which drafts would auto-approve"
$SALESMAN approve-all --campaign "$CAMPAIGN" --dry-run

pause_for "Review the held drafts above. Run 'salesman review' to inspect, \
'salesman reject --touch <UUID>' to drop, then 'salesman approve-all --campaign $CAMPAIGN' \
when you're ready to bless the clean ones. Send is STILL gated by 'send-pending --for-real'."

# --------------------------------------------------- 9. operator briefing
section "[9/9] next-best-actions — today's prioritized to-do"
$SALESMAN next-best-actions --campaign "$CAMPAIGN"

section "summary — last 24h pipeline"
$SALESMAN summary --since-hours 24

echo
echo "Daily cycle complete. Next: review + 'salesman send-pending --campaign $CAMPAIGN --for-real --confirm-typed' when ready."
