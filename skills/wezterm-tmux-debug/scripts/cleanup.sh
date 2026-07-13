#!/usr/bin/env bash
# TARGETED teardown of a test run. Kills ONLY:
#   - the tmux server on the run's dedicated socket
#   - the wezterm-gui process whose PID we recorded at spawn time
# Never uses a broad pkill, never touches the default tmux socket or the user's
# real wezterm. Artifacts under the run dir are kept for post-mortem analysis.
#
# Usage: cleanup.sh [--run-id ID]     (defaults to latest run)

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/_common.sh"

RUN_ID_ARG=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --run-id) RUN_ID_ARG="$2"; shift 2 ;;
    -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
    *) die "unknown arg: $1" ;;
  esac
done

load_run_meta "$RUN_ID_ARG"
info "tearing down run $RUN_ID (socket=$SOCKET class=$CLASS)"

# 1) kill the dedicated tmux server (safe: only this -L socket)
if tmux -L "$SOCKET" kill-server 2>/dev/null; then
  info "tmux server on '$SOCKET' killed"
else
  info "tmux server on '$SOCKET' already gone"
fi
# remove any lingering (dead) socket file for this dedicated socket only
rm -f "/tmp/tmux-$(id -u)/$SOCKET" 2>/dev/null || true

# 2) kill the recorded wezterm-gui pid (never a class-wide pkill)
pidfile="$RUN_DIR/wezterm.pid"
if [[ -f "$pidfile" ]]; then
  pid="$(cat "$pidfile")"
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    sleep 1
    kill -9 "$pid" 2>/dev/null || true
    info "wezterm-gui pid $pid terminated"
  else
    info "wezterm-gui pid ${pid:-?} already gone"
  fi
else
  info "no wezterm.pid recorded (nothing to kill on the wezterm side)"
fi

info "artifacts kept in: $RUN_DIR"
