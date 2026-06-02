#!/usr/bin/env bash
# Trigger the cosmic-claude-attention-card daemon by spooling a synthetic hook
# event (the daemon is hook-driven now, not registry-driven). Pass the kind:
#
#   ./test-card.sh stop         # "Claude finished"
#   ./test-card.sh error        # "Claude hit an error"
#   ./test-card.sh permission   # "Claude needs permission · Bash"  (default)
#   ./test-card.sh idle         # "Claude is waiting for you"
#
# Uses a fake session id, so the card shows the cwd's basename as the project
# and clicking it won't focus a real window. For a true end-to-end test, just
# start a *new* Claude session — the settings.json hooks fire on its Stop /
# permission / idle events automatically.
set -u

kind="${1:-permission}"
case "$kind" in
  stop | error | permission | idle) ;;
  *)
    echo "usage: $0 [stop|error|permission|idle]"
    exit 2
    ;;
esac

# Unique session id per run so repeated invocations stack as separate cards
# (one card per session id; same id would just update in place).
sid="test-$(date +%s%N)"
payload='{"session_id":"'"$sid"'","cwd":"'"$HOME"'/lab/cosmic-apps","tool_name":"Bash"}'
printf '%s' "$payload" | bash "$HOME/.claude/card-hook.sh" "$kind"

echo "Spooled a '$kind' event ($sid) — watch the top for the card (within ~1s)."
echo "Run it a few times with different kinds to see them stack to the right:"
echo "  $0 stop ; $0 error ; $0 permission ; $0 idle"
