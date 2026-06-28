//! Conversion of a tmux window layout tree into wezterm's binary `PaneNode`
//! tree.
//!
//! tmux describes a window as an n-ary tree of layout cells (`{...}` splits its
//! children left-to-right, `[...]` top-to-bottom, and a split may have three or
//! more children). wezterm's [`Tab`](crate::tab::Tab) instead uses a *binary*
//! split tree. To mirror a tmux layout faithfully we encode each n-ary tmux
//! split as a right-leaning chain of binary splits:
//!
//! ```text
//! tmux:  { A | B | C }            wezterm:        H
//!                                                / \
//!                                               A   H
//!                                                  / \
//!                                                 B   C
//! ```
//!
//! Because tmux guarantees that, along the split axis, the children sizes plus
//! the `(n-1)` one-cell dividers sum to the container size, and wezterm uses the
//! exact same one-cell divider convention
//! ([`SplitDirectionAndSize::left_of_second`](crate::tab) is `first.cols + 1`),
//! the rectangles rendered for every pane are identical to tmux's.
//!
//! This module is intentionally pure: it depends only on the parsed layout and a
//! [`LayoutContext`], so it can be unit-tested without a live mux.

use crate::pane::PaneId;
use crate::renderable::StableCursorPosition;
use crate::tab::{PaneEntry, PaneNode, SplitDirection, SplitDirectionAndSize, TabId};
use crate::window::WindowId;
use std::collections::HashMap;
use termwiz::tmux_cc::{TmuxLayoutCell, TmuxLayoutNode, TmuxPaneId, TmuxSplitDirection};
use wezterm_term::TerminalSize;

/// Everything needed to materialize a tmux layout tree into wezterm
/// `PaneNode`s, beyond the geometry carried by the layout itself.
pub(crate) struct LayoutContext<'a> {
    pub window_id: WindowId,
    pub tab_id: TabId,
    pub workspace: String,
    /// Cell pixel geometry + dpi, used to fill `TerminalSize`. Layout fidelity
    /// only depends on rows/cols; the pixel values are derived consistently so
    /// that the subsequent `Tab::resize` is a no-op.
    pub dpi: u32,
    pub cell_pixel_width: usize,
    pub cell_pixel_height: usize,
    /// The tmux pane id that is active in this window, if known.
    pub active_pane: Option<TmuxPaneId>,
    /// The tmux pane id that is zoomed in this window, if any.
    pub zoomed_pane: Option<TmuxPaneId>,
    /// Maps a tmux pane id to its already-created local wezterm pane id. Every
    /// pane present in the layout must have an entry (the caller reconciles the
    /// pane set before building the tree).
    pub local_pane_ids: &'a HashMap<TmuxPaneId, PaneId>,
}

impl<'a> LayoutContext<'a> {
    fn terminal_size(&self, cell: &TmuxLayoutCell) -> TerminalSize {
        let cols = cell.width as usize;
        let rows = cell.height as usize;
        TerminalSize {
            rows,
            cols,
            pixel_width: cols * self.cell_pixel_width,
            pixel_height: rows * self.cell_pixel_height,
            dpi: self.dpi,
        }
    }

    fn leaf(&self, cell: &TmuxLayoutCell, tmux_pane_id: TmuxPaneId) -> PaneEntry {
        PaneEntry {
            window_id: self.window_id,
            tab_id: self.tab_id,
            // Fall back to the tmux id only if the caller failed to reconcile;
            // in practice every pane is mapped before we get here.
            pane_id: self
                .local_pane_ids
                .get(&tmux_pane_id)
                .copied()
                .unwrap_or(tmux_pane_id as PaneId),
            title: String::new(),
            size: self.terminal_size(cell),
            working_dir: None,
            is_active_pane: self.active_pane == Some(tmux_pane_id),
            is_zoomed_pane: self.zoomed_pane == Some(tmux_pane_id),
            workspace: self.workspace.clone(),
            cursor_pos: StableCursorPosition::default(),
            physical_top: 0,
            top_row: 0,
            left_col: 0,
            tty_name: None,
        }
    }
}

fn map_direction(direction: TmuxSplitDirection) -> SplitDirection {
    match direction {
        TmuxSplitDirection::Horizontal => SplitDirection::Horizontal,
        TmuxSplitDirection::Vertical => SplitDirection::Vertical,
    }
}

