---
tags:
  - multiplexing
  - tmux
  - status
---
# `tmux_format_subscriptions`

{{since('nightly')}}

When wezterm is attached to a tmux server in control mode (`tmux -CC`), tmux
draws no status bar of its own. This option lets wezterm subscribe to tmux
[format strings](https://github.com/tmux/tmux/wiki/Formats) using tmux's
[format subscriptions](https://github.com/tmux/tmux/wiki/Control-Mode#format-subscriptions)
mechanism (`refresh-client -B`, requires **tmux >= 3.2**). tmux then *pushes*
the recomputed value of each format whenever it changes.

Each subscription's latest value is stored as a [pane user
var](../../../recipes/passing-data-to-wezterm.md) named by `name`, so you can
read it from Lua — typically in an
[update-status](../window-events/update-status.md) handler — and render it in
the wezterm status bar.

Each entry has three fields:

* `name` - the user var name under which the value is exposed (also the tmux
  subscription name). Use characters from `[A-Za-z0-9_-]`.
* `target` - what the format is computed against:
    * `"Session"` (default) - the attached session, e.g. `#{T:status-left}`.
    * `"Pane"` - every pane in the session; the value is set on each
      corresponding wezterm pane.
    * `"Window"` - every window in the session.
* `format` - the tmux format string, e.g. `#{pane_current_path}`.

The default value subscribes to the tmux status line halves and a couple of
per-pane formats:

```lua
config.tmux_format_subscriptions = {
  { name = 'tmux_status_left', target = 'Session', format = '#{T:status-left}' },
  { name = 'tmux_status_right', target = 'Session', format = '#{T:status-right}' },
  { name = 'tmux_pane_current_path', target = 'Pane', format = '#{pane_current_path}' },
  { name = 'tmux_pane_title', target = 'Pane', format = '#{pane_title}' },
}
```

If the attached tmux is older than 3.2 (no `refresh-client -B`), subscriptions
are silently skipped; everything else continues to work.

## Example: show tmux's status-left in the wezterm status bar

The value of `#{T:status-left}` / `#{T:status-right}` contains tmux's own style
markup (e.g. `#[fg=green]`). Use
[wezterm.format_items_from_tmux](../wezterm/format_items_from_tmux.md) to render
it as styled text:

```lua
local wezterm = require 'wezterm'
local config = wezterm.config_builder()

wezterm.on('update-status', function(window, pane)
  local vars = pane:get_user_vars()
  window:set_left_status(
    wezterm.format(wezterm.format_items_from_tmux(vars.tmux_status_left or ''))
  )
  window:set_right_status(
    wezterm.format(wezterm.format_items_from_tmux(vars.tmux_status_right or ''))
  )
end)

return config
```

To show the raw value instead (tags included), pass the user var straight to
`window:set_left_status(vars.tmux_status_left or '')`.
