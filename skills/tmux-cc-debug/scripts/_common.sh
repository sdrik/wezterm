#!/usr/bin/env bash
# Shared helpers + ISOLATION INVARIANTS for the tmux-cc-debug skill.
# Sourced by every other script. Never run directly.
#
# Invariants enforced here (non-negotiable):
#   - tmux test server ALWAYS uses a dedicated socket (-L) + empty config (-f /dev/null)
#   - wezterm test GUI ALWAYS uses --config-file <minimal>, --class <dedicated>,
#     and a dedicated XDG_RUNTIME_DIR so `wezterm cli` can never touch the real session
#   - teardown is ALWAYS targeted (socket + recorded PID), never global

set -euo pipefail

# --- locations -------------------------------------------------------------
COMMON_SH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(cd "$COMMON_SH_DIR/.." && pwd)"
MINIMAL_CONFIG="$SKILL_DIR/assets/minimal-config.lua"

TEST_ROOT="${WEZTERM_TMUX_TEST_ROOT:-$HOME/.wezterm-tmux-test}"
RUNS_DIR="$TEST_ROOT/runs"
LATEST_LINK="$RUNS_DIR/latest"

# --- output helpers --------------------------------------------------------
die()  { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }
info() { printf '\033[36m[wtd]\033[0m %s\n' "$*" >&2; }

# --- run identity ----------------------------------------------------------
# A run id is stable and everything else (socket, class, dirs) derives from it.
gen_run_id() {
  printf '%s-%s-%s' "$(date +%Y%m%d-%H%M%S)" "$$" "${RANDOM}"
}

run_dir_for()  { printf '%s/%s' "$RUNS_DIR" "$1"; }
socket_for()   { printf 'weztest-%s' "$1"; }
class_for()    { printf 'WeztermTmuxTest-%s' "$1"; }
xdg_for()      { printf '%s/%s/xdg' "$RUNS_DIR" "$1"; }

# WEZTERM_LOG value for a given --log-level. Empty string => leave unset (quiet).
wezterm_log_for_level() {
  case "$1" in
    trace) printf 'mux=trace,wezterm_escape_parser=trace,info' ;;
    debug) printf 'mux=debug,info' ;;
    quiet) printf '' ;;
    *) die "unknown --log-level '$1' (use: trace | debug | quiet)" ;;
  esac
}

# --- worktree / binary resolution -----------------------------------------
# Requires an explicit --worktree path (per design). Validates debug binaries.
resolve_binaries() {  # $1 = worktree path
  local wt="${1:-}"
  [[ -n "$wt" ]]      || die "--worktree <path> is required (e.g. /home/cedric/work/wezterm/main)"
  [[ -d "$wt" ]]      || die "worktree not found: $wt"
  wt="$(cd "$wt" && pwd)"
  BIN_GUI="$wt/target/debug/wezterm-gui"
  BIN_WEZTERM="$wt/target/debug/wezterm"
  [[ -x "$BIN_GUI" ]]     || die "missing binary: $BIN_GUI (build it: cargo build -p wezterm-gui)"
  [[ -x "$BIN_WEZTERM" ]] || die "missing binary: $BIN_WEZTERM (build it: cargo build -p wezterm)"
  WORKTREE="$wt"
}

# --- run directory + metadata ---------------------------------------------
# Persist everything downstream scripts need so RUN_ID is the only handle.
write_run_meta() {  # writes $RUN_DIR/meta.env from current vars
  cat >"$RUN_DIR/meta.env" <<EOF
# tmux-cc-debug run metadata
RUN_ID='$RUN_ID'
WORKTREE='$WORKTREE'
BIN_GUI='$BIN_GUI'
BIN_WEZTERM='$BIN_WEZTERM'
SOCKET='$SOCKET'
CLASS='$CLASS'
XDG='$XDG'
TMUX_CONFIG='${TMUX_CONFIG:-/dev/null}'
LOG_LEVEL='${LOG_LEVEL:-trace}'
EOF
}

load_run_meta() {  # $1 = run id (or empty => latest)
  local id="${1:-}"
  if [[ -z "$id" ]]; then
    [[ -L "$LATEST_LINK" ]] || die "no run id given and no 'latest' run found under $RUNS_DIR"
    RUN_DIR="$(readlink -f "$LATEST_LINK")"
  else
    RUN_DIR="$(run_dir_for "$id")"
  fi
  [[ -f "$RUN_DIR/meta.env" ]] || die "no meta.env in $RUN_DIR (not a valid run?)"
  # shellcheck disable=SC1091
  source "$RUN_DIR/meta.env"
  RUN_DIR="$(run_dir_for "$RUN_ID")"
}

# Run `wezterm cli` against the ISOLATED test GUI. Critical: `wezterm cli` resolves
# the mux via WEZTERM_UNIX_SOCKET (from the environment), falling back to a
# mux-server at $XDG/wezterm/sock if it is unset — it does NOT auto-discover a
# GUI's gui-sock from XDG. So when this skill runs from inside a real wezterm,
# merely setting XDG is not enough (the inherited WEZTERM_UNIX_SOCKET would make
# every cli call hit the LIVE session) and merely unsetting it is not enough
# either (cli then tries to spawn a mux-server). We must point WEZTERM_UNIX_SOCKET
# straight at the test GUI's own gui-sock-<pid>. Always use this helper.
iso_wezterm_cli() {  # $1 = isolated XDG dir; remaining args = cli args
  local xdg="$1"; shift
  local sock="$xdg/wezterm/gui-sock-$(cat "$RUN_DIR/wezterm.pid" 2>/dev/null)"
  env -u WEZTERM_PANE WEZTERM_UNIX_SOCKET="$sock" XDG_RUNTIME_DIR="$xdg" "$BIN_WEZTERM" cli "$@"
}

# Isolate XDG_RUNTIME_DIR without breaking the GUI display connection.
# The Wayland compositor socket lives INSIDE the real runtime dir, so symlink
# it into the isolated dir. X11 uses /tmp/.X11-unix and needs no help.
setup_isolated_xdg() {  # uses $XDG
  mkdir -p "$XDG"
  chmod 700 "$XDG"
  local real="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
  if [[ -n "${WAYLAND_DISPLAY:-}" && -e "$real/$WAYLAND_DISPLAY" ]]; then
    ln -sf "$real/$WAYLAND_DISPLAY" "$XDG/$WAYLAND_DISPLAY"
    [[ -e "$real/$WAYLAND_DISPLAY.lock" ]] && ln -sf "$real/$WAYLAND_DISPLAY.lock" "$XDG/$WAYLAND_DISPLAY.lock" || true
  fi
}