/// The bounding cell of `children[1..]` (everything past the head), derived from
/// the container and the head along the split axis. Equal to the union of the
/// tail children's cells given tmux's `Σchild + (n-1) dividers == container`
/// invariant.
fn tail_container(
    direction: TmuxSplitDirection,
    container: &TmuxLayoutCell,
    head: &TmuxLayoutCell,
) -> TmuxLayoutCell {
    match direction {
        TmuxSplitDirection::Horizontal => TmuxLayoutCell {
            left: head.left + head.width + 1,
            top: container.top,
            width: container.width.saturating_sub(head.width + 1),
            height: container.height,
        },
        TmuxSplitDirection::Vertical => TmuxLayoutCell {
            left: container.left,
            top: head.top + head.height + 1,
            width: container.width,
            height: container.height.saturating_sub(head.height + 1),
        },
    }
}

fn build_split_chain(
    direction: TmuxSplitDirection,
    container: &TmuxLayoutCell,
    children: &[TmuxLayoutNode],
    ctx: &LayoutContext,
) -> PaneNode {
    // A split always has at least one child (the parser guarantees this); real
    // tmux splits have two or more. A single child collapses to that child.
    match children {
        [] => PaneNode::Empty,
        [only] => build_pane_node(only, ctx),
        [head, rest @ ..] => {
            let head_cell = head.cell();
            let tail = tail_container(direction, container, &head_cell);
            PaneNode::Split {
                left: Box::new(build_pane_node(head, ctx)),
                right: Box::new(build_split_chain(direction, &tail, rest, ctx)),
                node: SplitDirectionAndSize {
                    direction: map_direction(direction),
                    first: ctx.terminal_size(&head_cell),
                    second: ctx.terminal_size(&tail),
                },
            }
        }
    }
}

