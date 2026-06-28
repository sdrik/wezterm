use crate::error::Context;
use crate::{Result, bail, format_err};
use parser::Rule;
use pest::Parser as _;
use pest::iterators::{Pair, Pairs};

pub type TmuxWindowId = u64;
pub type TmuxPaneId = u64;
pub type TmuxSessionId = u64;

pub mod parser {
    use pest_derive::Parser;
    #[derive(Parser)]
    #[grammar = "tmux_cc/tmux.pest"]
    pub struct TmuxParser;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guarded {
    pub error: bool,
    pub timestamp: i64,
    pub number: u64,
    pub flags: i64,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    // Tmux generic events
    Begin {
        timestamp: i64,
        number: u64,
        flags: i64,
    },
    End {
        timestamp: i64,
        number: u64,
        flags: i64,
    },
    Error {
        timestamp: i64,
        number: u64,
        flags: i64,
    },
    Guarded(Guarded),

    // Tmux specific events
    ClientDetached {
        client_name: String,
    },
    ClientSessionChanged {
        client_name: String,
        session: TmuxSessionId,
        session_name: String,
    },
    ConfigError {
        error: String,
    },
    Continue {
        pane: TmuxPaneId,
    },
    ExtendedOutput {
        pane: TmuxPaneId,
        text: Vec<u8>,
    },
    Exit {
        reason: Option<String>,
    },
    LayoutChange {
        window: TmuxWindowId,
        layout: String,
        visible_layout: Option<String>,
        raw_flags: Option<String>,
    },
    Message {
        message: String,
    },
    Output {
        pane: TmuxPaneId,
        text: Vec<u8>,
    },
    PaneModeChanged {
        pane: TmuxPaneId,
    },
    PasteBufferChanged {
        buffer: String,
    },
    PasteBufferDeleted {
        buffer: String,
    },
    Pause {
        pane: TmuxPaneId,
    },
    SessionChanged {
        session: TmuxSessionId,
        name: String,
    },
    SessionRenamed {
        name: String,
    },
    SessionsChanged,
    SessionWindowChanged {
        session: TmuxSessionId,
        window: TmuxWindowId,
    },
    /// A `%subscription-changed` notification carrying the latest value of a
    /// format string we subscribed to via `refresh-client -B`.
    ///
    /// The wire format is:
    /// `%subscription-changed <name> <session-id> <window-id|-> <window-index|-> <pane-id|-> : <value>`
    /// where the id/index fields are `-` when not applicable to the
    /// subscription's target (e.g. a session-scoped subscription has no
    /// window/pane). The id fields are parsed best-effort so that a malformed
    /// or future-extended notification never fails the whole parse.
    SubscriptionChanged {
        name: String,
        session: Option<TmuxSessionId>,
        window: Option<TmuxWindowId>,
        window_index: Option<u64>,
        pane: Option<TmuxPaneId>,
        value: String,
    },
    UnlinkedWindowAdd {
        window: TmuxWindowId,
    },
    UnlinkedWindowClose {
        window: TmuxWindowId,
    },
    UnlinkedWindowRenamed {
        window: TmuxWindowId,
    },
    WindowAdd {
        window: TmuxWindowId,
    },
    WindowClose {
        window: TmuxWindowId,
    },
    WindowPaneChanged {
        window: TmuxWindowId,
        pane: TmuxPaneId,
    },
    WindowRenamed {
        window: TmuxWindowId,
        name: String,
    },
}

/// The geometry of a tmux layout cell: its size and position within the
/// window, in terminal cells. Matches the `WxH,X,Y` portion of a tmux
/// layout string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TmuxLayoutCell {
    pub width: u64,
    pub height: u64,
    pub left: u64,
    pub top: u64,
}

/// The direction along which a tmux split arranges its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmuxSplitDirection {
    /// `{...}` in the layout string: children are laid out left-to-right.
    Horizontal,
    /// `[...]` in the layout string: children are laid out top-to-bottom.
    Vertical,
}

/// A faithful, n-ary mirror of tmux's window layout tree, as described by a
/// tmux layout string (see [`parse_layout_tree`]). A `Split` may have any
/// number of children (tmux produces 3+ for layouts such as
/// `even-horizontal`), so this is intentionally n-ary rather than binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmuxLayoutNode {
    Leaf {
        cell: TmuxLayoutCell,
        pane_id: TmuxPaneId,
    },
    Split {
        cell: TmuxLayoutCell,
        direction: TmuxSplitDirection,
        children: Vec<TmuxLayoutNode>,
    },
}

impl TmuxLayoutNode {
    /// The geometry of this node (its bounding cell), regardless of variant.
    pub fn cell(&self) -> TmuxLayoutCell {
        match self {
            TmuxLayoutNode::Leaf { cell, .. } => *cell,
            TmuxLayoutNode::Split { cell, .. } => *cell,
        }
    }

    /// All pane ids contained in this subtree, in layout order
    /// (left-to-right within `{}`, top-to-bottom within `[]`).
    pub fn pane_ids(&self) -> Vec<TmuxPaneId> {
        let mut out = Vec::new();
        self.collect_pane_ids(&mut out);
        out
    }

