#!/usr/bin/env bash
# Spawn an ISOLATED tmux server (dedicated socket + empty config), optionally
# pre-creating sessions. Use this when you want sessions/layouts to already
# exist so that spawn-test-wezterm.sh will ATTACH (not create) in CC mode.
#
# Usage:
#   spawn-test-tmux.sh --worktree <path> [--run-id ID] [--session NAME] [--windows N]
#
# Prints the RUN_ID; reuse it with spawn-test-wezterm.sh --run-id <ID>.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/_common.sh"

WORKTREE_ARG=""; RUN_ID=""; SESSION="test"; WINDOWS=1; TMUX_CONFIG="/dev/null"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --worktree) WORKTREE_ARG="$2"; shift 2 ;;
    --run-id)   RUN_ID="$2"; shift 2 ;;
    --session)  SESSION="$2"; shift 2 ;;
    --windows)  WINDOWS="$2"; shift 2 ;;
    --tmux-config) TMUX_CONFIG="$2"; shift 2 ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) die "unknown arg: $1" ;;
  esac
done

resolve_binaries "$WORKTREE_ARG"
[[ -n "$RUN_ID" ]] || RUN_ID="$(gen_run_id)"
RUN_DIR="$(run_dir_for "$RUN_ID")"
SOCKET="$(socket_for "$RUN_ID")"
CLASS="$(class_for "$RUN_ID")"
XDG="$(xdg_for "$RUN_ID")"
LOG_LEVEL="${LOG_LEVEL:-trace}"

mkdir -p "$RUN_DIR"
ln -sfn "$RUN_DIR" "$LATEST_LINK"
write_run_meta

# ISOLATION: dedicated socket (-L) + empty config (-f /dev/null). Never default.
info "starting isolated tmux server on socket '$SOCKET' (config: $TMUX_CONFIG)"
tmux -L "$SOCKET" -f "$TMUX_CONFIG" new-session -d -s "$SESSION"
for ((i = 1; i < WINDOWS; i++)); do
  tmux -L "$SOCKET" new-window -t "$SESSION"
done

tmux -L "$SOCKET" list-sessions
info "run id: $RUN_ID"
info "attach a test wezterm with:  spawn-test-wezterm.sh --worktree '$WORKTREE' --run-id $RUN_ID"
printf '%s\n' "$RUN_ID"
