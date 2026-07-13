#!/usr/bin/env bash
# Capture a NON-INTRUSIVE snapshot of both sides of the tmux-CC integration:
#   - wezterm's view:  `wezterm cli list --format json`  (isolated XDG)
#   - tmux's view:     `tmux -L <socket> list-panes -a -F ...`
# Safe to use in --log-level quiet (heisenbug mode): it observes via the CLI
# without touching internal timing, so it is the primary observability channel
# when trace logging is off.
#
# Usage: snapshot-state.sh [--run-id ID]     (defaults to latest run)

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/_common.sh"

RUN_ID_ARG=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --run-id) RUN_ID_ARG="$2"; shift 2 ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) die "unknown arg: $1" ;;
  esac
done

load_run_meta "$RUN_ID_ARG"
ts="$(date +%H%M%S)"
snap_dir="$RUN_DIR/snapshots"; mkdir -p "$snap_dir"
wez_out="$snap_dir/$ts.wez.json"
tmux_out="$snap_dir/$ts.tmux.txt"

# wezterm side (isolated: scrubbed env + dedicated XDG => only the test gui is
# reachable; WEZTERM_UNIX_SOCKET is stripped so we never hit the real session)
if iso_wezterm_cli "$XDG" list --format json >"$wez_out" 2>"$snap_dir/$ts.wez.err"; then
  info "wez state  -> $wez_out"
else
  info "WARN: 'wezterm cli list' failed (gui not ready / exited?) see $snap_dir/$ts.wez.err"
fi

# tmux side (dedicated socket only)
if tmux -L "$SOCKET" list-panes -a \
     -F 'sess=#{session_name} win=#{window_index}:#{window_name} pane=#{pane_id} #{pane_width}x#{pane_height} active=#{pane_active} layout=#{window_layout}' \
     >"$tmux_out" 2>/dev/null; then
  info "tmux state -> $tmux_out"
else
  info "WARN: tmux server on '$SOCKET' not reachable (detached/killed?)"
fi

# refresh stable 'latest' pointers for quick diffing
ln -sf "$wez_out"  "$RUN_DIR/wez-state.json"
ln -sf "$tmux_out" "$RUN_DIR/tmux-state.txt"
printf '%s %s\n' "$wez_out" "$tmux_out"