    fn collect_pane_ids(&self, out: &mut Vec<TmuxPaneId>) {
        match self {
            TmuxLayoutNode::Leaf { pane_id, .. } => out.push(*pane_id),
            TmuxLayoutNode::Split { children, .. } => {
                for child in children {
                    child.collect_pane_ids(out);
                }
            }
        }
    }

    /// All leaves `(pane_id, cell)` in this subtree, in layout order.
    pub fn leaves(&self) -> Vec<(TmuxPaneId, TmuxLayoutCell)> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves(&self, out: &mut Vec<(TmuxPaneId, TmuxLayoutCell)>) {
        match self {
            TmuxLayoutNode::Leaf { pane_id, cell } => out.push((*pane_id, *cell)),
            TmuxLayoutNode::Split { children, .. } => {
                for child in children {
                    child.collect_leaves(out);
                }
            }
        }
    }
}

fn parse_pane_id(pair: Pair<Rule>) -> Result<TmuxPaneId> {
    match pair.as_rule() {
        Rule::pane_id => {
            let mut pairs = pair.into_inner();
            pairs
                .next()
                .ok_or_else(|| format_err!("missing pane id"))?
                .as_str()
                .parse()
                .context("pane_id is somehow not digits")
        }
        _ => bail!("parse_pane_id can only parse Rule::pane_id, got {:?}", pair),
    }
}

fn parse_window_id(pair: Pair<Rule>) -> Result<TmuxWindowId> {
    match pair.as_rule() {
        Rule::window_id => {
            let mut pairs = pair.into_inner();
            pairs
                .next()
                .ok_or_else(|| format_err!("missing window id"))?
                .as_str()
                .parse()
                .context("window_id is somehow not digits")
        }
        _ => bail!(
            "parse_window_id can only parse Rule::window_id, got {:?}",
            pair
        ),
    }
}

fn parse_session_id(pair: Pair<Rule>) -> Result<TmuxSessionId> {
    match pair.as_rule() {
        Rule::session_id => {
            let mut pairs = pair.into_inner();
            pairs
                .next()
                .ok_or_else(|| format_err!("missing session id"))?
                .as_str()
                .parse()
                .context("session_id is somehow not digits")
        }
        _ => bail!(
            "parse_session_id can only parse Rule::session_id, got {:?}",
            pair
        ),
    }
}

/// Parse the body of a `%subscription-changed` notification (everything after
/// the `%subscription-changed ` prefix).
///
/// Example bodies:
///   `tmux_status_left $1 - - - : [main] 12:00`        (session-scoped)
///   `tmux_pane_title $1 @2 3 %4 : vim ~/src`          (pane-scoped)
///
/// The header (everything before the first ` : `) is split on whitespace into
/// the subscription name and the id/index fields; the value is the remainder
/// after ` : ` and may be empty. Parsing is best-effort: unrecognised id fields
/// simply become `None` rather than erroring.
fn parse_subscription_changed(body: &str) -> Event {
    let (header, value) = match body.split_once(" : ") {
        Some((header, value)) => (header, value.to_string()),
        None => (body, String::new()),
    };

    let mut fields = header.split(' ');
    let name = fields.next().unwrap_or("").to_string();
    let session = fields
        .next()
        .and_then(|s| s.strip_prefix('$'))
        .and_then(|n| n.parse().ok());
    let window = fields
        .next()
        .and_then(|s| s.strip_prefix('@'))
        .and_then(|n| n.parse().ok());
    let window_index = fields.next().and_then(|s| s.parse().ok());
    let pane = fields
        .next()
        .and_then(|s| s.strip_prefix('%'))
        .and_then(|n| n.parse().ok());

    Event::SubscriptionChanged {
        name,
        session,
        window,
        window_index,
        pane,
        value,
    }
}

/// Parses a %begin, %end, %error guard line tuple
fn parse_guard(mut pairs: Pairs<Rule>) -> Result<(i64, u64, i64)> {
    let timestamp = pairs
        .next()
        .ok_or_else(|| format_err!("missing timestamp"))?
        .as_str()
        .parse::<i64>()?;
    let number = pairs
        .next()
        .ok_or_else(|| format_err!("missing number"))?
        .as_str()
        .parse::<u64>()?;
    let flags = pairs
        .next()
        .ok_or_else(|| format_err!("missing flags"))?
        .as_str()
        .parse::<i64>()?;
    Ok((timestamp, number, flags))
}

