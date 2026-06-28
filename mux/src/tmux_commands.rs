use crate::domain::{DomainId, WriterWrapper};
use crate::localpane::LocalPane;
use crate::pane::{alloc_pane_id, PaneId};
use crate::tab::{SplitDirection, Tab};
use crate::tmux::{AttachState, TmuxDomain, TmuxDomainState, TmuxRemotePane, TmuxTab};
use crate::tmux_layout::{build_pane_node, LayoutContext};
use crate::tmux_pty::{TmuxChild, TmuxPty};
use crate::{Mux, MuxNotification, Pane};
use anyhow::{anyhow, Context};
use parking_lot::{Condvar, Mutex};
use portable_pty::{MasterPty, PtySize};
use std::collections::{HashMap, HashSet};
use std::fmt::{Debug, Write};
use std::io::Write as _;
use std::sync::Arc;
use termwiz::escape::csi::{Cursor, CSI};
use termwiz::escape::{Action, OneBased};
use termwiz::tmux_cc::*;
use wezterm_term::TerminalSize;

pub(crate) trait TmuxCommand: Send + Debug {
    fn get_command(&self, domain_id: DomainId) -> String;
    fn process_result(&self, domain_id: DomainId, result: &Guarded) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PaneItem {
    session_id: TmuxSessionId,
    window_id: TmuxWindowId,
    pane_id: TmuxPaneId,
    _pane_index: u64,
    cursor_x: u64,
    cursor_y: u64,
    pane_width: u64,
    pane_height: u64,
    pane_left: u64,
    pane_top: u64,
    pane_active: bool,
}

#[derive(Debug)]
struct WindowItem {
    session_id: TmuxSessionId,
    window_id: TmuxWindowId,
    window_width: u64,
    window_height: u64,
    window_active: bool,
    window_name: String,
    /// Raw tmux layout string (including the leading checksum).
    window_layout: String,
    /// Raw `#{window_visible_layout}` (single pane when zoomed).
    window_visible_layout: String,
    /// Raw `#{window_raw_flags}` (contains `Z` when the window is zoomed).
    window_raw_flags: String,
    history_limit: isize,
}

impl TmuxDomainState {
    /// check if a PaneItem received from ListAllPanes has been attached
    pub fn check_pane_attached(&self, window_id: TmuxWindowId, pane_id: TmuxPaneId) -> bool {
        let gui_tabs = self.gui_tabs.lock();
        let Some(local_tab) = gui_tabs.get(&window_id) else {
            return false;
        };

        return local_tab.panes.get(&pane_id).is_some();
    }

    pub fn check_window_attached(&self, window_id: TmuxWindowId) -> bool {
        let gui_tabs = self.gui_tabs.lock();
        return gui_tabs.get(&window_id).is_some();
    }

    pub fn remove_detached_window(&self, window_id: TmuxWindowId) -> anyhow::Result<()> {
        let mut gui_tabs = self.gui_tabs.lock();
        let tab = match gui_tabs.get(&window_id) {
            Some(x) => x,
            None => {
                anyhow::bail!("Cannot find the window {window_id}")
            }
        };

        let mux = Mux::get();
        mux.remove_tab(tab.tab_id);
        gui_tabs.remove(&window_id);

        Ok(())
    }

    fn set_pane_cursor_position(&self, pane: &Arc<dyn Pane>, x: usize, y: usize) {
        pane.perform_actions(vec![Action::CSI(CSI::Cursor(
            Cursor::CharacterAndLinePosition {
                col: OneBased::from_zero_based(x as u32),
                line: OneBased::from_zero_based(y as u32),
            },
        ))]);
    }

    fn create_pane(&self, pane: &PaneItem) -> anyhow::Result<Arc<dyn Pane>> {
        let local_pane_id = alloc_pane_id();
        let active_lock = Arc::new((Mutex::new(false), Condvar::new()));
        let (output_read, output_write) = filedescriptor::socketpair()?;
        let ref_pane = Arc::new(Mutex::new(TmuxRemotePane {
            local_pane_id,
            output_write,
            active_lock: active_lock.clone(),
            session_id: 0,
            window_id: pane.window_id,
            pane_id: pane.pane_id,
            cursor_x: pane.cursor_x,
            cursor_y: pane.cursor_y,
            pane_width: pane.pane_width,
            pane_height: pane.pane_height,
            pane_left: pane.pane_left,
            pane_top: pane.pane_top,
        }));

        {
            let mut pane_map = self.remote_panes.lock();
            pane_map.insert(pane.pane_id, ref_pane.clone());
        }

        let pane_pty = TmuxPty {
            domain_id: self.domain_id,
            reader: output_read,
            cmd_queue: self.cmd_queue.clone(),
            master_pane: ref_pane,
        };

        let writer = WriterWrapper::new(pane_pty.take_writer()?);

        let size = TerminalSize {
            rows: pane.pane_height as usize,
            cols: pane.pane_width as usize,
            pixel_width: 0,
            pixel_height: 0,
            dpi: 0,
        };

        let child = TmuxChild {
            active_lock: active_lock.clone(),
        };

        let terminal = wezterm_term::Terminal::new(
            size,
            std::sync::Arc::new(config::TermConfig::new()),
            "WezTerm",
            config::wezterm_version(),
            Box::new(writer.clone()),
        );

        let local_pane: Arc<dyn Pane> = Arc::new(LocalPane::new(
            local_pane_id,
            terminal,
            Box::new(child),
            Box::new(pane_pty),
            Box::new(writer),
            self.domain_id,
            "tmux pane".to_string(),
        ));

        // Seed session/window-scoped format subscription values so this pane
        // shows the current value immediately. tmux only re-emits these on
        // change, so without this a new tab would have empty values until the
        // next change (e.g. up to a minute for a clock in status-left).
        let seed: Vec<(String, String)> = self
            .session_format_values
            .lock()
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        for (name, value) in seed {
            local_pane.set_user_var(name, value);
        }

        Ok(local_pane)
    }

