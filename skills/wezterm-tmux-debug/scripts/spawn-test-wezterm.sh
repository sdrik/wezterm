#!/usr/bin/env bash
# Spawn an ISOLATED wezterm-gui attached to an isolated tmux server in CONTROL
# MODE (-CC). Auto-detects whether to create a new session or attach to an
# existing one (e.g. one created by spawn-test-tmux.sh).
#
# Usage:
#   spawn-test-wezterm.sh --worktree <path> [--run-id ID] [--log-level trace|debug|quiet]
#                         [--session NAME] [--attach [TARGET] | --new]
#
# Isolation guarantees (enforced):
#   - --config-file <minimal>  --class <dedicated>  + dedicated XDG_RUNTIME_DIR
#   - tmux launched with -L <dedicated socket> -f /dev/null (on create)
#
# HEISENBUG: if you suspect a race/detach/teardown bug, use --log-level quiet
# first (WEZTERM_LOG unset) so trace logging does not change the timing and
# mask the bug. Observe via snapshot-state.sh, not the log, in that mode.

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/_common.sh"

WORKTREE_ARG=""; RUN_ID=""; LOG_LEVEL="trace"; SESSION=""
TMUX_CONFIG="/dev/null"; FORCE_NEW=""; FORCE_ATTACH=""; ATTACH_TARGET=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --worktree)  WORKTREE_ARG="$2"; shift 2 ;;
    --run-id)    RUN_ID="$2"; shift 2 ;;
    --log-level) LOG_LEVEL="$2"; shift 2 ;;
    --session)   SESSION="$2"; shift 2 ;;
    --tmux-config) TMUX_CONFIG="$2"; shift 2 ;;
    --new)       FORCE_NEW=1; shift ;;
    --attach)    FORCE_ATTACH=1
                 # optional non-flag target
                 if [[ ${2:-} && ${2:0:1} != "-" ]]; then ATTACH_TARGET="$2"; shift 2; else shift; fi ;;
    -h|--help)   sed -n '2,17p' "$0"; exit 0 ;;
    *) die "unknown arg: $1" ;;
  esac
done

resolve_binaries "$WORKTREE_ARG"
[[ -n "$RUN_ID" ]] || RUN_ID="$(gen_run_id)"
RUN_DIR="$(run_dir_for "$RUN_ID")"
SOCKET="$(socket_for "$RUN_ID")"
CLASS="$(class_for "$RUN_ID")"
XDG="$(xdg_for "$RUN_ID")"

mkdir -p "$RUN_DIR"
ln -sfn "$RUN_DIR" "$LATEST_LINK"
write_run_meta
setup_isolated_xdg

# --- decide new vs attach --------------------------------------------------
mode="new"
if [[ -n "$FORCE_NEW" ]]; then
  mode="new"
elif [[ -n "$FORCE_ATTACH" ]]; then
  mode="attach"
elif tmux -L "$SOCKET" has-session 2>/dev/null; then
  mode="attach"   # a server (e.g. from spawn-test-tmux.sh) is already live
fi

# ISOLATION: on create, dedicated socket + empty config. On attach, the server
# already exists so -f is intentionally omitted (tmux would ignore it anyway).
tmux_cmd=(tmux -L "$SOCKET")
if [[ "$mode" == "new" ]]; then
  tmux_cmd+=(-f "$TMUX_CONFIG" -CC new-session)
  [[ -n "$SESSION" ]] && tmux_cmd+=(-s "$SESSION")
else
  tmux_cmd+=(-CC attach-session)
  local_target="${ATTACH_TARGET:-$SESSION}"
  [[ -n "$local_target" ]] && tmux_cmd+=(-t "$local_target")
fi

WLOG="$(wezterm_log_for_level "$LOG_LEVEL")"
info "run id:     $RUN_ID"
info "worktree:   $WORKTREE"
info "class:      $CLASS"
info "socket:     $SOCKET   (mode: $mode)"
info "log level:  $LOG_LEVEL   WEZTERM_LOG='${WLOG:-<unset>}'"
info "artifacts:  $RUN_DIR"
info "tmux cmd:   ${tmux_cmd[*]}"

# Launch the GUI detached so it survives this shell. stderr -> wezterm.log.
# XDG_RUNTIME_DIR is dedicated => `wezterm cli` (same XDG) can only see THIS gui.
launch() {
  export XDG_RUNTIME_DIR="$XDG"
  # CRITICAL isolation invariant: when this skill is run from INSIDE a wezterm
  # (the normal case), the environment carries WEZTERM_UNIX_SOCKET / WEZTERM_PANE.
  # `wezterm-gui start` honours WEZTERM_UNIX_SOCKET and would attach to the REAL
  # mux — defeating the dedicated XDG, so `wezterm cli --pane-id N` (same XDG)
  # would then read/write the user's LIVE session. Unset them so the test GUI
  # spins up its own mux. Also drop TMUX/TMUX_PANE so an inner `tmux -CC` does
  # not refuse to nest.
  unset WEZTERM_UNIX_SOCKET WEZTERM_PANE WEZTERM_EXECUTABLE WEZTERM_EXECUTABLE_DIR
  unset TMUX TMUX_PANE
  if [[ -n "$WLOG" ]]; then export WEZTERM_LOG="$WLOG"; else unset WEZTERM_LOG; fi
  exec "$BIN_GUI" --config-file "$MINIMAL_CONFIG" \
    start --class "$CLASS" --always-new-process \
    -- "${tmux_cmd[@]}"
}
launch >"$RUN_DIR/wezterm.stdout" 2>"$RUN_DIR/wezterm.log" &
echo $! >"$RUN_DIR/wezterm.pid"
disown || true

# --- ISOLATION GUARD -------------------------------------------------------
# Defence-in-depth against the classic leak where the test GUI attaches to the
# REAL mux (e.g. a WEZTERM_UNIX_SOCKET that slipped through): if the test GUI
# reports a tty that also belongs to the user's live session, abort and tear the
# run down NOW, before anyone runs `cli send-text` against the real session.
iso_ttys_of() { iso_wezterm_cli "$1" list --format json 2>/dev/null \
              | grep -o '"tty_name": *"[^"]*"' || true; }
# Real-session ttys: query the REAL gui via the ambient (inherited) socket.
real_ttys_of() { env -u WEZTERM_PANE "$BIN_WEZTERM" cli list --format json 2>/dev/null \
              | grep -o '"tty_name": *"[^"]*"' || true; }
if [[ -n "${WEZTERM_UNIX_SOCKET:-}" ]]; then
  iso_ttys=""
  for _ in $(seq 1 40); do iso_ttys="$(iso_ttys_of "$XDG")"; [[ -n "$iso_ttys" ]] && break; sleep 0.25; done
  real_ttys="$(real_ttys_of)"
  while IFS= read -r t; do
    [[ -n "$t" ]] || continue
    if grep -Fqx "$t" <<<"$real_ttys"; then
      kill "$(cat "$RUN_DIR/wezterm.pid")" 2>/dev/null || true
      die "ISOLATION BREACH: test GUI shares tty ($t) with the real session — killed run $RUN_ID (leaked WEZTERM_UNIX_SOCKET?). Do NOT send-text."
    fi
  done <<<"$iso_ttys"
  info "isolation ok: test GUI ttys are disjoint from the real session"
fi

info "wezterm-gui pid: $(cat "$RUN_DIR/wezterm.pid")"
info "snapshot:  snapshot-state.sh --run-id $RUN_ID"
info "cleanup:   cleanup.sh --run-id $RUN_ID"
printf '%s\n' "$RUN_ID"
