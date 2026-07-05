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

Whenever a subscription's value changes, tmux pushes the recomputed value to
wezterm, so you can read it from Lua and render it in the status bar.

Each entry has three fields:

* `name` - the subscription name. Use characters from `[A-Za-z0-9_-]`.
* `target` - what the format is computed against:
    * `"Session"` (default) - the attached session, e.g. `#{T:status-left}`.
    * `"Pane"` - every pane in the session.
    * `"Window"` - every window in the session.
* `format` - the tmux format string, e.g. `#{pane_current_path}`.

This option is empty by default; nothing is subscribed until you configure it.

If the attached tmux is older than 3.2 (no `refresh-client -B`), subscriptions
are silently skipped; everything else continues to work.