    /// Apply a parsed tmux window layout to the corresponding local tab,
    /// rebuilding its pane tree atomically (flicker-free, idempotent). New tmux
    /// panes are created and existing ones reused; panes that disappeared are
    /// reaped by the deferred global prune. The local tab for `window_id` must
    /// already exist.
    fn apply_layout(
        &self,
        window_id: TmuxWindowId,
        layout: &TmuxLayoutNode,
        active_pane: Option<TmuxPaneId>,
        zoomed_pane: Option<TmuxPaneId>,
        capture_new: bool,
    ) -> anyhow::Result<()> {
        let mux = Mux::get();

        let (tab_id, history_limit) = {
            let gui_tabs = self.gui_tabs.lock();
            let Some(t) = gui_tabs.get(&window_id) else {
                anyhow::bail!("apply_layout: tmux window @{window_id} has no local tab");
            };
            (t.tab_id, t.history_limit)
        };

        let Some(tab) = mux.get_tab(tab_id) else {
            anyhow::bail!("apply_layout: local tab {tab_id} is gone");
        };

        // We always rebuild rather than skipping on an unchanged checksum: the
        // rebuild reuses existing pane objects (so it is flicker-free and
        // effectively idempotent), and zoom toggles arrive as a %layout-change
        // whose full layout — and thus checksum — is unchanged, so a checksum
        // skip would miss them.

        // 1) Reconcile the pane set: create local panes for tmux panes we have
        // not seen yet (split results, freshly attached windows); reuse the rest.
        let leaves = layout.leaves();
        let mut newly_created: Vec<Arc<dyn Pane>> = Vec::new();
        for (tmux_pid, cell) in &leaves {
            let already_exists = self.remote_panes.lock().contains_key(tmux_pid);
            if already_exists {
                continue;
            }
            let item = PaneItem {
                session_id: 0,
                window_id,
                pane_id: *tmux_pid,
                _pane_index: 0,
                cursor_x: 0,
                cursor_y: 0,
                pane_width: cell.width,
                pane_height: cell.height,
                pane_left: cell.left,
                pane_top: cell.top,
                pane_active: false,
            };
            let local_pane = self
                .create_pane(&item)
                .context("failed to create tmux pane")?;
            let _ = mux.add_pane(&local_pane);

            // Flush any output that arrived before the pane existed.
            if let Some(text) = self.backlog.lock().remove(tmux_pid) {
                if let Some(ref_pane) = self.remote_panes.lock().get(tmux_pid) {
                    let _ = ref_pane.lock().output_write.write_all(&text);
                }
            }

            if capture_new {
                self.cmd_queue.lock().push_back(Box::new(CapturePane {
                    pane_id: *tmux_pid,
                    history_limit,
                }));
            }
            newly_created.push(local_pane);
        }

        // 2) Collect (tmux -> local) ids and live handles; bail before touching
        // the tab if anything is unexpectedly missing.
        let mut local_pane_ids: HashMap<TmuxPaneId, PaneId> = HashMap::new();
        let mut panes_by_local: HashMap<PaneId, Arc<dyn Pane>> = HashMap::new();
        for (tmux_pid, _) in &leaves {
            let local_id = match self.remote_panes.lock().get(tmux_pid) {
                Some(p) => p.lock().local_pane_id,
                None => anyhow::bail!("apply_layout: pane %{tmux_pid} missing after reconcile"),
            };
            let pane = mux
                .get_pane(local_id)
                .ok_or_else(|| anyhow!("apply_layout: local pane {local_id} missing"))?;
            local_pane_ids.insert(*tmux_pid, local_id);
            panes_by_local.insert(local_id, pane);
        }

        // 3) Build the wezterm PaneNode tree from the tmux layout.
        let Some(gui_window_id) = self.gui_window.lock().as_ref().map(|w| w.window_id) else {
            anyhow::bail!("apply_layout: no gui window");
        };
        let workspace = mux
            .get_window(gui_window_id)
            .map(|w| w.get_workspace().to_string())
            .unwrap_or_default();
        let (cell_pixel_width, cell_pixel_height, dpi) = self.cell_pixel_dimensions();

        let root = {
            let ctx = LayoutContext {
                window_id: gui_window_id,
                tab_id,
                workspace,
                dpi,
                cell_pixel_width,
                cell_pixel_height,
                active_pane,
                zoomed_pane,
                local_pane_ids: &local_pane_ids,
            };
            build_pane_node(layout, &ctx)
        };

        let root_cell = layout.cell();
        let size = TerminalSize {
            rows: root_cell.height as usize,
            cols: root_cell.width as usize,
            pixel_width: root_cell.width as usize * cell_pixel_width,
            pixel_height: root_cell.height as usize * cell_pixel_height,
            dpi,
        };

        // 4) Atomically swap in the rebuilt tree, reusing existing pane objects.
        tab.sync_with_pane_tree(size, root, |entry| {
            panes_by_local
                .get(&entry.pane_id)
                .cloned()
                .or_else(|| mux.get_pane(entry.pane_id))
                .expect("apply_layout: pane materialized during reconcile")
        });

        // 5) Record the new pane set (used by the global prune).
        {
            let mut gui_tabs = self.gui_tabs.lock();
            if let Some(t) = gui_tabs.get_mut(&window_id) {
                t.panes = leaves.iter().map(|(pid, _)| *pid).collect();
            }
        }

        // 6) Resolve any user-initiated split awaiting a freshly created pane.
        if !capture_new {
            for pane in &newly_created {
                let mut pending = self.pending_splits.lock();
                match pending.pop_front() {
                    Some(mut promise) => {
                        promise.ok(pane.pane_id());
                    }
                    None => break,
                }
            }
        }

        // 7) Reap panes that no longer appear in any window, once quiescent.
        self.schedule_prune();

        Ok(())
    }

