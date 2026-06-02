#!/usr/bin/env bash
# Trigger the cosmic-applet-claude-sessions attention card by injecting a
# synthetic session that goes idle -> waiting (the transition that fires the
# card). Run again to re-trigger. Run with "clean" to remove the fixture.
#
#   ./test-card.sh          # trigger the card
#   ./test-card.sh clean    # remove the synthetic session + process
#
# Applet debug output (while testing) lands in /tmp/claude-card.log
set -u

INFO="/tmp/claude-card-test.info"
SESSDIR="$HOME/.claude/sessions"

remove_fixture() {
  if [ -f "$INFO" ]; then
    read -r pid sess < "$INFO" 2>/dev/null || true
    [ -n "${sess:-}" ] && rm -f "$sess"
    [ -n "${pid:-}" ] && kill "$pid" 2>/dev/null
    rm -f "$INFO"
  fi
}

if [ "${1:-}" = clean ]; then
  remove_fixture
  echo "cleaned up the test session."
  exit 0
fi

remove_fixture

# A real, alive pid so the applet's liveness check passes.
nohup tail -f /dev/null >/dev/null 2>&1 &
fakepid=$!
sess="$SESSDIR/$fakepid.json"
now=$(($(date +%s) * 1000))
base="\"pid\":$fakepid,\"sessionId\":\"test-card\",\"cwd\":\"$HOME/lab/cosmic-apps\",\"kind\":\"interactive\",\"entrypoint\":\"cli\",\"name\":\"Test attention card\",\"startedAt\":$now,\"updatedAt\":$now"

printf '{%s,"status":"idle"}\n' "$base" > "$sess"
printf '%s %s\n' "$fakepid" "$sess" > "$INFO"

# Flip to waiting after a few seconds so the applet records 'idle' first and
# then sees the idle -> waiting transition that fires the card.
nohup bash -c "sleep 4; printf '{%s,\"status\":\"waiting\"}\n' '$base' > '$sess'" >/dev/null 2>&1 &

echo "Injected an idle session (pid $fakepid)."
echo "Flips to 'waiting' in ~4s — watch the TOP-RIGHT of the screen for the card."
echo "Re-run to trigger again, or '$0 clean' to remove it. Log: /tmp/claude-card.log"
