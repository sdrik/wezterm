-- vim: ts=2:sw=2:sts=2:et
-- Minimal wezterm config for ISOLATED tmux-CC integration testing.
-- Based on /home/cedric/work/wezterm/main/target/debug/test-layout-changed.lua
--
-- Isolation note: the test instance's mux socket is isolated via a dedicated
-- XDG_RUNTIME_DIR (set by spawn-test-wezterm.sh), NOT via a custom unix domain
-- here. Keep this config minimal on purpose so test behaviour is not perturbed
-- by unrelated settings.

local wezterm = require 'wezterm';
local mux = wezterm.mux

local config = {}
if wezterm.config_builder then
  config = wezterm.config_builder()
end

local act = wezterm.action

config.window_decorations = 'INTEGRATED_BUTTONS | RESIZE'

-- Leader key = équivalent de "prefix C-a"
config.leader = { key = 'a', mods = 'CTRL', timeout_milliseconds = 1000 }

config.keys = {
  -- Navigation tabs (Alt+Left/Right, sans prefix)
  { key = 'LeftArrow',  mods = 'ALT', action = act.ActivateTabRelative(-1) },
  { key = 'RightArrow', mods = 'ALT', action = act.ActivateTabRelative(1) },

  -- Navigation panes (Ctrl+Arrow, sans prefix)
  { key = 'LeftArrow',  mods = 'CTRL', action = act.ActivatePaneDirection 'Left' },
  { key = 'RightArrow', mods = 'CTRL', action = act.ActivatePaneDirection 'Right' },
  { key = 'UpArrow',    mods = 'CTRL', action = act.ActivatePaneDirection 'Up' },
  { key = 'DownArrow',  mods = 'CTRL', action = act.ActivatePaneDirection 'Down' },

  -- Toggle zoom pane
  { key = 'Home', mods = 'CTRL', action = act.TogglePaneZoomState },

  -- === Avec LEADER (équivalent prefix C-a) ===
  { key = 'c', mods = 'LEADER', action = act.SpawnTab 'CurrentPaneDomain' },
  { key = '|', mods = 'LEADER', action = act.SplitPane { direction = 'Right', size = { Percent = 50 } } },
  { key = '-', mods = 'LEADER', action = act.SplitPane { direction = 'Down', size = { Percent = 50 } } },
}

return config