    /// Cell pixel geometry + dpi from the controlling tmux pane, used to fill
    /// `TerminalSize` pixel fields. Falls back to zeros (tolerated by the layout
    /// math) if the controlling pane is unavailable.
    fn cell_pixel_dimensions(&self) -> (usize, usize, u32) {
        match Mux::get().get_pane(self.pane_id) {
            Some(p) => {
                let d = p.get_dimensions();
                let cpw = if d.cols > 0 { d.pixel_width / d.cols } else { 0 };
                let cph = if d.viewport_rows > 0 {
                    d.pixel_height / d.viewport_rows
                } else {
                    0
                };
                (cpw, cph, d.dpi)
            }
            None => (0, 0, 0),
        }
    }

    /// Handle a `%layout-change`: parse the layout, derive zoom state, and
    /// rebuild the local tab.
    pub fn handle_layout_change(
        &self,
        window_id: TmuxWindowId,
        layout: &str,
        visible_layout: Option<&str>,
        raw_flags: Option<&str>,
    ) -> anyhow::Result<()> {
        if !self.check_window_attached(window_id) {
            // tmux does not emit %layout-change for a window before it is linked
            // (window-add + list-windows build its tab); ignore until then.
            log::debug!("tmux layout-change for unattached window @{window_id}; ignoring");
            return Ok(());
        }

        let tree =
            parse_layout_tree(layout).with_context(|| format!("parsing tmux layout {layout:?}"))?;
        let active = self.gui_tabs.lock().get(&window_id).and_then(|t| t.active_pane);

        // The full layout always lists every pane; zoom is signalled by the `Z`
        // flag and the zoomed pane is the sole leaf of the visible layout
        // (equivalently, the active pane).
        let zoomed = raw_flags.map_or(false, |f| f.contains('Z'));
        let zoomed_pane = if zoomed {
            visible_layout
                .and_then(|v| parse_layout_tree(v).ok())
                .and_then(|v| match v {
                    TmuxLayoutNode::Leaf { pane_id, .. } => Some(pane_id),
                    _ => active,
                })
                .or(active)
        } else {
            None
        };

        self.apply_layout(window_id, &tree, active, zoomed_pane, false)
    }

    /// Track the active pane for a window (from `%window-pane-changed`) and, if
    /// the corresponding local pane already exists, focus it.
    pub fn set_active_tmux_pane(&self, window_id: TmuxWindowId, pane_id: TmuxPaneId) {
        let tab_id = {
            let mut gui_tabs = self.gui_tabs.lock();
            let Some(t) = gui_tabs.get_mut(&window_id) else {
                return;
            };
            t.active_pane = Some(pane_id);
            t.tab_id
        };

        let mux = Mux::get();
        let local_id = self
            .remote_panes
            .lock()
            .get(&pane_id)
            .map(|p| p.lock().local_pane_id);
        if let (Some(local_id), Some(tab)) = (local_id, mux.get_tab(tab_id)) {
            if let Some(local_pane) = mux.get_pane(local_id) {
                tab.set_active_pane(&local_pane);
            }
        }
    }

    /// Destroy local panes whose tmux id no longer appears in any window's last
    /// applied layout. Runs at a quiescent point so that a pane moved between
    /// windows (its source removal observed before the destination insertion)
    /// keeps its identity instead of being destroyed and recreated.
    pub fn prune_dead_panes(&self) {
        let mux = Mux::get();
        let live: HashSet<TmuxPaneId> = self
            .gui_tabs
            .lock()
            .values()
            .flat_map(|t| t.panes.iter().copied())
            .collect();

        let dead: Vec<(TmuxPaneId, PaneId)> = {
            let pane_map = self.remote_panes.lock();
            pane_map
                .iter()
                .filter(|(pid, _)| !live.contains(*pid))
                .map(|(pid, p)| (*pid, p.lock().local_pane_id))
                .collect()
        };

        for (tmux_pid, local_id) in dead {
            // Release the fake child so the pane's reader can exit.
            if let Some(ref_pane) = self.remote_panes.lock().get(&tmux_pid) {
                let p = ref_pane.lock();
                let (lock, condvar) = &*p.active_lock;
                *lock.lock() = true;
                condvar.notify_all();
            }
            self.remote_panes.lock().remove(&tmux_pid);
            self.backlog.lock().remove(&tmux_pid);
            mux.remove_pane(local_id);
            log::info!("tmux pruned dead pane %{tmux_pid} (local {local_id})");
        }
    }

