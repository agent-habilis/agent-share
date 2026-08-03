#!/bin/bash
# S0.4 — run the multi-source throughput measurement under an emulated
# delayed link.
#
# Scoped to UDP on lo0 ONLY, so it cannot touch real network traffic: every
# peer in the test is on loopback. Saves and restores pf state on any exit,
# including Ctrl-C and failure.
#
# Usage:  sudo ./s04-delayed-link.sh [delay_ms] [reps] [secs]
# Example: sudo ./s04-delayed-link.sh 25 5 10

set -uo pipefail

DELAY_MS="${1:-25}"
REPS="${2:-5}"
SECS="${3:-10}"
PIPE=1
# Repo root: four levels up from docs/rfc/03-fofoca-blobs/harness/
REPO="${REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)}"

if [ "$(id -u)" -ne 0 ]; then
  echo "must run as root (dnctl/pfctl); re-run with sudo" >&2
  exit 1
fi

# --- record prior state so we can put it back exactly ------------------------
PF_WAS_ENABLED=no
if pfctl -s info 2>/dev/null | head -1 | grep -q Enabled; then
  PF_WAS_ENABLED=yes
fi
PRIOR_RULES="$(pfctl -sr 2>/dev/null)"

if [ "$PF_WAS_ENABLED" = yes ] && [ -n "$PRIOR_RULES" ] && [ "${FORCE:-0}" != 1 ]; then
  echo "REFUSING: pf is enabled and already has rules loaded:" >&2
  echo "$PRIOR_RULES" | sed 's/^/    /' >&2
  echo "Loading a ruleset would replace them. Re-run with FORCE=1 if that is safe." >&2
  exit 1
fi

restore() {
  echo
  echo "--- restoring network state ---"
  pfctl -f /etc/pf.conf >/dev/null 2>&1
  if [ "$PF_WAS_ENABLED" = no ]; then
    pfctl -d >/dev/null 2>&1
    echo "pf disabled (was disabled before)"
  else
    echo "pf left enabled (was enabled before); rules reloaded from /etc/pf.conf"
  fi
  dnctl -q flush >/dev/null 2>&1
  echo "dummynet pipes flushed"
}
trap restore EXIT INT TERM

# --- apply -------------------------------------------------------------------
echo "--- applying ${DELAY_MS}ms delay to UDP on lo0 ---"
dnctl -q flush
dnctl pipe "$PIPE" config delay "${DELAY_MS}ms"

RULES=$(mktemp /tmp/s04-pf.XXXXXX)
cat > "$RULES" <<EOF
dummynet out on lo0 proto udp from any to any pipe $PIPE
EOF
pfctl -f "$RULES" >/dev/null 2>&1 || { echo "pfctl -f failed" >&2; exit 1; }
pfctl -e >/dev/null 2>&1
rm -f "$RULES"

echo "pipe $PIPE:"
dnctl pipe show | sed 's/^/    /'
echo
echo "loopback sanity check (expect ~$((DELAY_MS * 2))ms RTT if shaping is live):"
ping -c 3 -q 127.0.0.1 2>/dev/null | tail -2 | sed 's/^/    /'
echo

# --- measure -----------------------------------------------------------------
echo "--- running S0.4 (reps=$REPS, secs=$SECS) ---"
cd "$REPO" || exit 1
# Drop privileges for cargo so target/ does not end up root-owned.
RUN_AS="${SUDO_USER:-$(whoami)}"
sudo -u "$RUN_AS" env \
  S04_REPS="$REPS" S04_SECS="$SECS" \
  cargo test --release -p agent-share --lib s04_ -- --ignored --nocapture

echo
echo "Read the 'measured median RTT' line first: if it is ~0 the shaping did"
echo "not reach the test's traffic and the numbers mean nothing."
