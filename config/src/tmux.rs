use luahelper::impl_lua_conversion_dynamic;
use wezterm_dynamic::{FromDynamic, ToDynamic};

/// What a tmux control-mode format subscription is computed against. Determines
/// the `what` piece of the `refresh-client -B name:what:format` argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromDynamic, ToDynamic)]
pub enum TmuxSubscriptionTarget {
    /// The attached session. The notification carries no window/pane id.
    Session,
    /// All panes in the attached session (tmux `%*`); one notification per pane.
    Pane,
    /// All windows in the attached session (tmux `@*`); one notification per window.
    Window,
}

impl Default for TmuxSubscriptionTarget {
    fn default() -> Self {
        Self::Session
    }
}

impl TmuxSubscriptionTarget {
    /// The `what` token used in `refresh-client -B name:what:format`.
    pub fn tmux_type(&self) -> &'static str {
        match self {
            Self::Session => "",
            Self::Pane => "%*",
            Self::Window => "@*",
        }
    }
}

/// A tmux control-mode "format subscription".
///
/// When attached to a `tmux -CC` session, wezterm subscribes to each entry via
/// `refresh-client -B`; tmux then pushes the recomputed value whenever it
/// changes. The latest value is stored as a user var named `name` on the
/// relevant pane(s), where it can be read from Lua (e.g. in an `update-status`
/// handler via `pane:get_user_vars()`).
#[derive(Debug, Clone, PartialEq, Eq, FromDynamic, ToDynamic)]
pub struct TmuxFormatSubscription {
    /// The user var name under which the computed value is exposed; also used
    /// as the tmux subscription name. Should be made of `[A-Za-z0-9_-]`.
    pub name: String,
    /// What the format is computed against (session/pane/window).
    #[dynamic(default)]
    pub target: TmuxSubscriptionTarget,
    /// The tmux format string, e.g. `#{pane_current_path}` or `#{T:status-left}`.
    pub format: String,
}
impl_lua_conversion_dynamic!(TmuxFormatSubscription);