    fn sync_pane_state(&self, panes: &[PaneItem]) -> anyhow::Result<()> {
        let Some(current_session) = *self.tmux_session.lock() else {
            return Ok(());
        };
        let mux = Mux::get();

        for pane in panes.iter() {
            if pane.session_id != current_session
                || !self.check_pane_attached(pane.window_id, pane.pane_id)
            {
                continue;
            }

            // We now have the cursor information, fix the cursor position
            let pane_map = self.remote_panes.lock();
            let local_pane = match pane_map.get(&pane.pane_id) {
                Some(p) => {
                    let local_pane_id = p.lock().local_pane_id;
                    mux.get_pane(local_pane_id)
                }
                None => None,
            };

            if let Some(local_pane) = local_pane {
                let c = local_pane.get_cursor_position();
                // no capture, output case
                if (c.x + c.y as usize) == 0 {
                    if let Some(text) = self.backlog.lock().remove(&pane.pane_id) {
                        if let Some(ref_pane) = pane_map.get(&pane.pane_id) {
                            let mut ref_pane = ref_pane.lock();
                            if let Err(err) = ref_pane.output_write.write_all(&text) {
                                log::error!("Failed to write tmux data to output: {:#}", err);
                            }
                        }
                    }
                } else {
                    // we have capture, so remove the backlog
                    let _ = self.backlog.lock().remove(&pane.pane_id);
                    if (pane.cursor_x + pane.cursor_y) != 0 {
                        self.set_pane_cursor_position(
                            &local_pane,
                            pane.cursor_x as usize,
                            pane.cursor_y as usize,
                        );
                    }
                }
                if pane.pane_active {
                    let tab_id = {
                        let mut gui_tabs = self.gui_tabs.lock();
                        let Some(local_tab) = gui_tabs.get_mut(&pane.window_id) else {
                            anyhow::bail!("invalid tmux window id {}", pane.window_id);
                        };
                        local_tab.active_pane = Some(pane.pane_id);
                        local_tab.tab_id
                    };

                    match mux.get_tab(tab_id) {
                        Some(tab) => {
                            tab.set_active_pane(&local_pane);
                            mux.notify(MuxNotification::PaneFocused(local_pane.pane_id()));
                        }
                        None => {}
                    }
                }
            }

            log::info!("new pane synced, id: {}", pane.pane_id);
        }

        Ok(())
    }

    fn sync_window_state(&self, windows: &[WindowItem], new_window: bool) -> anyhow::Result<()> {
        let Some(current_session) = *self.tmux_session.lock() else {
            return Ok(());
        };
        let mux = Mux::get();
        self.create_gui_window();

        for window in windows.iter() {
            if window.session_id != current_session {
                continue;
            }

            let size = TerminalSize {
                rows: window.window_height as usize,
                cols: window.window_width as usize,
                pixel_width: 0,
                pixel_height: 0,
                dpi: 0,
            };

            let tree = match parse_layout_tree(&window.window_layout) {
                Ok(t) => t,
                Err(err) => {
                    log::error!(
                        "invalid window layout {:?}: {:#}",
                        window.window_layout,
                        err
                    );
                    continue;
                }
            };

            // Create the local tab if this window is new to us.
            let is_new_tab = !self.check_window_attached(window.window_id);
            if is_new_tab {
                let tab = Arc::new(Tab::new(&size));
                tab.set_title(&window.window_name);
                mux.add_tab_no_panes(&tab);
                self.gui_tabs.lock().insert(
                    window.window_id,
                    TmuxTab {
                        tab_id: tab.tab_id(),
                        tmux_window_id: window.window_id,
                        panes: HashSet::new(),
                        active_pane: None,
                        history_limit: window.history_limit,
                    },
                );
            } else {
                let tab_id = self.gui_tabs.lock().get(&window.window_id).map(|t| t.tab_id);
                if let Some(tab) = tab_id.and_then(|id| mux.get_tab(id)) {
                    tab.set_title(&window.window_name);
                }
                if let Some(t) = self.gui_tabs.lock().get_mut(&window.window_id) {
                    t.history_limit = window.history_limit;
                }
            }

            // Zoom state from window flags; the zoomed pane is the sole leaf of
            // the visible layout.
            let zoomed_pane = if window.window_raw_flags.contains('Z') {
                parse_layout_tree(&window.window_visible_layout)
                    .ok()
                    .and_then(|v| match v {
                        TmuxLayoutNode::Leaf { pane_id, .. } => Some(pane_id),
                        _ => None,
                    })
            } else {
                None
            };

            let active = self
                .gui_tabs
                .lock()
                .get(&window.window_id)
                .and_then(|t| t.active_pane);

            // Capture existing content only for the initial attach enumeration;
            // brand-new windows get their content via the live %output stream.
            let capture = !new_window;
            self.apply_layout(window.window_id, &tree, active, zoomed_pane, capture)?;

            // Link the (now populated) tab into the gui window.
            if is_new_tab {
                let tab_id = self.gui_tabs.lock().get(&window.window_id).map(|t| t.tab_id);
                if let Some(tab) = tab_id.and_then(|id| mux.get_tab(id)) {
                    let mut gui_window = self.gui_window.lock();
                    if let Some(gui_window_id) = gui_window.as_mut() {
                        mux.add_tab_to_window(&tab, **gui_window_id)?;
                        gui_window_id.notify();
                    }
                }
            }

            // Backfill cursor + active pane via list-panes (the layout string
            // carries neither). Keep the active window last so focus settles
            // there.
            if !window.window_active {
                self.cmd_queue.lock().push_back(Box::new(ListAllPanes {
                    window_id: window.window_id,
                }));
            }
        }

        if let Some(window) = windows.iter().find(|w| w.window_active) {
            self.cmd_queue.lock().push_back(Box::new(ListAllPanes {
                window_id: window.window_id,
            }));
        }

        if *self.attach_state.lock() == AttachState::Init {
            self.cmd_queue.lock().push_back(Box::new(AttachDone));
        }

        TmuxDomainState::schedule_send_next_command(self.domain_id);

        Ok(())
    }