/// Convert a faithful tmux layout tree into a wezterm `PaneNode` whose rendered
/// pane rectangles match tmux exactly.
pub(crate) fn build_pane_node(layout: &TmuxLayoutNode, ctx: &LayoutContext) -> PaneNode {
    match layout {
        TmuxLayoutNode::Leaf { cell, pane_id } => PaneNode::Leaf(ctx.leaf(cell, *pane_id)),
        TmuxLayoutNode::Split {
            cell,
            direction,
            children,
        } => build_split_chain(*direction, cell, children, ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termwiz::tmux_cc::parse_layout_tree;

    fn ctx(map: &HashMap<TmuxPaneId, PaneId>) -> LayoutContext<'_> {
        LayoutContext {
            window_id: 0,
            tab_id: 0,
            workspace: "default".to_string(),
            dpi: 96,
            cell_pixel_width: 1,
            cell_pixel_height: 1,
            active_pane: None,
            zoomed_pane: None,
            local_pane_ids: map,
        }
    }

    /// Walk the produced `PaneNode` using the exact positioning math that
    /// `Tab` uses (divider = 1 cell; `left_of_second = first.cols + 1`,
    /// `top_of_second = first.rows + 1`) and return every leaf's rectangle.
    fn rendered_rects(node: &PaneNode) -> HashMap<PaneId, (usize, usize, usize, usize)> {
        fn walk(
            node: &PaneNode,
            left: usize,
            top: usize,
            out: &mut HashMap<PaneId, (usize, usize, usize, usize)>,
        ) {
            match node {
                PaneNode::Empty => {}
                PaneNode::Leaf(entry) => {
                    out.insert(entry.pane_id, (left, top, entry.size.cols, entry.size.rows));
                }
                PaneNode::Split { left: l, right: r, node } => {
                    walk(l, left, top, out);
                    let (rl, rt) = match node.direction {
                        SplitDirection::Horizontal => (left + node.first.cols + 1, top),
                        SplitDirection::Vertical => (left, top + node.first.rows + 1),
                    };
                    walk(r, rl, rt, out);
                }
            }
        }
        let mut out = HashMap::new();
        walk(node, 0, 0, &mut out);
        out
    }

    /// The rectangles tmux itself describes for each pane.
    fn tmux_rects(node: &TmuxLayoutNode) -> HashMap<PaneId, (usize, usize, usize, usize)> {
        fn walk(node: &TmuxLayoutNode, out: &mut HashMap<PaneId, (usize, usize, usize, usize)>) {
            match node {
                TmuxLayoutNode::Leaf { cell, pane_id } => {
                    out.insert(
                        *pane_id as PaneId,
                        (
                            cell.left as usize,
                            cell.top as usize,
                            cell.width as usize,
                            cell.height as usize,
                        ),
                    );
                }
                TmuxLayoutNode::Split { children, .. } => {
                    for c in children {
                        walk(c, out);
                    }
                }
            }
        }
        let mut out = HashMap::new();
        walk(node, &mut out);
        out
    }

    /// Identity tmux-id -> local-id mapping for the pane ids in `layout`.
    fn identity_map(layout: &TmuxLayoutNode) -> HashMap<TmuxPaneId, PaneId> {
        layout
            .pane_ids()
            .into_iter()
            .map(|id| (id, id as PaneId))
            .collect()
    }

    fn assert_faithful(layout_str: &str) {
        let layout = parse_layout_tree(layout_str).unwrap();
        let map = identity_map(&layout);
        let node = build_pane_node(&layout, &ctx(&map));
        assert_eq!(
            rendered_rects(&node),
            tmux_rects(&layout),
            "rendered rectangles must match tmux for {layout_str}"
        );
    }

    #[test]
    fn faithful_single() {
        assert_faithful("80x24,0,0,0");
    }

    #[test]
    fn faithful_horizontal() {
        assert_faithful("80x24,0,0{40x24,0,0,0,39x24,41,0,1}");
    }

    #[test]
    fn faithful_vertical() {
        assert_faithful("80x24,0,0[80x12,0,0,1,80x11,0,13,2]");
    }

    #[test]
    fn faithful_deeply_nested() {
        assert_faithful(
            "1558,80x24,0,0{40x24,0,0,0,39x24,41,0[39x12,41,0,1,39x11,41,13{19x11,41,13,2,19x11,61,13,3}]}",
        );
    }

    #[test]
    fn faithful_four_children() {
        // even-horizontal: one split, four children -> right-leaning chain.
        assert_faithful("764c,80x24,0,0{19x24,0,0,0,19x24,20,0,1,19x24,40,0,2,20x24,60,0,3}");
    }

    #[test]
    fn faithful_tiled() {
        assert_faithful(
            "30d6,80x24,0,0[80x11,0,0{39x11,0,0,0,40x11,40,0,1},80x12,0,12{39x12,0,12,2,40x12,40,12,3}]",
        );
    }

    #[test]
    fn structure_is_right_leaning_binary_chain() {
        // Four children must become three nested binary splits, not a flat node.
        let layout =
            parse_layout_tree("80x24,0,0{19x24,0,0,0,19x24,20,0,1,19x24,40,0,2,20x24,60,0,3}")
                .unwrap();
        let map = identity_map(&layout);
        let node = build_pane_node(&layout, &ctx(&map));
        // depth of right spine == number of children - 1
        let mut depth = 0;
        let mut cur = &node;
        while let PaneNode::Split { right, .. } = cur {
            depth += 1;
            cur = right;
        }
        assert_eq!(depth, 3);
        assert!(matches!(cur, PaneNode::Leaf(_)));
    }

    #[test]
    fn active_and_zoom_flags_propagate() {
        let layout = parse_layout_tree("80x24,0,0{40x24,0,0,0,39x24,41,0,1}").unwrap();
        let map = identity_map(&layout);
        let mut c = ctx(&map);
        c.active_pane = Some(1);
        c.zoomed_pane = Some(1);
        let node = build_pane_node(&layout, &c);

        fn find(node: &PaneNode, pane_id: PaneId) -> Option<&PaneEntry> {
            match node {
                PaneNode::Leaf(e) if e.pane_id == pane_id => Some(e),
                PaneNode::Split { left, right, .. } => {
                    find(left, pane_id).or_else(|| find(right, pane_id))
                }
                _ => None,
            }
        }
        assert!(find(&node, 0).unwrap().is_active_pane == false);
        assert!(find(&node, 1).unwrap().is_active_pane);
        assert!(find(&node, 1).unwrap().is_zoomed_pane);
    }
}