fn parse_line(line: &[u8]) -> Result<Event> {
    let binding = String::from_utf8_lossy(line);
    let parsed_line = binding.as_ref();
    let mut pairs = parser::TmuxParser::parse(Rule::line_entire, parsed_line)?;
    let pair = pairs.next().ok_or_else(|| format_err!("no pairs!?"))?;
    match pair.as_rule() {
        // Tmux generic rules
        Rule::begin => {
            let (timestamp, number, flags) = parse_guard(pair.into_inner())?;
            Ok(Event::Begin {
                timestamp,
                number,
                flags,
            })
        }
        Rule::end => {
            let (timestamp, number, flags) = parse_guard(pair.into_inner())?;
            Ok(Event::End {
                timestamp,
                number,
                flags,
            })
        }
        Rule::error => {
            let (timestamp, number, flags) = parse_guard(pair.into_inner())?;
            Ok(Event::Error {
                timestamp,
                number,
                flags,
            })
        }

        // Tmux specific rules
        Rule::client_detached => {
            let mut pairs = pair.into_inner();
            let client_name = unvis(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing name"))?
                    .as_str(),
            )?;
            Ok(Event::ClientDetached { client_name })
        }
        Rule::client_session_changed => {
            let mut pairs = pair.into_inner();
            let client_name = unvis(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing name"))?
                    .as_str(),
            )?;
            let session = parse_session_id(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing session id"))?,
            )?;
            let session_name = unvis(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing session name"))?
                    .as_str(),
            )?;
            Ok(Event::ClientSessionChanged {
                client_name,
                session,
                session_name,
            })
        }
        Rule::config_error => {
            let mut pairs = pair.into_inner();
            let error = unvis(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing name"))?
                    .as_str(),
            )?;
            Ok(Event::ConfigError { error })
        }
        Rule::r#continue => {
            let mut pairs = pair.into_inner();
            let pane = parse_pane_id(pairs.next().ok_or_else(|| format_err!("missing pane id"))?)?;
            Ok(Event::Continue { pane })
        }
        Rule::extended_output => {
            let mut pairs = pair.into_inner();
            let pane = parse_pane_id(pairs.next().ok_or_else(|| format_err!("missing pane id"))?)?;
            let pair = pairs.next().ok_or_else(|| format_err!("missing text"))?;

            let (_, pos) = pair.line_col();
            let text = unvis_bytes(&line[pos - 1..])?;
            Ok(Event::ExtendedOutput { pane, text })
        }
        Rule::exit => {
            let mut pairs = pair.into_inner();
            let reason = pairs.next().map(|pair| pair.as_str().to_owned());
            Ok(Event::Exit { reason })
        }
        Rule::layout_change => {
            let mut pairs = pair.into_inner();
            let window = parse_window_id(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing window id"))?,
            )?;
            let layout = unvis(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing layout"))?
                    .as_str(),
            )?;
            let visible_layout = pairs.next().map(|pair| pair.as_str().to_owned());
            let raw_flags = pairs.next().map(|r| r.as_str().to_owned());
            Ok(Event::LayoutChange {
                window,
                layout,
                visible_layout,
                raw_flags,
            })
        }
        Rule::message => {
            let mut pairs = pair.into_inner();
            let message = unvis(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing text"))?
                    .as_str(),
            )?;
            Ok(Event::Message { message })
        }
        Rule::output => {
            let mut pairs = pair.into_inner();
            let pane = parse_pane_id(pairs.next().ok_or_else(|| format_err!("missing pane id"))?)?;
            let pair = pairs.next().ok_or_else(|| format_err!("missing text"))?;

            let (_, pos) = pair.line_col();
            let text = unvis_bytes(&line[pos - 1..])?;
            Ok(Event::Output { pane, text })
        }
        Rule::pane_mode_changed => {
            let mut pairs = pair.into_inner();
            let pane = parse_pane_id(pairs.next().ok_or_else(|| format_err!("missing pane id"))?)?;
            Ok(Event::PaneModeChanged { pane })
        }
        Rule::paste_buffer_changed => {
            let mut pairs = pair.into_inner();
            let buffer = unvis(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing text"))?
                    .as_str(),
            )?;
            Ok(Event::PasteBufferChanged { buffer })
        }
        Rule::paste_buffer_deleted => {
            let mut pairs = pair.into_inner();
            let buffer = unvis(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing text"))?
                    .as_str(),
            )?;
            Ok(Event::PasteBufferDeleted { buffer })
        }
        Rule::pause => {
            let mut pairs = pair.into_inner();
            let pane = parse_pane_id(pairs.next().ok_or_else(|| format_err!("missing pane id"))?)?;
            Ok(Event::Pause { pane })
        }
        Rule::session_changed => {
            let mut pairs = pair.into_inner();
            let session = parse_session_id(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing session id"))?,
            )?;
            let name = unvis(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing name"))?
                    .as_str(),
            )?;
            Ok(Event::SessionChanged { session, name })
        }
        Rule::session_renamed => {
            let mut pairs = pair.into_inner();
            let name = unvis(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing name"))?
                    .as_str(),
            )?;
            Ok(Event::SessionRenamed { name })
        }
        Rule::session_window_changed => {
            let mut pairs = pair.into_inner();
            let session = parse_session_id(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing session id"))?,
            )?;
            let window = parse_window_id(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing window id"))?,
            )?;
            Ok(Event::SessionWindowChanged { session, window })
        }
        Rule::sessions_changed => Ok(Event::SessionsChanged),
        Rule::subscription_changed => {
            let body = pair
                .into_inner()
                .next()
                .ok_or_else(|| format_err!("missing subscription body"))?
                .as_str();
            Ok(parse_subscription_changed(body))
        }
        Rule::unlinked_window_add => {
            let mut pairs = pair.into_inner();
            let window = parse_window_id(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing window id"))?,
            )?;
            Ok(Event::UnlinkedWindowAdd { window })
        }
        Rule::unlinked_window_close => {
            let mut pairs = pair.into_inner();
            let window = parse_window_id(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing window id"))?,
            )?;
            Ok(Event::UnlinkedWindowClose { window })
        }
        Rule::unlinked_window_renamed => {
            let mut pairs = pair.into_inner();
            let window = parse_window_id(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing window id"))?,
            )?;
            Ok(Event::UnlinkedWindowRenamed { window })
        }
        Rule::window_add => {
            let mut pairs = pair.into_inner();
            let window = parse_window_id(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing window id"))?,
            )?;
            Ok(Event::WindowAdd { window })
        }
        Rule::window_close => {
            let mut pairs = pair.into_inner();
            let window = parse_window_id(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing window id"))?,
            )?;
            Ok(Event::WindowClose { window })
        }
        Rule::window_pane_changed => {
            let mut pairs = pair.into_inner();
            let window = parse_window_id(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing window id"))?,
            )?;
            let pane = parse_pane_id(pairs.next().ok_or_else(|| format_err!("missing pane id"))?)?;
            Ok(Event::WindowPaneChanged { window, pane })
        }
        Rule::window_renamed => {
            let mut pairs = pair.into_inner();
            let window = parse_window_id(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing window id"))?,
            )?;
            let name = unvis(
                pairs
                    .next()
                    .ok_or_else(|| format_err!("missing name"))?
                    .as_str(),
            )?;
            Ok(Event::WindowRenamed { window, name })
        }
        Rule::EOI
        | Rule::any_text
        | Rule::client_name
        | Rule::layout_pane
        | Rule::layout_split_horizontal
        | Rule::layout_split_pane
        | Rule::layout_split_vertical
        | Rule::layout_window
        | Rule::line
        | Rule::line_entire
        | Rule::number
        | Rule::pane_id
        | Rule::session_id
        | Rule::window_id
        | Rule::window_layout
        | Rule::word => bail!("Should not reach here"),
    }
}