    pub fn subscribe_notification(&self) {
        let mux = Mux::get();
        let domain_id = self.domain_id;
        mux.subscribe(move |n| {
            promise::spawn::spawn_into_main_thread(async move {
                let mux = Mux::get();
                let domain = match mux.get_domain(domain_id) {
                    Some(d) => d,
                    None => return,
                };
                let tmux_domain = match domain.downcast_ref::<TmuxDomain>() {
                    Some(t) => t,
                    None => return,
                };

                if *tmux_domain.inner.attach_state.lock() == AttachState::Init {
                    return;
                }

                match n {
                    MuxNotification::PaneFocused(pane_id) => {
                        let tmux_pane_id = match tmux_domain
                            .inner
                            .remote_panes
                            .lock()
                            .iter()
                            .find(|(_, p)| p.lock().local_pane_id == pane_id)
                        {
                            Some((_, p)) => Some(p.lock().pane_id),
                            None => None,
                        };

                        if let Some(pane_id) = tmux_pane_id {
                            tmux_domain
                                .inner
                                .cmd_queue
                                .lock()
                                .push_back(Box::new(SelectPane { pane_id: pane_id }));
                            TmuxDomainState::schedule_send_next_command(domain_id);
                        }
                    }
                    MuxNotification::WindowInvalidated(window_id) => {
                        if let Some(window) = mux.get_window(window_id) {
                            let Some(tab) = window.get_active() else {
                                return;
                            };
                            let tmux_window_id = match tmux_domain
                                .inner
                                .gui_tabs
                                .lock()
                                .iter()
                                .find(|(_, t)| t.tab_id == tab.tab_id())
                            {
                                Some((_, t)) => Some(t.tmux_window_id),
                                None => None,
                            };
                            if let Some(window_id) = tmux_window_id {
                                tmux_domain.inner.cmd_queue.lock().push_back(Box::new(
                                    SelectWindow {
                                        window_id: window_id,
                                    },
                                ));
                                TmuxDomainState::schedule_send_next_command(domain_id);
                            }
                        }
                    }
                    _ => {}
                }
            })
            .detach();
            true
        });
    }
}

fn parse_sigil_number(text: &str) -> anyhow::Result<u64> {
    let num = text
        .get(1..)
        .ok_or_else(|| anyhow!("wrong prefixed id"))?
        .parse()?;

    Ok(num)
}

/// list-panes for a single window, used purely to backfill cursor positions and
/// the active pane (which the layout string does not carry). Structure comes
/// from `%layout-change` / `apply_layout`.
#[derive(Debug)]
pub(crate) struct ListAllPanes {
    pub window_id: TmuxWindowId,
}

impl TmuxCommand for ListAllPanes {
    fn get_command(&self, _domain_id: DomainId) -> String {
        format!(
            "list-panes -F '#{{session_id}} #{{window_id}} #{{pane_id}} \
            #{{pane_index}} #{{cursor_x}} #{{cursor_y}} #{{pane_width}} #{{pane_height}} \
            #{{pane_left}} #{{pane_top}} #{{pane_active}}' -t @{}\n",
            self.window_id
        )
    }

    fn process_result(&self, domain_id: DomainId, result: &Guarded) -> anyhow::Result<()> {
        if result.error {
            let error = format!("list-pane in domain={domain_id} failed: {result:#?}");
            log::error!("{error}");
            anyhow::bail!("{error}");
        }
        let mut items = vec![];
        for line in result.output.split('\n') {
            if line.is_empty() {
                continue;
            }
            let mut fields = line.split(' ');
            // These ids all have various sigils such as `$`, `%`, `@`,
            // so skip those prior to parsing them
            let session_id =
                parse_sigil_number(fields.next().ok_or_else(|| anyhow!("missing session_id"))?)?;
            let window_id =
                parse_sigil_number(fields.next().ok_or_else(|| anyhow!("missing window_id"))?)?;
            let pane_id =
                parse_sigil_number(fields.next().ok_or_else(|| anyhow!("missing pane_id"))?)?;
            let _pane_index = fields
                .next()
                .ok_or_else(|| anyhow!("missing pane_index"))?
                .parse()?;
            let cursor_x = fields
                .next()
                .ok_or_else(|| anyhow!("missing cursor_x"))?
                .parse()?;
            let cursor_y = fields
                .next()
                .ok_or_else(|| anyhow!("missing cursor_y"))?
                .parse()?;
            let pane_width = fields
                .next()
                .ok_or_else(|| anyhow!("missing pane_width"))?
                .parse()?;
            let pane_height = fields
                .next()
                .ok_or_else(|| anyhow!("missing pane_height"))?
                .parse()?;
            let pane_left = fields
                .next()
                .ok_or_else(|| anyhow!("missing pane_left"))?
                .parse()?;
            let pane_top = fields
                .next()
                .ok_or_else(|| anyhow!("missing pane_top"))?
                .parse()?;
            let pane_active = fields
                .next()
                .ok_or_else(|| anyhow!("missing pane_active"))?
                .parse::<usize>()?;

            let pane_active = pane_active == 1;

            items.push(PaneItem {
                session_id,
                window_id,
                pane_id,
                _pane_index,
                cursor_x,
                cursor_y,
                pane_width,
                pane_height,
                pane_left,
                pane_top,
                pane_active,
            });
        }

        log::debug!("panes in domain_id {}: {:?}", domain_id, items);
        let mux = Mux::get();
        if let Some(domain) = mux.get_domain(domain_id) {
            if let Some(tmux_domain) = domain.downcast_ref::<TmuxDomain>() {
                return tmux_domain.inner.sync_pane_state(&items);
            }
        }
        anyhow::bail!("Tmux domain lost");
    }
}

#[derive(Debug)]
pub(crate) struct ListAllWindows {
    pub session_id: TmuxSessionId,
    pub window_id: Option<TmuxWindowId>,
}

impl TmuxCommand for ListAllWindows {
    fn get_command(&self, _domain_id: DomainId) -> String {
        format!(
            "list-windows -F \
                '#{{session_id}} #{{window_id}} \
                #{{window_width}} #{{window_height}} \
                #{{window_active}} \
                #{{window_name}} \
                #{{window_layout}} \
                #{{window_visible_layout}} \
                #{{window_raw_flags}} \
                #{{history_limit}}' -t ${}\n",
            self.session_id
        )
    }

