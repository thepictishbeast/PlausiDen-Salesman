#!/usr/bin/env zsh
# Send the layman's-pitch email to william@plausiden.com via the
# plausiden mail server on submission port 587 (STARTTLS).
#
# Usage:
#   SMTP_PASS=... ./scripts/send_pitch.sh
#   # or, prompt for password (no shell-history leak):
#   ./scripts/send_pitch.sh
#
# Reads the body from drafts/email_to_william.txt.

set -e

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
BODY_FILE="${BODY_FILE:-$REPO_ROOT/drafts/email_to_william_v3.txt}"
SMTP_HOST="${SMTP_HOST:-mail.plausiden.com}"
SMTP_PORT="${SMTP_PORT:-587}"
SMTP_USER="${SMTP_USER:-william@plausiden.com}"
FROM_ADDR="${FROM_ADDR:-William Armstrong <william@plausiden.com>}"
TO_ADDR="${TO_ADDR:-william@plausiden.com}"
SUBJECT="${SUBJECT:-Cold outreach where the reply-handling does itself}"

if [[ ! -f "$BODY_FILE" ]]; then
  echo "FATAL: body file not found: $BODY_FILE" >&2
  exit 1
fi
if ! command -v swaks >/dev/null 2>&1; then
  echo "FATAL: swaks not installed (apt-get install swaks)" >&2
  exit 1
fi

# Strip any From:/To:/Subject: headers from the body file — we set them
# explicitly via swaks so we don't double up.
TMP_BODY=$(mktemp)
trap "rm -f $TMP_BODY" EXIT
awk 'BEGIN{header=1} /^$/{if(header){header=0; next}} {if(!header)print}' "$BODY_FILE" > "$TMP_BODY"
if [[ ! -s "$TMP_BODY" ]]; then
  # No header block detected — pipe the file through unchanged.
  cp "$BODY_FILE" "$TMP_BODY"
fi

if [[ -z "${SMTP_PASS:-}" ]]; then
  printf 'SMTP password for %s on %s:%s: ' "$SMTP_USER" "$SMTP_HOST" "$SMTP_PORT" >&2
  stty -echo
  IFS= read -r SMTP_PASS
  stty echo
  printf '\n' >&2
fi

if [[ -z "$SMTP_PASS" ]]; then
  echo "FATAL: empty password; aborting" >&2
  exit 1
fi

swaks \
  --to "$TO_ADDR" \
  --from "$FROM_ADDR" \
  --server "$SMTP_HOST:$SMTP_PORT" \
  --auth LOGIN \
  --auth-user "$SMTP_USER" \
  --auth-password "$SMTP_PASS" \
  --tls \
  --tls-protocol tlsv1_2 \
  --header "Subject: $SUBJECT" \
  --header "X-Salesman-Source: laptop-pitch-send" \
  --body @"$TMP_BODY"