/// Decode OpenBSD `vis` encoded strings
/// See: https://github.com/tmux/tmux/blob/486ce9b09855ae30a2bf5e576cb6f7ad37792699/compat/unvis.c
fn unvis_bytes(s: &[u8]) -> Result<Vec<u8>> {
    enum State {
        Ground,
        Start,
        Meta,
        Meta1,
        Ctrl(u8),
        Octal2(u8),
        Octal3(u8),
    }

    let mut state = State::Ground;
    let mut result: Vec<u8> = vec![];
    let mut bytes = s.iter();

    fn is_octal(b: u8) -> bool {
        b >= b'0' && b <= b'7'
    }

    fn unvis_byte(b: u8, state: &mut State, result: &mut Vec<u8>) -> Result<bool> {
        match state {
            State::Ground => {
                if b == b'\\' {
                    *state = State::Start;
                } else {
                    result.push(b);
                }
            }

            State::Start => {
                match b {
                    b'\\' => {
                        result.push(b'\\');
                        *state = State::Ground;
                    }
                    b'0' | b'1' | b'2' | b'3' | b'4' | b'5' | b'6' | b'7' => {
                        let value = b - b'0';
                        *state = State::Octal2(value);
                    }
                    b'M' => {
                        *state = State::Meta;
                    }
                    b'^' => {
                        *state = State::Ctrl(0);
                    }
                    b'n' => {
                        result.push(b'\n');
                        *state = State::Ground;
                    }
                    b'r' => {
                        result.push(b'\r');
                        *state = State::Ground;
                    }
                    b'b' => {
                        result.push(b'\x08');
                        *state = State::Ground;
                    }
                    b'a' => {
                        result.push(b'\x07');
                        *state = State::Ground;
                    }
                    b'v' => {
                        result.push(b'\x0b');
                        *state = State::Ground;
                    }
                    b't' => {
                        result.push(b'\t');
                        *state = State::Ground;
                    }
                    b'f' => {
                        result.push(b'\x0c');
                        *state = State::Ground;
                    }
                    b's' => {
                        result.push(b' ');
                        *state = State::Ground;
                    }
                    b'E' => {
                        result.push(b'\x1b');
                        *state = State::Ground;
                    }
                    b'\n' => {
                        // Hidden newline
                        // result.push(b'\n');
                        *state = State::Ground;
                    }
                    b'$' => {
                        // Hidden marker
                        *state = State::Ground;
                    }
                    _ => {
                        // Invalid syntax
                        bail!("Invalid \\ escape: {}", b);
                    }
                }
            }

            State::Meta => {
                if b == b'-' {
                    *state = State::Meta1;
                } else if b == b'^' {
                    *state = State::Ctrl(0o200);
                } else {
                    bail!("invalid \\M escape: {}", b);
                }
            }

            State::Meta1 => {
                result.push(b | 0o200);
                *state = State::Ground;
            }

            State::Ctrl(c) => {
                if b == b'?' {
                    result.push(*c | 0o177);
                } else {
                    result.push((b & 0o37) | *c);
                }
                *state = State::Ground;
            }

            State::Octal2(prior) => {
                if is_octal(b) {
                    // It's the second in a 2 or 3 byte octal sequence
                    let value = (*prior << 3) + (b - b'0');
                    *state = State::Octal3(value);
                } else {
                    // Prior character was a single octal value
                    result.push(*prior);
                    *state = State::Ground;
                    // re-process the current byte
                    return Ok(true);
                }
            }

            State::Octal3(prior) => {
                if is_octal(b) {
                    // It's the third in a 3 byte octal sequence
                    let value = (*prior << 3) + (b - b'0');
                    result.push(value);
                    *state = State::Ground;
                } else {
                    // Prior was a 2-byte octal sequence
                    result.push(*prior);
                    *state = State::Ground;
                    // re-process the current byte
                    return Ok(true);
                }
            }
        }
        // Don't process this byte again
        Ok(false)
    }

    while let Some(&b) = bytes.next() {
        let again = unvis_byte(b, &mut state, &mut result)?;
        if again {
            unvis_byte(b, &mut state, &mut result)?;
        }
    }

    Ok(result)
}