    fn process_result(&self, domain_id: DomainId, result: &Guarded) -> anyhow::Result<()> {
        if result.error {
            let error = format!("list-window in domain={domain_id} failed: {result:#?}");
            log::error!("{error}");
            anyhow::bail!("{error}");
        }
        let mut items = vec![];

        for line in result.output.split('\n') {
            if line.is_empty() {
                continue;
            }
            let mut fields = line.split(' ');
            let session_id =
                parse_sigil_number(fields.next().ok_or_else(|| anyhow!("missing session_id"))?)?;
            let window_id =
                parse_sigil_number(fields.next().ok_or_else(|| anyhow!("missing window_id"))?)?;
            let window_width = fields
                .next()
                .ok_or_else(|| anyhow!("missing window_width"))?
                .parse()?;
            let window_height = fields
                .next()
                .ok_or_else(|| anyhow!("missing window_height"))?
                .parse()?;
            let window_active = fields
                .next()
                .ok_or_else(|| anyhow!("missing window_active"))?
                .parse::<usize>()?;

            let window_name = fields
                .next()
                .ok_or_else(|| anyhow!("missing window_name"))?;

            let window_layout = fields
                .next()
                .ok_or_else(|| anyhow!("missing window_layout"))?;

            let window_visible_layout = fields
                .next()
                .ok_or_else(|| anyhow!("missing window_visible_layout"))?;

            // window_raw_flags may legitimately be empty (no flags); split(' ')
            // still yields an empty token between the surrounding spaces.
            let window_raw_flags = fields
                .next()
                .ok_or_else(|| anyhow!("missing window_raw_flags"))?;

            let history_limit = fields
                .next()
                .ok_or_else(|| anyhow!("missing history_limit"))?
                .parse::<isize>()?;

            let window_active = window_active == 1;

            if let Some(x) = self.window_id {
                if x != window_id {
                    continue;
                }
            }

            items.push(WindowItem {
                session_id,
                window_id,
                window_width,
                window_height,
                window_active,
                window_name: window_name.to_string(),
                window_layout: window_layout.to_string(),
                window_visible_layout: window_visible_layout.to_string(),
                window_raw_flags: window_raw_flags.to_string(),
                history_limit,
            });
        }

        log::debug!("layout in domain_id {}: {:#?}", domain_id, items);
        let mux = Mux::get();
        if let Some(domain) = mux.get_domain(domain_id) {
            if let Some(tmux_domain) = domain.downcast_ref::<TmuxDomain>() {
                let new_window = if let Some(_x) = self.window_id {
                    true
                } else {
                    false
                };
                return tmux_domain.inner.sync_window_state(&items, new_window);
            }
        }
        anyhow::bail!("Tmux domain lost");
    }
}

#[derive(Debug)]
pub(crate) struct Resize {
    pub pane_id: TmuxPaneId,
    pub size: PtySize,
}

impl TmuxCommand for Resize {
    fn get_command(&self, domain_id: DomainId) -> String {
        let mux = Mux::get();
        let domain = match mux.get_domain(domain_id) {
            Some(d) => d,
            None => return "".to_string(),
        };
        let tmux_domain = match domain.downcast_ref::<TmuxDomain>() {
            Some(t) => t,
            None => return "".to_string(),
        };

        // Not in stable state for now, don't do resizing, otherwise it will cause tmux output
        // unexpected content.
        if *tmux_domain.inner.attach_state.lock() == AttachState::Init {
            return "".to_string();
        }

        let pane_map = tmux_domain.inner.remote_panes.lock();
        {
            let mut pane = match pane_map.get(&self.pane_id) {
                Some(x) => x.lock(),
                None => return "".to_string(),
            };

            if pane.pane_width == self.size.cols as u64 && pane.pane_height == self.size.rows as u64
            {
                return "".to_string();
            } else {
                pane.pane_width = self.size.cols as u64;
                pane.pane_height = self.size.rows as u64;
            }
        }

        let tmux_window_id = match pane_map.get(&self.pane_id) {
            Some(x) => x.lock().window_id,
            None => return "".to_string(),
        };

        let gui_tabs = tmux_domain.inner.gui_tabs.lock();
        let local_tab = match gui_tabs.get(&tmux_window_id) {
            Some(t) => t,
            None => return "".to_string(),
        };

        let size = match mux.get_tab(local_tab.tab_id) {
            Some(x) => x.get_size(),
            None => return "".to_string(),
        };

        let support_commands = tmux_domain.inner.support_commands.lock();

        if let Some(_x) = support_commands.get("resize-window") {
            format!(
                "resize-window -x {} -y {} -t @{}\nresize-pane -x {} -y {} -t %{}\n",
                size.cols, size.rows, tmux_window_id, self.size.cols, self.size.rows, self.pane_id
            )
        } else if let Some(x) = support_commands.get("refresh-client") {
            if x.contains("-C XxY") {
                format!(
                    "refresh-client -C {}x{}\nresize-pane -x {} -y {} -t %{}\n",
                    size.cols, size.rows, self.size.cols, self.size.rows, self.pane_id
                )
            } else {
                format!(
                    "refresh-client -C {},{}\nresize-pane -x {} -y {} -t %{}\n",
                    size.cols, size.rows, self.size.cols, self.size.rows, self.pane_id
                )
            }
        } else {
            log::info!("The tmux version is not supported");
            return "".to_string();
        }
    }

