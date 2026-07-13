---
name: wezterm-tmux-debug
description: Use when debugging or developing wezterm's tmux control-mode (tmux -CC) integration — reproducing/instrumenting layout-change, detach, teardown, or child-wake behaviour. Spawns an ISOLATED tmux+wezterm-gui test pair (dedicated socket, empty config, dedicated window class + XDG_RUNTIME_DIR) so tests never touch the real wezterm/tmux session Claude runs inside. Provides log/CC-traffic capture and state snapshots, plus a heisenbug-safe quiet mode.
---

# wezterm-tmux-debug

Helps develop and debug wezterm's **tmux control-mode (CC)** integration
(`mux/src/tmux*.rs`, `wezterm-escape-parser/src/tmux_cc/`) without disturbing the
real wezterm/tmux session you (and the user) may be running inside.

## ⛔ Isolation invariants — never break these

You are very likely running **inside** a wezterm and/or tmux. A stray test
process can resize, detach, or kill the user's real session. The scripts below
enforce isolation; **always go through them, never spawn tmux/wezterm by hand.**

1. **tmux** is always started with a **dedicated socket** (`-L weztest-<run>`) and
   an **empty config** (`-f /dev/null`). Never the default socket.
2. **wezterm-gui** is always started with `--config-file assets/minimal-config.lua`,
   a **dedicated `--class`**, and `--always-new-process`.
3. **A dedicated `XDG_RUNTIME_DIR`** per run. This is essential: `--class` only
   isolates window grouping; `wezterm cli` discovers the GUI by scanning
   `XDG_RUNTIME_DIR` for `gui-sock-*`, so without a dedicated runtime dir a
   snapshot could talk to the user's **real** wezterm. (The scripts symlink the
   Wayland compositor socket into the isolated dir so the GUI still displays.)
4. **`WEZTERM_UNIX_SOCKET` / `TMUX` must be unset** at launch. When run from
   inside a wezterm (the normal case) these leak in: `wezterm-gui start` honours
   `WEZTERM_UNIX_SOCKET` and attaches to the **real** mux even with a dedicated
   XDG + `--always-new-process`, so `cli send-text` then hits the live session;
   a set `TMUX` also makes an inner `tmux -CC` refuse to nest. `spawn-test-wezterm.sh`
   unsets `WEZTERM_UNIX_SOCKET WEZTERM_PANE WEZTERM_EXECUTABLE{,_DIR} TMUX TMUX_PANE`
   and then runs an **isolation guard** (aborts if the test GUI shares a tty with
   the real session). Never `send-text` before that guard has passed.
5. **Teardown is targeted**: kill the run's tmux socket + the recorded wezterm
   PID only. Never a broad `pkill`, never the default socket.

## Scripts (`scripts/`)

| Script | Purpose |
|---|---|
| `spawn-test-wezterm.sh` | Launch isolated wezterm-gui attached to isolated tmux in `-CC`. Auto new/attach. |
| `spawn-test-tmux.sh` | Pre-create an isolated tmux server/sessions (then attach wezterm to test the attach path). |
| `snapshot-state.sh` | Non-intrusive `wezterm cli list` + `tmux list-panes` snapshot of both sides. |
| `cc-traffic.sh` | Extract the CC exchange from `wezterm.log` into `cc-traffic.log`. |
| `cleanup.sh` | Targeted teardown of a run. |

`--worktree <path>` is **required** on spawn scripts (which `target/debug/`
binaries to use). Everything else keys off the printed `RUN_ID`; other scripts
default to the latest run.

## Debug method

**reproduce → instrument → snapshot → diff wez/tmux → cleanup**

```bash
S="${CLAUDE_PLUGIN_ROOT}/skills/wezterm-tmux-debug/scripts"
RUN=$("$S"/spawn-test-wezterm.sh --worktree /home/cedric/work/wezterm/main)
# ... trigger the behaviour in the test window (split, detach, resize, ...) ...
"$S"/snapshot-state.sh --run-id "$RUN"
"$S"/cc-traffic.sh   --run-id "$RUN"
"$S"/cleanup.sh      --run-id "$RUN"
```

Attach-path variant (pre-create sessions, then attach in CC):
```bash
RUN=$("$S"/spawn-test-tmux.sh --worktree /home/cedric/work/wezterm/main --windows 3)
"$S"/spawn-test-wezterm.sh --worktree /home/cedric/work/wezterm/main --run-id "$RUN"  # auto-attaches
```

## ⚠️ Heisenbug protocol (read before chasing a race)

`WEZTERM_LOG=mux=trace` changes timing (synchronous log I/O + formatting) and can
**neutralise a CC race** — a bug that reproduces normally vanishes under trace.

- Suspect a race / detach / teardown / wake bug → **reproduce first with
  `--log-level quiet`** (WEZTERM_LOG unset). Observe via `snapshot-state.sh`
  (CLI-based, non-intrusive), not the log.
- A traced run that "doesn't reproduce" is **not** evidence of no bug.
- Only after confirming under `quiet`, raise to `--log-level debug` then `trace`
  to capture detail — accepting the timing shift.

`--log-level` values: `trace` (default, `mux=trace,wezterm_escape_parser=trace,info`),
`debug` (`mux=debug,info`), `quiet` (unset).

## Reading the logs (`wezterm.log` / `cc-traffic.log`)

- `tmux: <event> in state <state>` — an **incoming** CC line parsed by wezterm
  (`mux/src/tmux.rs:120`).
- `sending cmd <cmd>` — a command wezterm **sends** to tmux (`mux/src/tmux.rs:293`).
- `tmux layout-change error`, `tmux configuration error`, `tmux session changed`,
  `tmux window add`, `Tmux create window id` — lifecycle/error events.

Correlate `wez-state.json` (wezterm's pane/tab view) against `tmux-state.txt`
(`#{window_layout}` etc.) to find where the two sides disagree.

## Artifacts

Per run under `~/.wezterm-tmux-test/runs/<RUN_ID>/`: `meta.env`, `wezterm.log`,
`wezterm.pid`, `cc-traffic.log`, `snapshots/`, and `wez-state.json` /
`tmux-state.txt` symlinks to the latest snapshot. `runs/latest` points at the
most recent run. Artifacts persist after cleanup for post-mortem.