pub fn unvis(s: &str) -> Result<String> {
    let bytes = s.as_bytes();

    let result = unvis_bytes(bytes)?;

    String::from_utf8(result)
        .map_err(|err| format_err!("Unescaped string is not valid UTF8: {}", err))
}

/// tmux prefixes a layout string with a 4 hex digit checksum followed by a
/// comma (`#{window_layout}` => e.g. `b25d,80x24,0,0,0`). The checksum is a
/// `%04hx` of a `u16`, so it is always exactly 4 hex digits. A real layout
/// cell always begins with `WxH` (i.e. contains an `x` before the first
/// comma), so an all-hex token before the first comma can only be a checksum.
/// Strip it if present; otherwise return the input unchanged.
fn strip_layout_checksum(layout: &str) -> &str {
    if let Some((prefix, rest)) = layout.split_once(',') {
        if prefix.len() == 4
            && !rest.is_empty()
            && prefix.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return rest;
        }
    }
    layout
}

fn parse_layout_u64(pair: Pair<Rule>) -> Result<u64> {
    pair.as_str()
        .parse()
        .map_err(|err| format_err!("invalid number in layout: {}", err))
}

/// Read the four `WxH,X,Y` numbers that begin a `layout_pane` or
/// `layout_split_pane` into a [`TmuxLayoutCell`], consuming them from `pairs`.
fn parse_layout_cell(pairs: &mut Pairs<Rule>) -> Result<TmuxLayoutCell> {
    let mut next = || {
        pairs
            .next()
            .ok_or_else(|| format_err!("layout cell: missing field"))
    };
    let width = parse_layout_u64(next()?)?;
    let height = parse_layout_u64(next()?)?;
    let left = parse_layout_u64(next()?)?;
    let top = parse_layout_u64(next()?)?;
    Ok(TmuxLayoutCell {
        width,
        height,
        left,
        top,
    })
}

fn build_layout_node(pair: Pair<Rule>) -> Result<TmuxLayoutNode> {
    match pair.as_rule() {
        Rule::layout_pane => {
            let mut inner = pair.into_inner();
            let cell = parse_layout_cell(&mut inner)?;
            let pane_id = parse_layout_u64(
                inner
                    .next()
                    .ok_or_else(|| format_err!("layout leaf: missing pane id"))?,
            )?;
            Ok(TmuxLayoutNode::Leaf { cell, pane_id })
        }
        rule @ (Rule::layout_split_horizontal | Rule::layout_split_vertical) => {
            let direction = if rule == Rule::layout_split_horizontal {
                TmuxSplitDirection::Horizontal
            } else {
                TmuxSplitDirection::Vertical
            };
            let mut inner = pair.into_inner();
            // The first inner pair is the `layout_split_pane` describing the
            // container's geometry; the rest are its children.
            let container = inner
                .next()
                .ok_or_else(|| format_err!("layout split: missing container cell"))?;
            let cell = parse_layout_cell(&mut container.into_inner())?;
            let mut children = Vec::new();
            for child in inner {
                children.push(build_layout_node(child)?);
            }
            if children.is_empty() {
                bail!("layout split: no children");
            }
            Ok(TmuxLayoutNode::Split {
                cell,
                direction,
                children,
            })
        }
        other => bail!("unexpected rule in layout: {:?}", other),
    }
}

/// Parse a tmux window layout string into a faithful n-ary [`TmuxLayoutNode`]
/// tree. Accepts the string with or without the leading checksum.
pub fn parse_layout_tree(layout: &str) -> Result<TmuxLayoutNode> {
    let body = strip_layout_checksum(layout);
    let mut pairs = parser::TmuxParser::parse(Rule::layout_window, body)?;
    let pair = pairs
        .next()
        .ok_or_else(|| format_err!("empty layout: {:?}", layout))?;
    // Ensure the whole layout was consumed (catches malformed trailing data).
    let end = pair.as_span().end();
    if end != body.len() {
        bail!(
            "trailing data in layout after offset {}: {:?}",
            end,
            layout
        );
    }
    build_layout_node(pair)
}

