use crate::activity::Activity;
use crate::domain::{alloc_domain_id, Domain, DomainId, DomainState, SplitSource};
use crate::pane::{Pane, PaneId};
use crate::tab::{SplitRequest, Tab, TabId};
use crate::tmux_commands::{
    ListAllWindows, ListCommands, NewWindow, RawCommand, SplitPane, TmuxCommand
};
use crate::window::WindowId;
use crate::{Mux, MuxWindowBuilder};
use async_trait::async_trait;
use filedescriptor::FileDescriptor;
use parking_lot::{Condvar, Mutex};
use portable_pty::CommandBuilder;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use termwiz::tmux_cc::*;
use wezterm_term::TerminalSize;

#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub enum AttachState {
    Init,
    Done,
}

#[derive(PartialEq, Eq, Debug, Copy, Clone)]
enum State {
    WaitForInitialGuard,
    Idle,
    WaitingForResponse,
    Exit,
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct TmuxRemotePane {
    // members for local
    pub local_pane_id: PaneId,
    pub output_write: FileDescriptor,
    pub active_lock: Arc<(Mutex<bool>, Condvar)>,
    // members sync with remote
    pub session_id: TmuxSessionId,
    pub window_id: TmuxWindowId,
    pub pane_id: TmuxPaneId,
    pub cursor_x: u64,
    pub cursor_y: u64,
    pub pane_width: u64,
    pub pane_height: u64,
    pub pane_left: u64,
    pub pane_top: u64,
}

pub(crate) type RefTmuxRemotePane = Arc<Mutex<TmuxRemotePane>>;

/// As a remote TmuxTab, keeping the TmuxPanes ID
/// within the remote tab.
#[allow(dead_code)]
pub(crate) struct TmuxTab {
    pub tab_id: TabId, // local tab ID
    pub tmux_window_id: TmuxWindowId,
    /// tmux panes currently present in this window, per the last applied layout.
    pub panes: HashSet<TmuxPaneId>,
    /// The tmux pane id that is active in this window, tracked from
    /// `%window-pane-changed` and list-panes; used to mark the active pane when
    /// (re)building the local tree.
    pub active_pane: Option<TmuxPaneId>,
    /// The window's scrollback limit, used when capturing newly discovered panes.
    pub history_limit: isize,
}

pub(crate) type TmuxCmdQueue = VecDeque<Box<dyn TmuxCommand>>;
pub(crate) struct TmuxDomainState {
    pub pane_id: PaneId,     // ID of the original pane
    pub domain_id: DomainId, // ID of TmuxDomain
    state: Mutex<State>,
    pub cmd_queue: Arc<Mutex<TmuxCmdQueue>>,
    pub gui_window: Mutex<Option<MuxWindowBuilder>>,
    pub gui_tabs: Mutex<HashMap<TmuxWindowId, TmuxTab>>,
    pub remote_panes: Mutex<HashMap<TmuxPaneId, RefTmuxRemotePane>>,
    pub tmux_session: Mutex<Option<TmuxSessionId>>,
    pub support_commands: Mutex<HashMap<String, String>>,
    pub attach_state: Mutex<AttachState>,
    /// Promises awaiting the local pane that results from a user-initiated
    /// split; resolved (FIFO) with the local `PaneId` once the split's
    /// `%layout-change` creates the pane.
    pub(crate) pending_splits: Mutex<VecDeque<promise::Promise<PaneId>>>,
    pub backlog: Mutex<HashMap<TmuxPaneId, Vec<u8>>>,
    /// Set when a global prune of dead panes has been scheduled but not yet run;
    /// coalesces multiple requests into a single quiescent prune.
    pub(crate) prune_pending: AtomicBool,
}

pub struct TmuxDomain {
    pub(crate) inner: Arc<TmuxDomainState>,
}

/// Resolve the `TmuxDomainState` for `domain_id` and run `f` with it. Used to
/// hop event handling onto the main thread, where mux mutations are safe.
fn with_tmux_domain<F: FnOnce(&Arc<TmuxDomainState>)>(domain_id: DomainId, f: F) {
    let mux = Mux::get();
    if let Some(domain) = mux.get_domain(domain_id) {
        if let Some(tmux_domain) = domain.downcast_ref::<TmuxDomain>() {
            f(&tmux_domain.inner);
        }
    }
}

impl TmuxDomainState {
    pub fn advance(&self, events: Box<Vec<Event>>) {
        for event in events.iter() {
            let state = *self.state.lock();
            log::debug!("tmux: {:?} in state {:?}", event, state);
            match event {
                // Tmux generic events
                Event::Guarded(response) => match state {
                    State::WaitForInitialGuard => {
                        *self.state.lock() = State::Idle;
                    }
                    State::WaitingForResponse => {
                        let mut cmd_queue = self.cmd_queue.as_ref().lock();
                        if let Some(cmd) = cmd_queue.pop_front() {
                            let domain_id = self.domain_id;
                            *self.state.lock() = State::Idle;
                            let resp = response.clone();
                            promise::spawn::spawn_into_main_thread(async move {
                                if let Err(err) = cmd.process_result(domain_id, &resp) {
                                    log::error!("Tmux processing command result error: {}", err);
                                }
                            })
                            .detach();
                        }
                    }
                    State::Idle => {}
                    State::Exit => {}
                },

                // Tmux specific events
                Event::ConfigError { error } => {
                    // tmux config file error, not our fault, just log it and go
                    log::warn!("tmux configuration error: {error}");
                }
                Event::Exit { reason: _ } => {
                    *self.state.lock() = State::Exit;
                    let mut pane_map = self.remote_panes.lock();
                    for (_, v) in pane_map.iter_mut() {
                        let remote_pane = v.lock();
                        let (lock, condvar) = &*remote_pane.active_lock;
                        let mut released = lock.lock();
                        *released = true;
                        condvar.notify_all();
                    }
                    let mut cmd_queue = self.cmd_queue.as_ref().lock();
                    cmd_queue.clear();

                    // Force to quit the tmux mode
                    let pane_id = self.pane_id;
                    promise::spawn::spawn_into_main_thread_with_low_priority(async move {
                        if let Some(x) = Mux::get().get_pane(pane_id) {
                            let _ = write!(x.writer(), "\n\n");
                        }
                    })
                    .detach();

                    return;
                }
                Event::LayoutChange {
                    window,
                    layout,
                    visible_layout,
                    raw_flags,
                } => {
                    // The remote layout is the source of truth for this window's
                    // structure. Rebuild the local tab atomically on the main
                    // thread (it mutates the mux).
                    let domain_id = self.domain_id;
                    let window = *window;
                    let layout = layout.clone();
                    let visible_layout = visible_layout.clone();
                    let raw_flags = raw_flags.clone();
                    promise::spawn::spawn_into_main_thread(async move {
                        with_tmux_domain(domain_id, |inner| {
                            if let Err(err) = inner.handle_layout_change(
                                window,
                                &layout,
                                visible_layout.as_deref(),
                                raw_flags.as_deref(),
                            ) {
                                log::error!("tmux layout-change error: {:#}", err);
                            }
                            inner.maybe_schedule_send();
                        });
                    })
                    .detach();
                }
                Event::Output { pane, text } => {
                    let pane_map = self.remote_panes.lock();
                    if let Some(ref_pane) = pane_map.get(pane) {
                        let mut tmux_pane = ref_pane.lock();
                        if let Err(err) = tmux_pane.output_write.write_all(text) {
                            log::error!("Failed to write tmux data to output: {:#}", err);
                        }
                    } else {
                        // the output may come early then pane is ready, in this case we
                        // backlog it
                        self.backlog.lock().insert(*pane, text.to_vec());
                        log::debug!("Tmux pane {} havn't been attached", pane);
                    }
                }
                Event::SessionChanged { session, name: _ } => {
                    *self.tmux_session.lock() = Some(*session);
                    let mut cmd_queue = self.cmd_queue.as_ref().lock();
                    cmd_queue.push_back(Box::new(ListCommands));

                    self.subscribe_notification();
                    log::info!("tmux session changed:{}", session);
                }
                Event::WindowAdd { window } => {
                    // Only handle the new tab, the first empty window handled by sync_window_state
                    if !self.gui_window.lock().is_none() {
                        if let Some(session) = *self.tmux_session.lock() {
                            let mut cmd_queue = self.cmd_queue.as_ref().lock();
                            cmd_queue.push_back(Box::new(ListAllWindows {
                                session_id: session,
                                window_id: Some(*window),
                            }));
                            log::info!("tmux window add: {}:{}", session, window);
                        }
                    }
                }
                Event::WindowClose { window } => {
                    let _ = self.remove_detached_window(*window);
                    self.schedule_prune();
                }
                Event::WindowPaneChanged { window, pane } => {
                    // Track the active pane for this window. The split-completion
                    // promise is resolved later, when the corresponding
                    // %layout-change actually creates the local pane.
                    let domain_id = self.domain_id;
                    let window = *window;
                    let pane = *pane;
                    promise::spawn::spawn_into_main_thread(async move {
                        with_tmux_domain(domain_id, |inner| {
                            inner.set_active_tmux_pane(window, pane);
                        });
                    })
                    .detach();
                }
                Event::WindowRenamed { window, name } => {
                    let gui_tabs = self.gui_tabs.lock();
                    if let Some(x) = gui_tabs.get(&window) {
                        let mux = Mux::get();
                        if let Some(tab) = mux.get_tab(x.tab_id) {
                            tab.set_title(&format!("{}", name));
                        }
                    }
                }
                Event::UnlinkedWindowClose { window } => {
                    let _ = self.remove_detached_window(*window);
                    self.schedule_prune();
                }
                _ => {}
            }
        }

        // send pending commands to tmux
        let cmd_queue = self.cmd_queue.as_ref().lock();
        if *self.state.lock() == State::Idle && !cmd_queue.is_empty() {
            TmuxDomainState::schedule_send_next_command(self.domain_id);
        }
    }

