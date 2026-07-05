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

If the attached tmux is older than 3.2 (no `refresh-client -B`), subscriptions
are silently skipped; everything else continues to work.