pub struct Parser {
    buffer: Vec<u8>,
    begun: Option<Guarded>,
}

impl Parser {
    pub fn new() -> Self {
        Self {
            buffer: vec![],
            begun: None,
        }
    }

    pub fn advance_byte(&mut self, c: u8) -> Result<Option<Event>> {
        if c == b'\n' {
            self.process_line()
        } else {
            self.buffer.push(c);
            Ok(None)
        }
    }

    pub fn advance_string(&mut self, s: &str) -> Result<Vec<Event>> {
        self.advance_bytes(s.as_bytes())
    }

    pub fn advance_bytes(&mut self, bytes: &[u8]) -> Result<Vec<Event>> {
        let mut events = vec![];
        for (i, &b) in bytes.iter().enumerate() {
            match self.advance_byte(b) {
                Ok(option_event) => {
                    if let Some(e) = option_event {
                        events.push(e);
                    }
                }
                Err(err) => {
                    // concat remained bytes after digested bytes
                    bail!("{}{}", err, String::from_utf8_lossy(&bytes[i..]));
                }
            }
        }
        Ok(events)
    }

    fn process_guarded_line(&mut self) -> Result<Option<Event>> {
        let line = std::str::from_utf8(&self.buffer)?;
        let result = match parse_line(&self.buffer) {
            Ok(Event::End {
                timestamp,
                number,
                flags,
            }) => {
                if let Some(begun) = self.begun.take() {
                    if begun.timestamp == timestamp
                        && begun.number == number
                        && begun.flags == flags
                    {
                        Some(Event::Guarded(begun))
                    } else {
                        log::error!("mismatched %end; expected {:?} but got {}", begun, line);
                        None
                    }
                } else {
                    log::error!("unexpected %end with no %begin ({})", line);
                    None
                }
            }
            Ok(Event::Error {
                timestamp,
                number,
                flags,
            }) => {
                if let Some(mut begun) = self.begun.take() {
                    if begun.timestamp == timestamp
                        && begun.number == number
                        && begun.flags == flags
                    {
                        begun.error = true;
                        Some(Event::Guarded(begun))
                    } else {
                        log::error!("mismatched %error; expected {:?} but got {}", begun, line);
                        None
                    }
                } else {
                    log::error!("unexpected %error with no %begin ({})", line);
                    None
                }
            }
            _ => {
                let begun = self
                    .begun
                    .as_mut()
                    .ok_or_else(|| format_err!("missing begun"))?;
                begun.output.push_str(line);
                begun.output.push('\n');
                None
            }
        };
        self.buffer.clear();
        return Ok(result);
    }