    /// send next command at the front of cmd_queue.
    /// must be called inside main thread
    fn send_next_command(&self) {
        if *self.state.lock() != State::Idle {
            return;
        }
        let mut cmd_queue = self.cmd_queue.as_ref().lock();
        while let Some(first) = cmd_queue.front() {
            let cmd = first.get_command(self.domain_id);
            if cmd.is_empty() {
                cmd_queue.pop_front();
                continue;
            }
            log::debug!("sending cmd {:?}", cmd);
            let mux = Mux::get();
            if let Some(pane) = mux.get_pane(self.pane_id) {
                let mut writer = pane.writer();
                let _ = write!(writer, "{}", cmd);
            }
            *self.state.lock() = State::WaitingForResponse;
            break;
        }
    }

    /// schedule a `send_next_command` into main thread
    pub fn schedule_send_next_command(domain_id: usize) {
        promise::spawn::spawn_into_main_thread(async move {
            let mux = Mux::get();
            if let Some(domain) = mux.get_domain(domain_id) {
                if let Some(tmux_domain) = domain.downcast_ref::<TmuxDomain>() {
                    tmux_domain.send_next_command();
                }
            }
        })
        .detach();
    }

    /// create a standalone window for tmux tabs
    pub fn create_gui_window(&self) {
        if self.gui_window.lock().is_none() {
            let mux = Mux::get();
            let window_builder =
                if let Some((_domain, window_id, _tab)) = mux.resolve_pane_id(self.pane_id) {
                    MuxWindowBuilder {
                        window_id,
                        activity: Some(Activity::new()),
                        notified: false,
                    }
                } else {
                    mux.new_empty_window(
                        None, /* TODO: pass session here */
                        None, /* position */
                    )
                };

            log::info!("Tmux create window id {}", window_builder.window_id);
            {
                let mut window_id = self.gui_window.lock();
                *window_id = Some(window_builder); // keep the builder so it won't be purged
            }
        };
    }

