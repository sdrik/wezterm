# `tmux-subscription-changed`

{{since('nightly')}}

The `tmux-subscription-changed` event is emitted when wezterm, attached to a
tmux server in control mode (`tmux -CC`), receives a new value for one of the
tmux [format subscriptions](https://github.com/tmux/tmux/wiki/Control-Mode#format-subscriptions)
configured via
[`tmux_format_subscriptions`](../config/tmux_format_subscriptions.md). tmux
recomputes each subscribed format and *pushes* its value whenever it changes;
each such change fires this event.

The handler is called as:

```lua
wezterm.on('tmux-subscription-changed', function(window, pane, name, value, meta)
end)
```

with these arguments:

* `window` - the [`window`](../window/index.md) showing the tmux domain.
* `pane` - for a `"Pane"`-scoped subscription, the
  [`pane`](../pane/index.md) the value applies to; `nil` otherwise.
* `name` - the subscription name (the `name` from your
  `tmux_format_subscriptions` entry).
* `value` - the tmux-computed value of the format string.
* `meta` - a table with the remaining details:
    * `meta.tab` - for a `"Window"`-scoped subscription, the
      [`MuxTab`](../MuxTab/index.md) backing that tmux window; `nil` otherwise.
      (A tmux *window* maps to a wezterm *tab*.)
    * `meta.session` - the raw tmux session id, or `nil`.
    * `meta.tmux_window` - the raw tmux window id, or `nil`.
    * `meta.window_index` - the tmux window index, or `nil`.
    * `meta.tmux_pane` - the raw tmux pane id, or `nil`.
    * `meta.domain_id` - the id of the tmux [domain](../MuxDomain/index.md).

Unlike a [user var](user-var-changed.md), this event carries the value only when
it changes; it does not persist anywhere. Keep the values you care about in your
own Lua state (for example, a table keyed by window id) and read them back when
you render the status bar. The values of `#{T:status-left}` /
`#{T:status-right}` contain tmux's own style markup, which
[`wezterm.format_items_from_tmux`](../wezterm/format_items_from_tmux.md) can turn
into styled text:

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
  window:set_left_status(
    wezterm.format(wezterm.format_items_from_tmux(tmux_status[id].status_left or ''))
  )
  window:set_right_status(
    wezterm.format(wezterm.format_items_from_tmux(tmux_status[id].status_right or ''))
  )
end)

return config
```

See also [`tmux_format_subscriptions`](../config/tmux_format_subscriptions.md).