    fn process_line(&mut self) -> Result<Option<Event>> {
        if self.buffer.last() == Some(&b'\r') {
            self.buffer.pop();
        }
        if self.begun.is_some() {
            return self.process_guarded_line();
        }

        let result = match parse_line(&self.buffer) {
            Ok(Event::Begin {
                timestamp,
                number,
                flags,
            }) => {
                if self.begun.is_some() {
                    log::error!(
                        "expected %end or %error before %begin ({})",
                        String::from_utf8_lossy(&self.buffer)
                    );
                }
                self.begun.replace(Guarded {
                    timestamp,
                    number,
                    flags,
                    error: false,
                    output: String::new(),
                });
                None
            }
            Ok(event) => Some(event),
            Err(err) => {
                log::error!("Unrecognized tmux cc line: {}", err);
                bail!("{}", String::from_utf8_lossy(&self.buffer));
            }
        };

        self.buffer.clear();
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k9::assert_equal as assert_eq;

    #[test]
    fn test_parse_line() {
        let _ = env_logger::Builder::new()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        assert_eq!(
            Event::Begin {
                timestamp: 12345,
                number: 321,
                flags: 0,
            },
            parse_line(b"%begin 12345 321 0").unwrap()
        );

        assert_eq!(
            Event::End {
                timestamp: 12345,
                number: 321,
                flags: 0,
            },
            parse_line(b"%end 12345 321 0").unwrap()
        );
    }

    #[test]
    fn test_parse_sequence() {
        let input = b"%sessions-changed
%pane-mode-changed %0
%begin 1604279270 310 0
stuff
in
here
%end 1604279270 310 0
%window-add @1
%window-close @38
%unlinked-window-close @39
%sessions-changed
%session-changed $1 1
%client-session-changed /dev/pts/5 $1 home
%client-detached /dev/pts/10
%layout-change @1 b25d,80x24,0,0,0
%layout-change @1 cafd,120x29,0,0,0 cafd,120x29,0,0,0 *
%output %1 \\033[1m\\033[7m%\\033[27m\\033[1m\\033[0m    \\015 \\015
%output %1 \\033kwez@cube-localdomain:~\\033\\134\\033]2;wez@cube-localdomain:~\\033\\134
%output %1 \\033]7;file://cube-localdomain/home/wez\\033\\134
%output %1 \\033[K\\033[?2004h
%exit
%exit I said so
%config-error /home/joe/.tmux.conf:1: unknown command: dadsafafasdf
%continue %2
%extended-output %1 \\033[1m\\033[7m%\\033[27m\\033[1m\\033[0m    \\015 \\015
%message message text
%unlinked-window-add @40
%unlinked-window-renamed @41
%paste-buffer-changed just something
%paste-buffer-deleted just something else
%pause %3
%subscription-changed tmux_pane_title $1 @2 3 %4 : vim ~/src
";

        let mut p = Parser::new();
        let events = p.advance_bytes(input).unwrap();
        assert_eq!(
            vec![
                Event::SessionsChanged,
                Event::PaneModeChanged { pane: 0 },
                Event::Guarded(Guarded {
                    timestamp: 1604279270,
                    number: 310,
                    flags: 0,
                    error: false,
                    output: "stuff\nin\nhere\n".to_owned()
                }),
                Event::WindowAdd { window: 1 },
                Event::WindowClose { window: 38 },
                Event::UnlinkedWindowClose { window: 39 },
                Event::SessionsChanged,
                Event::SessionChanged {
                    session: 1,
                    name: "1".to_owned(),
                },
                Event::ClientSessionChanged {
                    client_name: "/dev/pts/5".to_owned(),
                    session: 1,
                    session_name: "home".to_owned()
                },
                Event::ClientDetached {
                    client_name: "/dev/pts/10".to_owned()
                },
                Event::LayoutChange {
                    window: 1,
                    layout: "b25d,80x24,0,0,0".to_owned(),
                    visible_layout: None,
                    raw_flags: None
                },
                Event::LayoutChange {
                    window: 1,
                    layout: "cafd,120x29,0,0,0".to_owned(),
                    visible_layout: Some("cafd,120x29,0,0,0".to_owned()),
                    raw_flags: Some("*".to_owned())
                },
                Event::Output {
                    pane: 1,
                    text: "\x1b[1m\x1b[7m%\x1b[27m\x1b[1m\x1b[0m    \r \r"
                        .to_owned()
                        .as_bytes()
                        .to_vec()
                },
                Event::Output {
                    pane: 1,
                    text: "\x1bkwez@cube-localdomain:~\x1b\\\x1b]2;wez@cube-localdomain:~\x1b\\"
                        .to_owned()
                        .as_bytes()
                        .to_vec()
                },
                Event::Output {
                    pane: 1,
                    text: "\x1b]7;file://cube-localdomain/home/wez\x1b\\"
                        .to_owned()
                        .as_bytes()
                        .to_vec(),
                },
                Event::Output {
                    pane: 1,
                    text: "\x1b[K\x1b[?2004h".to_owned().as_bytes().to_vec(),
                },
                Event::Exit { reason: None },
                Event::Exit {
                    reason: Some("I said so".to_owned())
                },
                Event::ConfigError {
                    error: "/home/joe/.tmux.conf:1: unknown command: dadsafafasdf".to_owned()
                },
                Event::Continue { pane: 2 },
                Event::ExtendedOutput {
                    pane: 1,
                    text: "\x1b[1m\x1b[7m%\x1b[27m\x1b[1m\x1b[0m    \r \r"
                        .to_owned()
                        .as_bytes()
                        .to_vec()
                },
                Event::Message {
                    message: "message text".to_owned()
                },
                Event::UnlinkedWindowAdd { window: 40 },
                Event::UnlinkedWindowRenamed { window: 41 },
                Event::PasteBufferChanged {
                    buffer: "just something".to_owned()
                },
                Event::PasteBufferDeleted {
                    buffer: "just something else".to_owned()
                },
                Event::Pause { pane: 3 },
                Event::SubscriptionChanged {
                    name: "tmux_pane_title".to_owned(),
                    session: Some(1),
                    window: Some(2),
                    window_index: Some(3),
                    pane: Some(4),
                    value: "vim ~/src".to_owned(),
                },
            ],
            events
        );
    }

    fn leaf(node: &TmuxLayoutNode) -> (TmuxPaneId, TmuxLayoutCell) {
        match node {
            TmuxLayoutNode::Leaf { pane_id, cell } => (*pane_id, *cell),
            other => panic!("expected leaf, got {:?}", other),
        }
    }

    fn split(node: &TmuxLayoutNode) -> (TmuxSplitDirection, &[TmuxLayoutNode], TmuxLayoutCell) {
        match node {
            TmuxLayoutNode::Split {
                direction,
                children,
                cell,
            } => (*direction, children.as_slice(), *cell),
            other => panic!("expected split, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_layout_tree_single() {
        // A single pane (real capture: `b25d,80x24,0,0,0`).
        let node = parse_layout_tree("80x24,0,0,0").unwrap();
        assert_eq!(
            node,
            TmuxLayoutNode::Leaf {
                cell: TmuxLayoutCell {
                    width: 80,
                    height: 24,
                    left: 0,
                    top: 0
                },
                pane_id: 0,
            }
        );
        assert_eq!(node.pane_ids(), vec![0]);
    }

    #[test]
    fn test_parse_subscription_changed() {
        // pane-scoped: all fields populated
        assert_eq!(
            Event::SubscriptionChanged {
                name: "tmux_pane_current_path".to_owned(),
                session: Some(1),
                window: Some(2),
                window_index: Some(3),
                pane: Some(4),
                value: "/home/wez/src".to_owned(),
            },
            parse_line(b"%subscription-changed tmux_pane_current_path $1 @2 3 %4 : /home/wez/src")
                .unwrap()
        );

        // session-scoped: window/index/pane are `-`
        assert_eq!(
            Event::SubscriptionChanged {
                name: "tmux_status_left".to_owned(),
                session: Some(1),
                window: None,
                window_index: None,
                pane: None,
                value: "[main] 12:00".to_owned(),
            },
            parse_line(b"%subscription-changed tmux_status_left $1 - - - : [main] 12:00").unwrap()
        );

        // empty value (note the trailing space before the empty value)
        assert_eq!(
            Event::SubscriptionChanged {
                name: "tmux_status_right".to_owned(),
                session: Some(1),
                window: None,
                window_index: None,
                pane: None,
                value: "".to_owned(),
            },
            parse_line(b"%subscription-changed tmux_status_right $1 - - - : ").unwrap()
        );

        // value containing a colon must be preserved intact
        assert_eq!(
            Event::SubscriptionChanged {
                name: "tmux_status_left".to_owned(),
                session: Some(1),
                window: None,
                window_index: None,
                pane: None,
                value: "12:34:56".to_owned(),
            },
            parse_line(b"%subscription-changed tmux_status_left $1 - - - : 12:34:56").unwrap()
        );
    }

    #[test]
    fn test_parse_layout_tree_checksum_optional() {
        // Parsing with and without the leading checksum yields the same tree.
        let with = parse_layout_tree("b25d,80x24,0,0,0").unwrap();
        let without = parse_layout_tree("80x24,0,0,0").unwrap();
        assert_eq!(with, without);
    }

    #[test]
    fn test_parse_layout_tree_horizontal() {
        // Real capture of `split-window -h`.
        let node = parse_layout_tree("80x24,0,0{40x24,0,0,0,39x24,41,0,1}").unwrap();
        let (dir, children, cell) = split(&node);
        assert_eq!(dir, TmuxSplitDirection::Horizontal);
        assert_eq!(cell.width, 80);
        assert_eq!(children.len(), 2);
        assert_eq!(leaf(&children[0]).0, 0);
        assert_eq!(leaf(&children[1]).0, 1);
        assert_eq!(node.pane_ids(), vec![0, 1]);
    }

    #[test]
    fn test_parse_layout_tree_deeply_nested() {
        // Real capture: H{ pane0, V[ pane1, H{ pane2, pane3 } ] }.
        let node = parse_layout_tree(
            "1558,80x24,0,0{40x24,0,0,0,39x24,41,0[39x12,41,0,1,39x11,41,13{19x11,41,13,2,19x11,61,13,3}]}",
        )
        .unwrap();
        let (dir, children, _) = split(&node);
        assert_eq!(dir, TmuxSplitDirection::Horizontal);
        assert_eq!(children.len(), 2);
        assert_eq!(leaf(&children[0]), (
            0,
            TmuxLayoutCell { width: 40, height: 24, left: 0, top: 0 }
        ));

        let (dir1, children1, cell1) = split(&children[1]);
        assert_eq!(dir1, TmuxSplitDirection::Vertical);
        assert_eq!(cell1, TmuxLayoutCell { width: 39, height: 24, left: 41, top: 0 });
        assert_eq!(children1.len(), 2);
        assert_eq!(leaf(&children1[0]).0, 1);

        let (dir2, children2, _) = split(&children1[1]);
        assert_eq!(dir2, TmuxSplitDirection::Horizontal);
        assert_eq!(children2.len(), 2);
        assert_eq!(leaf(&children2[0]).0, 2);
        assert_eq!(leaf(&children2[1]).0, 3);

        // Faithful in-order pane enumeration.
        assert_eq!(node.pane_ids(), vec![0, 1, 2, 3]);
    }

    #[test]
    fn test_parse_layout_tree_many_children() {
        // Real capture of `select-layout even-horizontal`: ONE split, FOUR children.
        let node =
            parse_layout_tree("764c,80x24,0,0{19x24,0,0,0,19x24,20,0,1,19x24,40,0,2,20x24,60,0,3}")
                .unwrap();
        let (dir, children, cell) = split(&node);
        assert_eq!(dir, TmuxSplitDirection::Horizontal);
        assert_eq!(children.len(), 4);
        assert_eq!(node.pane_ids(), vec![0, 1, 2, 3]);

        // Divider arithmetic invariant: sum of child widths + (n-1) dividers
        // equals the container width.
        let total: u64 = children.iter().map(|c| c.cell().width).sum();
        assert_eq!(total + (children.len() as u64 - 1), cell.width);
        // And the lefts step over each child plus a 1-cell divider.
        let mut x = cell.left;
        for c in children {
            assert_eq!(c.cell().left, x);
            x += c.cell().width + 1;
        }
    }

    #[test]
    fn test_parse_layout_tree_rejects_trailing_garbage() {
        assert!(parse_layout_tree("80x24,0,0,0,garbage").is_err());
    }
}