    /// create a tmux window
    pub fn create_tmux_window(&self) {
        let mut cmd_queue = self.cmd_queue.as_ref().lock();
        cmd_queue.push_back(Box::new(NewWindow));
        TmuxDomainState::schedule_send_next_command(self.domain_id);
    }

    /// split the tmux pane
    pub fn split_tmux_pane(
        &self,
        _tab: TabId,
        pane_id: PaneId,
        split_request: SplitRequest,
    ) -> anyhow::Result<()> {
        let tmux_pane_id = self
            .remote_panes
            .lock()
            .iter()
            .find(|(_, ref_pane)| ref_pane.lock().local_pane_id == pane_id)
            .map(|p| p.1.lock().pane_id);

        if let Some(id) = tmux_pane_id {
            let mut cmd_queue = self.cmd_queue.as_ref().lock();
            cmd_queue.push_back(Box::new(SplitPane {
                pane_id: id,
                direction: split_request.direction,
            }));
            TmuxDomainState::schedule_send_next_command(self.domain_id);
            return Ok(());
        } else {
            anyhow::bail!("Could not find the tmux pane peer for local pane: {pane_id}");
        }
    }

    /// True when there are no in-flight commands: the FSM is Idle and the
    /// command queue is empty. Used to defer the global prune until all the
    /// consequences of a compound operation (e.g. a cross-window move that
    /// emits the source and destination layout changes separately) have been
    /// applied.
    pub(crate) fn is_quiescent(&self) -> bool {
        *self.state.lock() == State::Idle && self.cmd_queue.lock().is_empty()
    }

    /// Schedule sending the next queued command if we are idle and have work.
    pub(crate) fn maybe_schedule_send(&self) {
        if *self.state.lock() == State::Idle && !self.cmd_queue.lock().is_empty() {
            TmuxDomainState::schedule_send_next_command(self.domain_id);
        }
    }

