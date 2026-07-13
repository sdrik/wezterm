#!/usr/bin/env bash
# Extract the tmux control-mode (CC) exchange from wezterm.log into a readable
# cc-traffic.log. Source lines come from mux/src/tmux.rs:
#   - incoming CC events:  "tmux: <event> in state <state>"   (log::debug, ~l.120)
#   - outgoing commands:   "sending cmd <cmd>"                 (log::debug, ~l.293)
#   - lifecycle:           "tmux session changed", "tmux window add", "Tmux create window id"
# Requires the run to have used --log-level debug or trace.
#
# Usage: cc-traffic.sh [--run-id ID] [--follow]

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/_common.sh"

RUN_ID_ARG=""; FOLLOW=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --run-id) RUN_ID_ARG="$2"; shift 2 ;;
    --follow) FOLLOW=1; shift ;;
    -h|--help) sed -n '2,11p' "$0"; exit 0 ;;
    *) die "unknown arg: $1" ;;
  esac
done

load_run_meta "$RUN_ID_ARG"
log="$RUN_DIR/wezterm.log"
out="$RUN_DIR/cc-traffic.log"
[[ -f "$log" ]] || die "no wezterm.log in $RUN_DIR"

if [[ "${LOG_LEVEL:-trace}" == "quiet" ]]; then
  info "WARN: this run used --log-level quiet; wezterm.log has no CC traffic."
  info "      Use snapshot-state.sh instead, or re-run with --log-level debug/trace."
fi

PAT='tmux: |sending cmd |tmux session changed|tmux window add|Tmux create window id|tmux layout-change|tmux configuration error'
if [[ -n "$FOLLOW" ]]; then
  info "following CC traffic (Ctrl-C to stop)"
  grep --line-buffered -E "$PAT" <(tail -n +1 -f "$log") | tee "$out"
else
  grep -E "$PAT" "$log" >"$out" || true
  info "cc traffic -> $out ($(wc -l <"$out") lines)"
  cat "$out"
fi