    fn process_result(&self, domain_id: DomainId, result: &Guarded) -> anyhow::Result<()> {
        if result.error {
            let error = format!("resize-pane in domain={domain_id} failed: {result:#?}");
            log::error!("{error}");
            anyhow::bail!("{error}");
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct CapturePane {
    pane_id: TmuxPaneId,
    history_limit: isize,
}

impl TmuxCommand for CapturePane {
    fn get_command(&self, _domain_id: DomainId) -> String {
        format!(
            "capture-pane -p -t %{} -e -C -S {}\n",
            self.pane_id,
            self.history_limit * -1
        )
    }

    fn process_result(&self, domain_id: DomainId, result: &Guarded) -> anyhow::Result<()> {
        if result.error {
            let error = format!("capture-pane in domain={domain_id} failed: {result:#?}");
            log::error!("{error}");
            anyhow::bail!("{error}");
        }
        let mux = Mux::get();
        let domain = match mux.get_domain(domain_id) {
            Some(d) => d,
            None => anyhow::bail!("Tmux domain lost"),
        };
        let tmux_domain = match domain.downcast_ref::<TmuxDomain>() {
            Some(t) => t,
            None => anyhow::bail!("Tmux domain lost"),
        };

        let unescaped = termwiz::tmux_cc::unvis(&result.output).context("unescape pane content")?;
        // capturep contents returned from guarded lines which always contain a tailing '\n'
        let unescaped = &unescaped[0..unescaped.len().saturating_sub(1)].replace("\n", "\r\n");

        let pane_map = tmux_domain.inner.remote_panes.lock();
        if let Some(pane) = pane_map.get(&self.pane_id) {
            let mut pane = pane.lock();
            if let Some(p) = mux.get_pane(pane.local_pane_id) {
                tmux_domain.inner.set_pane_cursor_position(&p, 0, 0);
            }

            pane.output_write
                .write_all(unescaped.as_bytes())
                .context("writing capture pane result to output")?;
        }

        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct SendKeys {
    pub keys: Vec<u8>,
    pub pane: TmuxPaneId,
}
impl TmuxCommand for SendKeys {
    fn get_command(&self, _domain_id: DomainId) -> String {
        let mut s = String::new();
        for &byte in self.keys.iter() {
            write!(&mut s, "0x{:X} ", byte).expect("unable to write key");
        }
        format!("send-keys -H -t %{} {}\r", self.pane, s)
    }

    fn process_result(&self, domain_id: DomainId, result: &Guarded) -> anyhow::Result<()> {
        if result.error {
            let error = format!("send-key in domain={domain_id} failed: {result:#?}");
            log::error!("{error}");
            anyhow::bail!("{error}");
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct NewWindow;
impl TmuxCommand for NewWindow {
    fn get_command(&self, _domain_id: DomainId) -> String {
        "new-window\n".to_owned()
    }

    fn process_result(&self, domain_id: DomainId, result: &Guarded) -> anyhow::Result<()> {
        if result.error {
            let error = format!("new-window in domain={domain_id} failed: {result:#?}");
            log::error!("{error}");
            anyhow::bail!("{error}");
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct ListCommands;
impl TmuxCommand for ListCommands {
    fn get_command(&self, _domain_id: DomainId) -> String {
        "list-commands\n".to_owned()
    }

    fn process_result(&self, domain_id: DomainId, result: &Guarded) -> anyhow::Result<()> {
        if result.error {
            let error = format!("list-command in domain={domain_id} failed: {result:#?}");
            log::error!("{error}");
            anyhow::bail!("{error}");
        }
        let mux = Mux::get();
        let domain = match mux.get_domain(domain_id) {
            Some(d) => d,
            None => anyhow::bail!("Tmux domain lost"),
        };
        let tmux_domain = match domain.downcast_ref::<TmuxDomain>() {
            Some(t) => t,
            None => anyhow::bail!("Tmux domain lost"),
        };

        let mut support_commands = tmux_domain.inner.support_commands.lock();

        for line in result.output.split('\n') {
            if line.is_empty() {
                continue;
            }
            let v: Vec<&str> = line.split(' ').collect();
            support_commands.insert(v[0].to_string(), line.to_string());
        }

        let mut cmd_queue = tmux_domain.inner.cmd_queue.as_ref().lock();
        if let Some(session) = *tmux_domain.inner.tmux_session.lock() {
            cmd_queue.push_back(Box::new(ListAllWindows {
                session_id: session,
                window_id: None,
            }));
            TmuxDomainState::schedule_send_next_command(domain_id);
        }

        Ok(())
    }
}

/// Register tmux control-mode format subscriptions via `refresh-client -B`.
///
/// All subscriptions are registered in a single `refresh-client` invocation
/// (multiple `-B` flags) so this remains one command / one response in the
/// queue. Each `-B` argument is `name:what:format`; the format is single-quoted
/// so tmux's command parser passes it through literally (refresh-client expands
/// it itself, per subscription) rather than expanding `#{...}` at parse time.
#[derive(Debug)]
pub(crate) struct Subscribe {
    pub subs: Vec<config::TmuxFormatSubscription>,
}

impl TmuxCommand for Subscribe {
    fn get_command(&self, _domain_id: DomainId) -> String {
        let mut cmd = String::from("refresh-client");
        for sub in &self.subs {
            let arg = format!("{}:{}:{}", sub.name, sub.target.tmux_type(), sub.format);
            // tmux treats everything inside single quotes literally; represent
            // an embedded single quote with the close/escape/reopen idiom.
            let arg = arg.replace('\'', "'\\''");
            let _ = write!(cmd, " -B '{arg}'");
        }
        cmd.push('\n');
        cmd
    }

    fn process_result(&self, domain_id: DomainId, result: &Guarded) -> anyhow::Result<()> {
        if result.error {
            // tmux < 3.2, or a format string tmux rejected. Degrade gracefully:
            // log and keep the control-mode session running.
            log::warn!(
                "tmux format subscriptions in domain={domain_id} were rejected \
                 (requires tmux >= 3.2): {result:#?}"
            );
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct SplitPane {
    pub pane_id: TmuxPaneId,
    pub direction: SplitDirection,
}

impl TmuxCommand for SplitPane {
    fn get_command(&self, _domain_id: DomainId) -> String {
        if self.direction == SplitDirection::Horizontal {
            format!("split-window -h -t %{}\n", self.pane_id)
        } else {
            format!("split-window -v -t %{}\n", self.pane_id)
        }
    }

    fn process_result(&self, domain_id: DomainId, result: &Guarded) -> anyhow::Result<()> {
        if result.error {
            let error = format!("split-window in domain={domain_id} failed: {result:#?}");
            log::error!("{error}");
            anyhow::bail!("{error}");
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct SelectWindow {
    pub window_id: TmuxWindowId,
}

impl TmuxCommand for SelectWindow {
    fn get_command(&self, _domain_id: DomainId) -> String {
        format!("select-window -t @{}\n", self.window_id)
    }

    fn process_result(&self, domain_id: DomainId, result: &Guarded) -> anyhow::Result<()> {
        if result.error {
            let error = format!("select-window in domain={domain_id} failed: {result:#?}");
            log::error!("{error}");
            anyhow::bail!("{error}");
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct SelectPane {
    pub pane_id: TmuxPaneId,
}

impl TmuxCommand for SelectPane {
    fn get_command(&self, _domain_id: DomainId) -> String {
        format!("select-pane -t %{}\n", self.pane_id)
    }

    fn process_result(&self, domain_id: DomainId, result: &Guarded) -> anyhow::Result<()> {
        if result.error {
            let error = format!("select-pane in domain={domain_id} failed: {result:#?}");
            log::error!("{error}");
            anyhow::bail!("{error}");
        }
        Ok(())
    }
}

/// Inject an arbitrary tmux command line into the -CC control stream. Used by
/// the `TmuxSendCommand` key assignment so that WezTerm can trigger tmux key
/// bindings (which are otherwise bypassed in control mode) via e.g.
/// `send-keys -K <chord>`. Fire-and-forget: the `%begin/%end` guard is still
/// consumed by `process_result`, but errors are only logged, never bubbled up.
#[derive(Debug)]
pub(crate) struct RawCommand {
    pub command: String,
}

impl TmuxCommand for RawCommand {
    fn get_command(&self, _domain_id: DomainId) -> String {
        if self.command.ends_with('\n') {
            self.command.clone()
        } else {
            format!("{}\n", self.command)
        }
    }

    fn process_result(&self, domain_id: DomainId, result: &Guarded) -> anyhow::Result<()> {
        if result.error {
            log::warn!(
                "RawCommand {:?} in domain={domain_id} returned an error: {result:#?}",
                self.command
            );
        }
        Ok(())
    }
}

// This is a dummy command which indicates the attaching is done, it prevents the tmux output
// the unexpected and unnecessary content when syncing with back end in attaching stage.
#[derive(Debug)]
pub(crate) struct AttachDone;
impl TmuxCommand for AttachDone {
    fn get_command(&self, _domain_id: DomainId) -> String {
        // The command doesn't matter, just give a legal simple command to let process_result called.
        "list-session\n".to_string()
    }

    fn process_result(&self, domain_id: DomainId, result: &Guarded) -> anyhow::Result<()> {
        if result.error {
            let error = format!("list-session in domain={domain_id} failed: {result:#?}");
            log::error!("{error}");
            anyhow::bail!("{error}");
        }
        let mux = Mux::get();
        let domain = match mux.get_domain(domain_id) {
            Some(d) => d,
            None => anyhow::bail!("Tmux domain lost"),
        };
        let tmux_domain = match domain.downcast_ref::<TmuxDomain>() {
            Some(t) => t,
            None => anyhow::bail!("Tmux domain lost"),
        };

        *tmux_domain.inner.attach_state.lock() = AttachState::Done;

        // Now that the attach is complete and all panes are mapped, register
        // tmux format subscriptions (if this tmux supports them). Subscribing
        // here ensures the initial values land on already-attached panes.
        // tmux's `%*` / `@*` targets also cover panes/windows created later, so
        // a single registration per attach is sufficient.
        let supports_subscribe = tmux_domain
            .inner
            .support_commands
            .lock()
            .get("refresh-client")
            .map_or(false, |usage| usage.contains("-B"));
        if supports_subscribe {
            let subs = config::configuration().tmux_format_subscriptions.clone();
            if !subs.is_empty() {
                tmux_domain
                    .inner
                    .cmd_queue
                    .as_ref()
                    .lock()
                    .push_back(Box::new(Subscribe { subs }));
                TmuxDomainState::schedule_send_next_command(domain_id);
            }
        } else {
            log::info!(
                "tmux in domain={domain_id} does not support format subscriptions \
                 (refresh-client -B); skipping"
            );
        }
        Ok(())
    }
}