    /// Request a global prune of panes that no longer appear in any window.
    /// Coalesced: at most one prune is in flight, and it only runs once the
    /// domain is quiescent so that a pane moved between windows (whose source
    /// and destination `%layout-change` arrive as separate events) is never
    /// destroyed mid-move.
    pub(crate) fn schedule_prune(&self) {
        if self.prune_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        TmuxDomainState::schedule_prune_task(self.domain_id);
    }

    fn schedule_prune_task(domain_id: DomainId) {
        promise::spawn::spawn_into_main_thread_with_low_priority(async move {
            with_tmux_domain(domain_id, |inner| {
                if !inner.is_quiescent() {
                    // Not settled yet; retry after pending work drains.
                    TmuxDomainState::schedule_prune_task(domain_id);
                    return;
                }
                inner.prune_pending.store(false, Ordering::SeqCst);
                inner.prune_dead_panes();
            });
        })
        .detach();
    }
}

impl TmuxDomain {
    pub fn new(pane_id: PaneId) -> Self {
        let domain_id = alloc_domain_id();
        let cmd_queue = VecDeque::new();
        let inner = Arc::new(TmuxDomainState {
            domain_id,
            pane_id,
            // parser,
            state: Mutex::new(State::WaitForInitialGuard),
            cmd_queue: Arc::new(Mutex::new(cmd_queue)),
            gui_window: Mutex::new(None),
            gui_tabs: Mutex::new(HashMap::default()),
            remote_panes: Mutex::new(HashMap::default()),
            tmux_session: Mutex::new(None),
            support_commands: Mutex::new(HashMap::default()),
            attach_state: Mutex::new(AttachState::Init),
            pending_splits: Mutex::new(VecDeque::default()),
            backlog: Mutex::new(HashMap::default()),
            prune_pending: AtomicBool::new(false),
        });

        Self { inner }
    }

    fn send_next_command(&self) {
        self.inner.send_next_command();
    }

    /// Inject an arbitrary tmux command line into the -CC control stream.
    /// Routed through the existing `cmd_queue` so the `%begin/%end` guard
    /// emitted by tmux is consumed by the normal response machinery.
    pub fn send_raw_command(&self, command: String) {
        self.inner
            .cmd_queue
            .lock()
            .push_back(Box::new(RawCommand { command }));
        TmuxDomainState::schedule_send_next_command(self.inner.domain_id);
    }
}

#[async_trait(?Send)]
impl Domain for TmuxDomain {
    async fn spawn(
        &self,
        _size: TerminalSize,
        _command: Option<CommandBuilder>,
        _command_dir: Option<String>,
        _window: WindowId,
    ) -> anyhow::Result<Arc<Tab>> {
        self.inner.create_tmux_window();
        // This is intention, we would not return a Tab, since we don't have now!
        // We use create_tmux_window to create back end tmux window, then the
        // Tmux WindowAdd event will triage us to do the rest things.
        anyhow::bail!("Intention: we use tmux command to do so");
    }

    async fn split_pane(
        &self,
        _source: SplitSource,
        tab: TabId,
        pane_id: PaneId,
        split_request: SplitRequest,
    ) -> anyhow::Result<Arc<dyn Pane>> {
        let mut promise = promise::Promise::new();
        let future = promise
            .get_future()
            .ok_or_else(|| anyhow::anyhow!("failed to create split promise"))?;
        {
            let mut pending_splits = self.inner.pending_splits.lock();
            self.inner.split_tmux_pane(tab, pane_id, split_request)?;
            pending_splits.push_back(promise);
        }

        // The new pane is created by the %layout-change that tmux emits in
        // response to split-window; that handler resolves this promise with the
        // newly created local pane id.
        let local_pane_id = future
            .await
            .map_err(|_| anyhow::anyhow!("split-pane was cancelled"))?;
        Mux::get()
            .get_pane(local_pane_id)
            .ok_or_else(|| anyhow::anyhow!("split-pane: created pane vanished"))
    }

    async fn spawn_pane(
        &self,
        _size: TerminalSize,
        _command: Option<CommandBuilder>,
        _command_dir: Option<String>,
    ) -> anyhow::Result<Arc<dyn Pane>> {
        anyhow::bail!("Spawn_pane not yet implemented for TmuxDomain");
    }

    fn domain_id(&self) -> DomainId {
        self.inner.domain_id
    }

    fn domain_name(&self) -> &str {
        "tmux"
    }

    async fn attach(&self, _window_id: Option<crate::WindowId>) -> anyhow::Result<()> {
        Ok(())
    }

    fn detachable(&self) -> bool {
        false
    }

    fn detach(&self) -> anyhow::Result<()> {
        anyhow::bail!("detach not implemented for TmuxDomain");
    }

    fn state(&self) -> DomainState {
        DomainState::Attached
    }
}
