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

Whenever a subscription's value changes, wezterm emits the
[tmux-subscription-changed](../window-events/tmux-subscription-changed.md) event,
carrying the subscription `name`, its new `value`, and the relevant wezterm
objects (pane/tab/window). Handle it from Lua to render the value in the wezterm
status bar.

Each entry has three fields:

* `name` - the subscription name; it is passed to your event handler to identify
  which value changed. Use characters from `[A-Za-z0-9_-]`.
* `target` - what the format is computed against:
    * `"Session"` (default) - the attached session, e.g. `#{T:status-left}`.
      The event fires with `pane = nil`.
    * `"Pane"` - every pane in the session; the event fires with the
      corresponding wezterm `pane`.
    * `"Window"` - every window in the session; the event fires with the
      corresponding wezterm tab in `meta.tab`.
* `format` - the tmux format string, e.g. `#{pane_current_path}`.

This option is empty by default; nothing is subscribed until you configure it.
For example, to surface the tmux status line halves and a couple of per-pane
formats in the wezterm status bar:

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

Handle the
[tmux-subscription-changed](../window-events/tmux-subscription-changed.md) event
and keep the latest values in your own Lua state:

```lua
local wezterm = require 'wezterm'
local config = wezterm.config_builder()

config.tmux_format_subscriptions = {
  { name = 'status_left', target = 'Session', format = '#{T:status-left}' },
  { name = 'status_right', target = 'Session', format = '#{T:status-right}' },
}

-- Latest subscription values, keyed by wezterm window id.
local tmux_status = {}

wezterm.on('tmux-subscription-changed', function(window, pane, name, value, meta)
  local id = window:window_id()
  tmux_status[id] = tmux_status[id] or {}
  tmux_status[id][name] = value
  window:set_left_status(tmux_status[id].status_left or '')
  window:set_right_status(tmux_status[id].status_right or '')
end)

return config
```

!!! note
    The value of `#{T:status-left}` / `#{T:status-right}` contains tmux's own
    style markup (e.g. `#[fg=green]`) literally; wezterm does not interpret it.
    Strip or translate it on the Lua side if you don't want it shown verbatim.
