# wezterm

A Claude Code plugin bundling development and debugging tools for the
[WezTerm](https://github.com/wezterm/wezterm) terminal emulator.

## Skills

- **`tmux-cc-debug`** — spins up an isolated tmux + wezterm-gui test pair
  (dedicated socket, empty config, dedicated window class and `XDG_RUNTIME_DIR`) so
  experiments never disturb the real wezterm/tmux session you are running inside. It
  provides log and control-mode traffic capture plus non-intrusive state snapshots, and
  documents a timing-safe protocol for chasing races. See
  [`skills/tmux-cc-debug/SKILL.md`](skills/tmux-cc-debug/SKILL.md).
