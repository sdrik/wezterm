---
title: wezterm.format_items_from_tmux
tags:
 - utility
 - string
 - tmux
---

# `wezterm.format_items_from_tmux(s)`

{{since('nightly')}}

Parses a tmux *style string* — text containing tmux `#[...]` style directives,
as used in `status-left`/`status-right` and in the value of a
[`#{T:...}` format subscription](../config/tmux_format_subscriptions.md) — and
returns a list of `FormatItem`s, the same representation that
[`wezterm.format`](format.md) accepts.

This is intended for rendering tmux-computed, styled content in the wezterm
status bar when attached to a `tmux -CC` session. Pass the result straight to
`wezterm.format`:

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

Because it returns plain `FormatItem`s, you can also combine the result with
your own items before formatting:

```lua
local items = wezterm.format_items_from_tmux(vars.tmux_status_left or '')
table.insert(items, 1, { Text = ' ' })
table.insert(items, { Text = ' ' })
window:set_left_status(wezterm.format(items))
```

## Supported directives

- Colors: `fg=` / `bg=` with named colors (`red`, `brightblue`, …),
  `colour0`–`colour255`, `#rrggbb`, and `default`. Colors `0`–`15` and ANSI
  names map to your theme's palette; `16`–`255` map to the standard xterm-256
  colors.
- Attributes: `bold`/`bright`, `dim`, `italics`, `underscore` (and
  `double-`/`curly-`/`dotted-`/`dashed-underscore`), `blink`, `reverse`,
  `hidden`, `strikethrough`, and their `no…` negations.
- Style stack: `default`, `none`, `push-default`, `pop-default`, `set-default`.

Layout/interactivity directives that have no inline equivalent (`align=`,
`fill=`, `width=`, `pad=`, `list=`, `range=`, `us=`, `acs`, `overline`, …) are
ignored. The parser never errors, so it is safe to feed arbitrary tmux output.
