use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use crate::app_event::AppEvent;
use crate::app_event::ConnectorsSnapshot;
use crate::app_event_sender::AppEventSender;
use crate::app_server_session::AppServerSession;
use crate::app_server_session::HISTORY_ITEM_PAGE_LIMIT;
use crate::app_server_session::HISTORY_ITEM_SCAN_LIMIT;
use crate::bottom_pane::ChatComposer;
use crate::bottom_pane::InputResult;
use crate::clipboard_paste::normalize_pasted_search_query;
use crate::color::blend;
use crate::color::is_light;
use crate::dashboard::DashboardGroupMode;
use crate::dashboard::DashboardHistory;
use crate::dashboard::DashboardStatus;
use crate::file_search::FileSearchManager;
use crate::git_action_directives::parse_assistant_markdown;
use crate::inline_visualization::InlineVisualizationContext;
use crate::key_hint::KeyBindingListExt;
use crate::key_hint::is_plain_text_key_event;
use crate::keymap::ListAction;
use crate::keymap::ListKeymap;
use crate::keymap::PagerKeymap;
use crate::keymap::RuntimeChordKeymap;
use crate::keymap::RuntimeKeymap;
use crate::legacy_core::config::Config;
use crate::legacy_core::config::edit::ConfigEditsBuilder;
use crate::markdown::append_markdown;
use crate::pager_overlay::Overlay;
use crate::session_resume::resolve_session_thread_id;
use crate::status::format_directory_display;
use crate::terminal_palette::best_color;
use crate::terminal_palette::default_bg;
use crate::text_formatting::truncate_text;
use crate::thread_transcript::RawReasoningVisibility;
use crate::thread_transcript::TranscriptCells;
use crate::thread_transcript::load_session_transcript;
use crate::tui::FrameRequester;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_lines;
use chrono::DateTime;
use chrono::Utc;
use codex_app_server_client::AppServerEvent;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::SkillMetadata;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadArchiveParams;
use codex_app_server_protocol::ThreadArchiveResponse;
use codex_app_server_protocol::ThreadHistoryMode;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadListCwdFilter;
use codex_app_server_protocol::ThreadListParams;
use codex_app_server_protocol::ThreadSortKey;
use codex_app_server_protocol::ThreadUnarchiveParams;
use codex_app_server_protocol::ThreadUnarchiveResponse;
use codex_config::types::SessionPickerViewMode;
use codex_plugin::PluginCapabilitySummary;
use codex_protocol::ThreadId;
use codex_utils_path as path_utils;
use color_eyre::eyre::Result;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::layout::Size;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Styled as _;
use ratatui::style::Stylize as _;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Widget;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::warn;
use unicode_width::UnicodeWidthStr;
use uuid::Uuid;

mod agents_dashboard;
mod archive;
mod page_loading;

use agents_dashboard::DashboardComposer;
use page_loading::PageLoadMode;
use page_loading::PaginationState;

const PAGE_SIZE: usize = 25;
const LOAD_NEAR_THRESHOLD: usize = 5;
const SESSION_META_INDENT_WIDTH: usize = 2;
const SESSION_META_DATE_WIDTH: usize = 12;
const SESSION_META_FIELD_GAP_WIDTH: usize = 2;
const SESSION_META_MIN_CWD_WIDTH: usize = 30;
const SESSION_META_MAX_CWD_WIDTH: usize = 72;
const DASHBOARD_STATUS_COLUMN_WIDTH: usize = 14;
const SESSION_META_BRANCH_ICON: &str = "";
const SESSION_META_CWD_ICON: &str = "⌁";
const FOOTER_COMPACT_BREAKPOINT: u16 = 120;
const FOOTER_HINT_LEFT_PADDING: usize = 1;
const FOOTER_HINT_GAP: usize = 3;
const PICKER_LIST_HORIZONTAL_MARGIN: u16 = 2;
const DASHBOARD_LIST_HORIZONTAL_MARGIN: u16 = 1;

#[derive(Debug, Clone)]
pub struct SessionTarget {
    pub path: Option<PathBuf>,
    pub thread_id: ThreadId,
}

impl SessionTarget {
    pub fn display_label(&self) -> String {
        self.path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| format!("thread {}", self.thread_id))
    }
}

#[derive(Debug, Clone)]
pub enum SessionSelection {
    StartFresh,
    ReconnectDashboard(DashboardResumeState),
    StartFreshIn {
        cwd: PathBuf,
        user_message: crate::chatwidget::UserMessage,
    },
    Resume(SessionTarget),
    ResumeInSessionCwd(SessionTarget),
    Fork(SessionTarget),
    Exit,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DashboardResumeState {
    draft: String,
    group_mode: DashboardGroupMode,
    selected_thread_id: Option<ThreadId>,
    selected_cwd: Option<PathBuf>,
}

impl DashboardResumeState {
    pub(crate) fn with_draft(draft: Option<String>) -> Self {
        Self {
            draft: draft.unwrap_or_default(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum SessionPickerAction {
    Resume,
    Fork,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPickerLaunchContext {
    Startup,
    AgentsDashboard,
    ExistingSession { current_thread_id: Option<ThreadId> },
}

struct SessionPickerLaunch {
    show_all: bool,
    include_non_interactive: bool,
    context: SessionPickerLaunchContext,
    dashboard_resume_state: Option<DashboardResumeState>,
}

impl SessionPickerAction {
    fn title(self) -> &'static str {
        match self {
            SessionPickerAction::Resume => "Resume a previous session",
            SessionPickerAction::Fork => "Fork a previous session",
        }
    }

    fn action_label(self) -> &'static str {
        match self {
            SessionPickerAction::Resume => "resume",
            SessionPickerAction::Fork => "fork",
        }
    }

    fn selection(self, path: Option<PathBuf>, thread_id: ThreadId) -> SessionSelection {
        let target_session = SessionTarget { path, thread_id };
        match self {
            SessionPickerAction::Resume => SessionSelection::Resume(target_session),
            SessionPickerAction::Fork => SessionSelection::Fork(target_session),
        }
    }
}

#[derive(Clone)]
struct PageLoadRequest {
    cursor: Option<PageCursor>,
    request_token: usize,
    search_token: Option<usize>,
    mode: PageLoadMode,
    cwd_filter: Option<PathBuf>,
    status: SessionStatus,
    provider_filter: ProviderFilter,
    sort_key: ThreadSortKey,
}

enum PickerLoadRequest {
    Page(PageLoadRequest),
    Preview {
        thread_id: ThreadId,
    },
    Transcript {
        thread_id: ThreadId,
        cancellation: oneshot::Receiver<()>,
    },
    Archive {
        thread_id: ThreadId,
    },
    Unarchive {
        thread_id: ThreadId,
    },
    DashboardComposerInventory {
        cwd: PathBuf,
    },
}

#[derive(Clone)]
enum ProviderFilter {
    Any,
    MatchDefault(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionFilterMode {
    Cwd,
    All,
}

impl SessionFilterMode {
    fn from_show_all(show_all: bool, filter_cwd: Option<&Path>) -> Self {
        if show_all || filter_cwd.is_none() {
            Self::All
        } else {
            Self::Cwd
        }
    }

    fn toggle(self, filter_cwd: Option<&Path>) -> Self {
        match self {
            Self::Cwd => Self::All,
            Self::All if filter_cwd.is_some() => Self::Cwd,
            Self::All => Self::All,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionStatus {
    Active,
    Archived,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolbarControl {
    Filter,
    Status,
    Sort,
}

impl ToolbarControl {
    fn previous(self, action: SessionPickerAction) -> Self {
        match self {
            Self::Filter => Self::Sort,
            Self::Status => Self::Filter,
            Self::Sort if matches!(action, SessionPickerAction::Resume) => Self::Status,
            Self::Sort => Self::Filter,
        }
    }

    fn next(self, action: SessionPickerAction) -> Self {
        match self {
            Self::Filter if matches!(action, SessionPickerAction::Resume) => Self::Status,
            Self::Filter | Self::Status => Self::Sort,
            Self::Sort => Self::Filter,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionListDensity {
    Comfortable,
    Dense,
}

impl SessionListDensity {
    fn toggle(self) -> Self {
        match self {
            Self::Comfortable => Self::Dense,
            Self::Dense => Self::Comfortable,
        }
    }
}

impl From<SessionPickerViewMode> for SessionListDensity {
    fn from(mode: SessionPickerViewMode) -> Self {
        match mode {
            SessionPickerViewMode::Comfortable => Self::Comfortable,
            SessionPickerViewMode::Dense => Self::Dense,
        }
    }
}

impl From<SessionListDensity> for SessionPickerViewMode {
    fn from(density: SessionListDensity) -> Self {
        match density {
            SessionListDensity::Comfortable => Self::Comfortable,
            SessionListDensity::Dense => Self::Dense,
        }
    }
}

type PickerLoader = Arc<dyn Fn(PickerLoadRequest) + Send + Sync>;
enum BackgroundEvent {
    Page {
        request_token: usize,
        search_token: Option<usize>,
        page: std::io::Result<PickerPage>,
    },
    Preview {
        thread_id: ThreadId,
        preview: std::io::Result<Vec<TranscriptPreviewLine>>,
    },
    Transcript {
        thread_id: ThreadId,
        transcript: std::io::Result<TranscriptCells>,
    },
    Archive {
        thread_id: ThreadId,
        result: std::io::Result<()>,
    },
    Unarchive {
        thread_id: ThreadId,
        result: std::io::Result<SessionTarget>,
    },
    AppServer(AppServerEvent),
    DashboardComposerInventory {
        cwd: PathBuf,
        skills: Option<Vec<SkillMetadata>>,
        plugins: Option<Vec<PluginCapabilitySummary>>,
        connectors: Option<ConnectorsSnapshot>,
    },
}

#[derive(Clone)]
enum PageCursor {
    AppServer(String),
}

struct PickerPage {
    rows: Vec<Row>,
    dashboard_system_errors: HashSet<ThreadId>,
    next_cursor: Option<PageCursor>,
    num_scanned_files: usize,
    reached_scan_cap: bool,
}

#[derive(Clone)]
struct SessionPickerViewPersistence {
    codex_home: PathBuf,
}

struct SessionPickerRunOptions {
    show_all: bool,
    filter_cwd: Option<PathBuf>,
    local_filter_cwd: Option<PathBuf>,
    action: SessionPickerAction,
    launch_context: SessionPickerLaunchContext,
    provider_filter: ProviderFilter,
    initial_density: SessionListDensity,
    view_persistence: Option<SessionPickerViewPersistence>,
    pager_keymap: PagerKeymap,
    list_keymap: ListKeymap,
    initial_page_mode: PageLoadMode,
    chord_keymap: Arc<RuntimeChordKeymap>,
    dashboard_keymap: Option<RuntimeKeymap>,
    dashboard_fallback_cwd: Option<PathBuf>,
    dashboard_disable_paste_burst: bool,
    dashboard_history: Option<DashboardHistory>,
    dashboard_resume_state: Option<DashboardResumeState>,
}

/// Interactive session picker that lists app-server threads with simple search,
/// lazy transcript previews, and pagination.
///
/// Sessions render as compact multi-line records with stable metadata first and
/// the conversation preview last. Users can focus the toolbar controls with
/// Tab, change the focused control with the arrow keys, and expand the
/// selected session with Ctrl+E to load recent transcript context on demand.
///
/// Sessions are loaded on-demand via cursor-based pagination. The backend
/// `thread/list` API returns pages ordered by the selected sort key, and the
/// picker deduplicates across pages to handle overlapping windows when new
/// sessions appear during pagination.
///
/// Filtering happens in two layers:
/// 1. Provider, source, and eligible working-directory filtering at the backend.
/// 2. Typed search filtering over loaded rows in the picker.
pub async fn run_resume_picker_with_app_server(
    tui: &mut Tui,
    config: &Config,
    show_all: bool,
    include_non_interactive: bool,
    app_server: AppServerSession,
) -> Result<SessionSelection> {
    let archive_request_handle = app_server.request_handle();
    run_resume_picker_with_launch_context(
        tui,
        config,
        app_server,
        archive_request_handle,
        SessionPickerLaunch {
            show_all,
            include_non_interactive,
            context: SessionPickerLaunchContext::Startup,
            dashboard_resume_state: None,
        },
    )
    .await
}

pub(crate) async fn run_agents_dashboard_with_app_server(
    tui: &mut Tui,
    config: &Config,
    app_server: AppServerSession,
    resume_state: DashboardResumeState,
) -> Result<SessionSelection> {
    let archive_request_handle = app_server.request_handle();
    run_resume_picker_with_launch_context(
        tui,
        config,
        app_server,
        archive_request_handle,
        SessionPickerLaunch {
            show_all: true,
            include_non_interactive: false,
            context: SessionPickerLaunchContext::AgentsDashboard,
            dashboard_resume_state: Some(resume_state),
        },
    )
    .await
}

pub async fn run_resume_picker_from_existing_session_with_app_server(
    tui: &mut Tui,
    config: &Config,
    show_all: bool,
    include_non_interactive: bool,
    app_server: AppServerSession,
    archive_request_handle: AppServerRequestHandle,
    current_thread_id: Option<ThreadId>,
) -> Result<SessionSelection> {
    run_resume_picker_with_launch_context(
        tui,
        config,
        app_server,
        archive_request_handle,
        SessionPickerLaunch {
            show_all,
            include_non_interactive,
            context: SessionPickerLaunchContext::ExistingSession { current_thread_id },
            dashboard_resume_state: None,
        },
    )
    .await
}

async fn run_resume_picker_with_launch_context(
    tui: &mut Tui,
    config: &Config,
    app_server: AppServerSession,
    archive_request_handle: AppServerRequestHandle,
    launch: SessionPickerLaunch,
) -> Result<SessionSelection> {
    let SessionPickerLaunch {
        show_all,
        include_non_interactive,
        context: launch_context,
        dashboard_resume_state,
    } = launch;
    let (bg_tx, bg_rx) = mpsc::unbounded_channel();
    let uses_remote_workspace = app_server.uses_remote_workspace();
    let cwd_filter = picker_cwd_filter(
        config.cwd.as_path(),
        /*show_all*/ false,
        uses_remote_workspace,
        app_server.remote_cwd_override(),
    );
    let local_filter_cwd = local_picker_cwd_filter(&cwd_filter, uses_remote_workspace);
    let provider_filter = picker_provider_filter(config, uses_remote_workspace);
    let runtime_keymap = picker_runtime_keymap(config)?;
    let dashboard_keymap = matches!(launch_context, SessionPickerLaunchContext::AgentsDashboard)
        .then(|| runtime_keymap.clone());
    let options = SessionPickerRunOptions {
        show_all,
        filter_cwd: cwd_filter,
        local_filter_cwd,
        action: SessionPickerAction::Resume,
        launch_context,
        provider_filter,
        initial_density: SessionListDensity::from(config.tui_session_picker_view),
        view_persistence: Some(SessionPickerViewPersistence {
            codex_home: config.codex_home.to_path_buf(),
        }),
        pager_keymap: runtime_keymap.pager,
        list_keymap: runtime_keymap.list,
        initial_page_mode: if uses_remote_workspace {
            PageLoadMode::StoreDefault
        } else {
            PageLoadMode::StateDbOnly
        },
        chord_keymap: runtime_keymap.chords,
        dashboard_keymap,
        dashboard_fallback_cwd: matches!(
            launch_context,
            SessionPickerLaunchContext::AgentsDashboard
        )
        .then(|| config.cwd.to_path_buf()),
        dashboard_disable_paste_burst: config.disable_paste_burst,
        dashboard_history: Some(DashboardHistory::new(config)),
        dashboard_resume_state,
    };
    run_session_picker_with_loader(
        tui,
        options,
        spawn_app_server_page_loader(
            app_server,
            archive_request_handle,
            include_non_interactive,
            raw_reasoning_visibility(config),
            (!uses_remote_workspace).then(|| config.codex_home.to_path_buf()),
            matches!(launch_context, SessionPickerLaunchContext::AgentsDashboard),
            bg_tx,
        ),
        bg_rx,
    )
    .await
}

pub async fn run_fork_picker_with_app_server(
    tui: &mut Tui,
    config: &Config,
    show_all: bool,
    app_server: AppServerSession,
) -> Result<SessionSelection> {
    let archive_request_handle = app_server.request_handle();
    let (bg_tx, bg_rx) = mpsc::unbounded_channel();
    let uses_remote_workspace = app_server.uses_remote_workspace();
    let cwd_filter = picker_cwd_filter(
        config.cwd.as_path(),
        /*show_all*/ false,
        uses_remote_workspace,
        app_server.remote_cwd_override(),
    );
    let local_filter_cwd = local_picker_cwd_filter(&cwd_filter, uses_remote_workspace);
    let provider_filter = picker_provider_filter(config, uses_remote_workspace);
    let runtime_keymap = picker_runtime_keymap(config)?;
    let options = SessionPickerRunOptions {
        show_all,
        filter_cwd: cwd_filter,
        local_filter_cwd,
        action: SessionPickerAction::Fork,
        launch_context: SessionPickerLaunchContext::Startup,
        provider_filter,
        initial_density: SessionListDensity::from(config.tui_session_picker_view),
        view_persistence: Some(SessionPickerViewPersistence {
            codex_home: config.codex_home.to_path_buf(),
        }),
        pager_keymap: runtime_keymap.pager,
        list_keymap: runtime_keymap.list,
        initial_page_mode: if uses_remote_workspace {
            PageLoadMode::StoreDefault
        } else {
            PageLoadMode::StateDbOnly
        },
        chord_keymap: runtime_keymap.chords,
        dashboard_keymap: None,
        dashboard_fallback_cwd: None,
        dashboard_disable_paste_burst: false,
        dashboard_history: None,
        dashboard_resume_state: None,
    };
    run_session_picker_with_loader(
        tui,
        options,
        spawn_app_server_page_loader(
            app_server,
            archive_request_handle,
            /*include_non_interactive*/ false,
            raw_reasoning_visibility(config),
            (!uses_remote_workspace).then(|| config.codex_home.to_path_buf()),
            /*show_dashboard_status*/ false,
            bg_tx,
        ),
        bg_rx,
    )
    .await
}

async fn run_session_picker_with_loader(
    tui: &mut Tui,
    options: SessionPickerRunOptions,
    picker_loader: PickerLoader,
    bg_rx: mpsc::UnboundedReceiver<BackgroundEvent>,
) -> Result<SessionSelection> {
    let alt = AltScreenGuard::enter(tui);
    let (dashboard_app_event_tx, dashboard_app_event_rx) = mpsc::unbounded_channel();
    let dashboard_app_event_sender = AppEventSender::new(dashboard_app_event_tx.clone());
    let mut state = PickerState::new(
        alt.tui.frame_requester(),
        picker_loader,
        options.provider_filter,
        options.show_all,
        options.filter_cwd,
        options.action,
    );
    state.local_filter_cwd = options.local_filter_cwd;
    state.density = options.initial_density;
    state.view_persistence = options.view_persistence;
    state.pager_keymap = options.pager_keymap;
    state.list_keymap = options.list_keymap;
    state.chord_keymap = options.chord_keymap;
    state.launch_context = options.launch_context;
    state.initial_page_mode = options.initial_page_mode;
    let mut dashboard_app_events = UnboundedReceiverStream::new(dashboard_app_event_rx).fuse();
    let mut dashboard_file_search = options
        .dashboard_fallback_cwd
        .as_ref()
        .map(|cwd| FileSearchManager::new(cwd.clone(), dashboard_app_event_sender.clone()));
    if let (Some(cwd), Some(keymap)) = (
        options.dashboard_fallback_cwd.as_deref(),
        options.dashboard_keymap.as_ref(),
    ) {
        state.initialize_dashboard_composer(
            alt.tui.enhanced_keys_supported(),
            options.dashboard_disable_paste_burst,
            dashboard_app_event_sender.clone(),
            cwd,
            keymap,
        );
        if let Some(resume_state) = options.dashboard_resume_state.as_ref()
            && let Some(dashboard) = state.dashboard_composer.as_mut()
        {
            dashboard.composer.insert_str(&resume_state.draft);
            state.dashboard_group_mode = resume_state.group_mode;
            state.dashboard_restore_thread_id = resume_state.selected_thread_id;
            state.dashboard_restore_cwd = resume_state.selected_cwd.clone();
        }
        if let Some(history) = options.dashboard_history.as_ref()
            && let Some(dashboard) = state.dashboard_composer.as_mut()
        {
            history.initialize(&mut dashboard.composer).await;
        }
    }
    state.start_initial_load();
    state.request_frame();

    let mut tui_events = alt.tui.event_stream().fuse();
    let mut background_events = UnboundedReceiverStream::new(bg_rx).fuse();

    loop {
        tokio::select! {
            Some(ev) = tui_events.next() => {
                let screen_size = alt.tui.screen_size_for_event(&ev)?;
                let ev = if let TuiEvent::Key(key) = ev {
                    let Some(key) = state.route_key_chord(key) else {
                        continue;
                    };
                    TuiEvent::Key(key)
                } else {
                    ev
                };
                if state.overlay.is_some() {
                    state.handle_overlay_event(alt.tui, ev)?;
                    continue;
                }
                match ev {
                    TuiEvent::Key(key) => {
                        if matches!(key.kind, KeyEventKind::Release) {
                            continue;
                        }
                        if let Some(sel) = state.handle_key(key).await? {
                            return Ok(sel);
                        }
                    }
                    TuiEvent::Paste(pasted) => {
                        state.handle_paste(pasted);
                    }
                    TuiEvent::Draw | TuiEvent::Resume | TuiEvent::Resize(_) => {
                        if let Some(dashboard) = state.dashboard_composer.as_mut() {
                            if dashboard.composer.flush_paste_burst_if_due() {
                                state.request_frame();
                            } else if dashboard.composer.is_in_paste_burst() {
                                state.requester.schedule_frame_in(
                                    ChatComposer::recommended_paste_flush_delay(),
                                );
                            }
                        }
                        let list_width = list_viewport_width(screen_size.width, &state);
                        let list_height = usize::from(
                            screen_size
                                .height
                                .saturating_sub(4)
                                .saturating_sub(picker_bottom_height(
                                    &state,
                                    screen_size.width,
                                    screen_size.height,
                                )),
                        );
                        state.update_viewport(list_height, list_width);
                        state.ensure_minimum_rows_for_view(list_height);
                        draw_picker(alt.tui, &state, screen_size)?;
                        if state.note_transcript_loading_frame_drawn() {
                            state.open_pending_transcript_if_ready();
                        }
                    }
                }
            }
            Some(event) = background_events.next() => {
                if let Some(selection) = state.handle_background_event(event).await? {
                    return Ok(selection);
                }
            }
            Some(event) = dashboard_app_events.next(), if state.dashboard_composer.is_some() => {
                match event {
                    AppEvent::StartFileSearch(query) => {
                        if let Some(file_search) = dashboard_file_search.as_mut() {
                            let cwd = state.dashboard_project_cwd();
                            file_search.update_search_dir(cwd);
                            file_search.on_user_query(query);
                        }
                    }
                    AppEvent::FileSearchResult { query, matches } => {
                        if let Some(composer) = state.dashboard_composer.as_mut() {
                            composer.composer.on_file_search_result(query, matches);
                            state.request_frame();
                        }
                    }
                    event => {
                        if let (Some(history), Some(composer)) = (
                            options.dashboard_history.as_ref(),
                            state.dashboard_composer.as_mut(),
                        ) && history.handle_event(
                            event,
                            &dashboard_app_event_sender,
                            &mut composer.composer,
                        ) {
                            state.request_frame();
                        }
                    }
                }
            }
            else => break,
        }
    }

    // Fallback – treat as cancel/new
    Ok(SessionSelection::StartFresh)
}

fn raw_reasoning_visibility(config: &Config) -> RawReasoningVisibility {
    if config.show_raw_agent_reasoning {
        RawReasoningVisibility::Visible
    } else {
        RawReasoningVisibility::Hidden
    }
}

fn local_picker_cwd_filter(
    cwd_filter: &Option<PathBuf>,
    uses_remote_workspace: bool,
) -> Option<PathBuf> {
    if uses_remote_workspace {
        None
    } else {
        cwd_filter.clone()
    }
}

fn picker_provider_filter(config: &Config, uses_remote_workspace: bool) -> ProviderFilter {
    if uses_remote_workspace {
        ProviderFilter::Any
    } else {
        ProviderFilter::MatchDefault(config.model_provider_id.to_string())
    }
}

fn picker_runtime_keymap(config: &Config) -> Result<RuntimeKeymap> {
    RuntimeKeymap::from_config(&config.tui_keymap)
        .map_err(|err| color_eyre::eyre::eyre!("invalid keymap configuration: {err}"))
}

fn picker_cwd_filter(
    config_cwd: &Path,
    show_all: bool,
    uses_remote_workspace: bool,
    remote_cwd_override: Option<&Path>,
) -> Option<PathBuf> {
    if show_all {
        None
    } else if uses_remote_workspace {
        remote_cwd_override.map(Path::to_path_buf)
    } else {
        Some(config_cwd.to_path_buf())
    }
}

fn spawn_app_server_page_loader(
    app_server: AppServerSession,
    archive_request_handle: AppServerRequestHandle,
    include_non_interactive: bool,
    raw_reasoning_visibility: RawReasoningVisibility,
    codex_home: Option<PathBuf>,
    show_dashboard_status: bool,
    bg_tx: mpsc::UnboundedSender<BackgroundEvent>,
) -> PickerLoader {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel::<PickerLoadRequest>();

    tokio::spawn(async move {
        let mut app_server = app_server;
        loop {
            tokio::select! {
                request = request_rx.recv() => {
                    let Some(request) = request else {
                        break;
                    };
                    match request {
                PickerLoadRequest::Page(request) => {
                    let cursor = request.cursor.map(|PageCursor::AppServer(cursor)| cursor);
                    let params = thread_list_params(
                        cursor,
                        request.cwd_filter.as_deref(),
                        request.status,
                        request.provider_filter,
                        request.sort_key,
                        include_non_interactive,
                        matches!(request.mode, PageLoadMode::StateDbOnly),
                    );
                    let page =
                        load_app_server_page(&mut app_server, params, show_dashboard_status).await;
                    let _ = bg_tx.send(BackgroundEvent::Page {
                        request_token: request.request_token,
                        search_token: request.search_token,
                        page,
                    });
                }
                PickerLoadRequest::Preview { thread_id } => {
                    let preview =
                        load_transcript_preview(&mut app_server, thread_id, codex_home.as_deref())
                            .await;
                    let _ = bg_tx.send(BackgroundEvent::Preview { thread_id, preview });
                }
                PickerLoadRequest::Transcript {
                    thread_id,
                    cancellation,
                } => {
                    tokio::select! {
                        transcript = load_session_transcript(
                            &mut app_server,
                            thread_id,
                            raw_reasoning_visibility,
                            codex_home.as_deref(),
                        ) => {
                            let _ = bg_tx.send(BackgroundEvent::Transcript {
                                thread_id,
                                transcript,
                            });
                        }
                        _ = cancellation => {}
                    }
                }
                PickerLoadRequest::Archive { thread_id } => {
                    let result = archive_request_handle
                        .request_typed::<ThreadArchiveResponse>(ClientRequest::ThreadArchive {
                            request_id: RequestId::String(format!(
                                "resume-picker-archive-{}",
                                Uuid::new_v4()
                            )),
                            params: ThreadArchiveParams {
                                thread_id: thread_id.to_string(),
                            },
                        })
                        .await
                        .map(|_| ())
                        .map_err(std::io::Error::other);
                    let _ = bg_tx.send(BackgroundEvent::Archive { thread_id, result });
                }
                PickerLoadRequest::Unarchive { thread_id } => {
                    let result = archive_request_handle
                        .request_typed::<ThreadUnarchiveResponse>(ClientRequest::ThreadUnarchive {
                            request_id: RequestId::String(format!(
                                "resume-picker-unarchive-{}",
                                Uuid::new_v4()
                            )),
                            params: ThreadUnarchiveParams {
                                thread_id: thread_id.to_string(),
                            },
                        })
                        .await
                        .map(|response| SessionTarget {
                            path: response.thread.path,
                            thread_id,
                        })
                        .map_err(std::io::Error::other);
                    let _ = bg_tx.send(BackgroundEvent::Unarchive { thread_id, result });
                }
                PickerLoadRequest::DashboardComposerInventory { cwd } => {
                    let request_handle = app_server.request_handle();
                    let (skills, plugins, connectors) = tokio::join!(
                        crate::app::background_requests::fetch_skills_list(
                            request_handle.clone(),
                            cwd.clone(),
                        ),
                        crate::app::plugin_mentions::fetch_plugin_mentions(
                            request_handle.clone(),
                            cwd.clone(),
                        ),
                        crate::app::background_requests::fetch_connectors_list(
                            request_handle,
                            /*force_refetch*/ false,
                            /*thread_id*/ None,
                        ),
                    );
                    let skills = skills.ok().and_then(|response| {
                        response
                            .data
                            .into_iter()
                            .find(|entry| entry.cwd == cwd)
                            .map(|entry| {
                                entry.skills.into_iter().filter(|skill| skill.enabled).collect()
                            })
                    });
                    let _ = bg_tx.send(BackgroundEvent::DashboardComposerInventory {
                        cwd,
                        skills,
                        plugins: plugins.ok(),
                        connectors: connectors.ok(),
                    });
                }
                    }
                }
                event = app_server.next_event(), if show_dashboard_status => {
                    let Some(event) = event else {
                        break;
                    };
                    let _ = bg_tx.send(BackgroundEvent::AppServer(event));
                }
            }
        }
        if let Err(err) = app_server.shutdown().await {
            warn!(%err, "Failed to shut down app-server picker session");
        }
    });

    Arc::new(move |request: PickerLoadRequest| {
        let _ = request_tx.send(request);
    })
}

/// Returns the human-readable column header for the given sort key.
fn sort_key_label(sort_key: ThreadSortKey) -> &'static str {
    match sort_key {
        ThreadSortKey::CreatedAt => "Created",
        ThreadSortKey::UpdatedAt | ThreadSortKey::RecencyAt | ThreadSortKey::SectionPosition => {
            "Updated"
        }
    }
}

/// RAII guard that ensures we leave the alt-screen on scope exit.
struct AltScreenGuard<'a> {
    tui: &'a mut Tui,
}

impl<'a> AltScreenGuard<'a> {
    fn enter(tui: &'a mut Tui) -> Self {
        let _ = tui.enter_alt_screen();
        Self { tui }
    }
}

impl Drop for AltScreenGuard<'_> {
    fn drop(&mut self) {
        let _ = self.tui.leave_alt_screen();
    }
}

struct PickerState {
    requester: FrameRequester,
    relative_time_reference: Option<DateTime<Utc>>,
    pagination: PaginationState,
    all_rows: Vec<Row>,
    filtered_rows: Vec<Row>,
    seen_rows: HashSet<SeenRowKey>,
    selected: usize,
    scroll_top: usize,
    dashboard_scroll_offset: usize,
    pending_page_down_target: Option<usize>,
    frozen_footer_percent: Option<u8>,
    query: String,
    search_state: SearchState,
    next_request_token: usize,
    next_search_token: usize,
    picker_loader: PickerLoader,
    view_rows: Option<usize>,
    view_width: Option<u16>,
    provider_filter: ProviderFilter,
    filter_mode: SessionFilterMode,
    status: SessionStatus,
    filter_cwd: Option<PathBuf>,
    local_filter_cwd: Option<PathBuf>,
    toolbar_focus: ToolbarControl,
    density: SessionListDensity,
    launch_context: SessionPickerLaunchContext,
    dashboard_group_mode: DashboardGroupMode,
    view_persistence: Option<SessionPickerViewPersistence>,
    action: SessionPickerAction,
    sort_key: ThreadSortKey,
    inline_error: Option<String>,
    archive_state: archive::ArchiveState,
    expanded_thread_id: Option<ThreadId>,
    transcript_previews: HashMap<ThreadId, TranscriptPreviewState>,
    transcript_cells: HashMap<ThreadId, SessionTranscriptState>,
    pending_transcript_open: Option<ThreadId>,
    pending_transcript_cancellation: Option<oneshot::Sender<()>>,
    transcript_loading_frame_shown: bool,
    overlay: Option<Overlay>,
    pager_keymap: PagerKeymap,
    list_keymap: ListKeymap,
    initial_page_mode: PageLoadMode,
    chord_keymap: Arc<RuntimeChordKeymap>,
    chord_matcher: crate::keymap::KeyChordMatcher,
    dashboard_composer: Option<DashboardComposer>,
    dashboard_search_active: bool,
    dashboard_fallback_cwd: Option<PathBuf>,
    pending_dashboard_submission: Option<crate::chatwidget::UserMessage>,
    dashboard_system_errors: HashSet<ThreadId>,
    dashboard_inventory_cwd: Option<PathBuf>,
    dashboard_restore_thread_id: Option<ThreadId>,
    dashboard_restore_cwd: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug)]
enum SearchState {
    Idle,
    Active { token: usize },
}

#[derive(Clone)]
enum TranscriptPreviewState {
    Loading,
    Loaded(Vec<TranscriptPreviewLine>),
    Failed,
}

enum SessionTranscriptState {
    Loading,
    Loaded(TranscriptCells),
    Failed,
}

#[derive(Clone)]
pub(crate) struct TranscriptPreviewLine {
    speaker: TranscriptPreviewSpeaker,
    text: String,
}

#[derive(Clone, Copy)]
enum TranscriptPreviewSpeaker {
    User,
    Assistant,
}

enum LoadTrigger {
    Scroll,
    Search { token: usize },
}

async fn load_app_server_page(
    app_server: &mut AppServerSession,
    params: ThreadListParams,
    show_dashboard_status: bool,
) -> std::io::Result<PickerPage> {
    let response = app_server
        .thread_list(params)
        .await
        .map_err(std::io::Error::other)?;
    let num_scanned_files = response.data.len();

    let dashboard_system_errors = response
        .data
        .iter()
        .filter(|thread| {
            show_dashboard_status
                && dashboard_thread_is_root(thread)
                && matches!(
                    thread.status,
                    codex_app_server_protocol::ThreadStatus::SystemError
                )
        })
        .filter_map(|thread| ThreadId::from_string(&thread.id).ok())
        .collect();
    Ok(PickerPage {
        rows: response
            .data
            .into_iter()
            .filter(|thread| thread_visible_in_picker(thread, show_dashboard_status))
            .filter_map(|thread| row_from_app_server_thread(thread, show_dashboard_status))
            .collect(),
        dashboard_system_errors,
        next_cursor: response.next_cursor.map(PageCursor::AppServer),
        num_scanned_files,
        reached_scan_cap: false,
    })
}

fn dashboard_thread_is_root(thread: &Thread) -> bool {
    thread.parent_thread_id.is_none()
}

fn thread_visible_in_picker(thread: &Thread, show_dashboard_status: bool) -> bool {
    !show_dashboard_status || dashboard_thread_is_root(thread)
}

pub(crate) async fn load_transcript_preview(
    app_server: &mut AppServerSession,
    thread_id: ThreadId,
    codex_home: Option<&Path>,
) -> std::io::Result<Vec<TranscriptPreviewLine>> {
    const MAX_PREVIEW_LINES: usize = 6;

    let mut thread = app_server
        .thread_read(thread_id, /*include_turns*/ false)
        .await
        .map_err(std::io::Error::other)?;
    if thread.history_mode == ThreadHistoryMode::Legacy {
        app_server
            .hydrate_initial_thread_history(
                &mut thread,
                /*turn_cursor*/ None,
                /*item_cursor*/ None,
                /*config*/ None,
                crate::app_server_session::HistoryHydrationScope::Initial,
            )
            .await
            .map_err(std::io::Error::other)?;
    }
    let cwd = thread.cwd.as_path();
    let inline_visualization_context = codex_home.and_then(|codex_home| {
        ThreadId::from_string(&thread.id)
            .ok()
            .and_then(|thread_id| InlineVisualizationContext::new(codex_home, thread_id))
    });
    let mut lines = if thread.history_mode == ThreadHistoryMode::Paginated {
        let mut groups = Vec::new();
        let mut visible_lines = 0_usize;
        let mut scanned_items = 0_usize;
        let mut cursor = None;
        let mut seen_cursors = HashSet::new();
        loop {
            let page = app_server
                .thread_items_page(
                    thread_id,
                    /*turn_id*/ None,
                    cursor.clone(),
                    HISTORY_ITEM_PAGE_LIMIT,
                )
                .await
                .map_err(std::io::Error::other)?;
            scanned_items = scanned_items.saturating_add(page.data.len());
            for entry in page.data {
                let item_lines = transcript_preview_lines_for_item(
                    &entry.item,
                    cwd,
                    inline_visualization_context.as_ref(),
                );
                visible_lines = visible_lines.saturating_add(item_lines.len());
                if !item_lines.is_empty() {
                    groups.push(item_lines);
                }
                if visible_lines >= MAX_PREVIEW_LINES {
                    break;
                }
            }
            if visible_lines >= MAX_PREVIEW_LINES || scanned_items >= HISTORY_ITEM_SCAN_LIMIT {
                break;
            }
            let Some(next_cursor) = page
                .next_cursor
                .filter(|next| seen_cursors.insert(next.clone()))
            else {
                break;
            };
            cursor = Some(next_cursor);
        }
        groups.into_iter().rev().flatten().collect::<Vec<_>>()
    } else {
        thread
            .turns
            .iter()
            .flat_map(|turn| turn.items.iter())
            .flat_map(|item| {
                transcript_preview_lines_for_item(item, cwd, inline_visualization_context.as_ref())
            })
            .collect::<Vec<_>>()
    };
    if lines.len() > MAX_PREVIEW_LINES {
        lines.drain(..lines.len() - MAX_PREVIEW_LINES);
    }
    Ok(lines)
}

fn transcript_preview_lines_for_item(
    item: &ThreadItem,
    cwd: &Path,
    inline_visualization_context: Option<&InlineVisualizationContext>,
) -> Vec<TranscriptPreviewLine> {
    let line = match item {
        ThreadItem::UserMessage { content, .. } => TranscriptPreviewLine {
            speaker: TranscriptPreviewSpeaker::User,
            text: content
                .iter()
                .filter_map(|input| match input {
                    codex_app_server_protocol::UserInput::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
        },
        ThreadItem::AgentMessage { text, .. } => {
            let visible_markdown = parse_assistant_markdown(text, cwd).visible_markdown;
            let rewritten = crate::inline_visualization::rewrite_inline_visualizations(
                &visible_markdown,
                inline_visualization_context,
            );
            let mut text = rewritten.markdown.into_owned();
            for (placeholder, link) in &rewritten.trusted_file_links {
                text = text.replace(
                    &format!(
                        "{}  \n[{}]({placeholder})",
                        link.markdown_label, link.markdown_destination_label
                    ),
                    &format!("{}  \n{}", link.display_label, link.destination),
                );
            }
            TranscriptPreviewLine {
                speaker: TranscriptPreviewSpeaker::Assistant,
                text,
            }
        }
        _ => return Vec::new(),
    };

    line.text
        .lines()
        .filter(|text| !text.trim().is_empty())
        .map(|text| TranscriptPreviewLine {
            speaker: line.speaker,
            text: text.trim().to_string(),
        })
        .collect()
}

impl SearchState {
    fn active_token(&self) -> Option<usize> {
        match self {
            SearchState::Idle => None,
            SearchState::Active { token } => Some(*token),
        }
    }

    fn is_active(&self) -> bool {
        self.active_token().is_some()
    }
}

#[derive(Clone)]
struct Row {
    path: Option<PathBuf>,
    preview: String,
    thread_id: Option<ThreadId>,
    thread_name: Option<String>,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
    cwd: Option<PathBuf>,
    git_branch: Option<String>,
    dashboard_status: Option<DashboardStatus>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum SeenRowKey {
    Path(PathBuf),
    Thread(ThreadId),
}

impl Row {
    fn recency_timestamp(&self) -> i64 {
        self.updated_at
            .or(self.created_at)
            .map_or(0, |time| time.timestamp())
    }

    fn seen_key(&self) -> Option<SeenRowKey> {
        if let Some(path) = self.path.clone() {
            return Some(SeenRowKey::Path(path));
        }
        self.thread_id.map(SeenRowKey::Thread)
    }

    fn display_preview(&self) -> &str {
        self.thread_name.as_deref().unwrap_or(&self.preview)
    }

    fn matches_query(&self, query: &str) -> bool {
        if self.preview.to_lowercase().contains(query) {
            return true;
        }
        if let Some(thread_name) = self.thread_name.as_ref()
            && thread_name.to_lowercase().contains(query)
        {
            return true;
        }
        if self
            .thread_id
            .is_some_and(|thread_id| thread_id.to_string().to_lowercase().contains(query))
        {
            return true;
        }
        if self
            .git_branch
            .as_ref()
            .is_some_and(|branch| branch.to_lowercase().contains(query))
        {
            return true;
        }
        if self
            .cwd
            .as_ref()
            .is_some_and(|cwd| cwd.to_string_lossy().to_lowercase().contains(query))
        {
            return true;
        }
        false
    }
}

impl PickerState {
    fn new(
        requester: FrameRequester,
        picker_loader: PickerLoader,
        provider_filter: ProviderFilter,
        show_all: bool,
        filter_cwd: Option<PathBuf>,
        action: SessionPickerAction,
    ) -> Self {
        Self {
            requester,
            relative_time_reference: None,
            pagination: PaginationState::new(),
            all_rows: Vec::new(),
            filtered_rows: Vec::new(),
            seen_rows: HashSet::new(),
            selected: 0,
            scroll_top: 0,
            dashboard_scroll_offset: 0,
            pending_page_down_target: None,
            frozen_footer_percent: None,
            query: String::new(),
            search_state: SearchState::Idle,
            next_request_token: 0,
            next_search_token: 0,
            picker_loader,
            view_rows: None,
            view_width: None,
            provider_filter,
            filter_mode: SessionFilterMode::from_show_all(show_all, filter_cwd.as_deref()),
            status: SessionStatus::Active,
            local_filter_cwd: filter_cwd.clone(),
            filter_cwd,
            toolbar_focus: ToolbarControl::Filter,
            density: SessionListDensity::Comfortable,
            launch_context: SessionPickerLaunchContext::Startup,
            dashboard_group_mode: DashboardGroupMode::Project,
            view_persistence: None,
            action,
            sort_key: ThreadSortKey::UpdatedAt,
            inline_error: None,
            archive_state: archive::ArchiveState::default(),
            expanded_thread_id: None,
            transcript_previews: HashMap::new(),
            transcript_cells: HashMap::new(),
            pending_transcript_open: None,
            pending_transcript_cancellation: None,
            transcript_loading_frame_shown: false,
            overlay: None,
            pager_keymap: RuntimeKeymap::defaults().pager,
            list_keymap: RuntimeKeymap::defaults().list,
            initial_page_mode: PageLoadMode::StoreDefault,
            chord_keymap: Arc::default(),
            chord_matcher: crate::keymap::KeyChordMatcher::default(),
            dashboard_composer: None,
            dashboard_search_active: false,
            dashboard_fallback_cwd: None,
            pending_dashboard_submission: None,
            dashboard_system_errors: HashSet::new(),
            dashboard_inventory_cwd: None,
            dashboard_restore_thread_id: None,
            dashboard_restore_cwd: None,
        }
    }

    fn route_key_chord(&mut self, key: KeyEvent) -> Option<KeyEvent> {
        let context = if self.overlay.is_some() {
            crate::keymap::KeymapContext::Pager
        } else if !self.dashboard_search_active
            && self.dashboard_composer.as_ref().is_some_and(|dashboard| {
                !dashboard.composer.is_empty() || dashboard.composer.popup_active()
            })
        {
            crate::keymap::KeymapContext::Composer
        } else {
            crate::keymap::KeymapContext::List
        };
        match self.chord_matcher.advance(
            key,
            &self.chord_keymap,
            crate::keymap::KeymapContextSet::new(context),
            tokio::time::Instant::now(),
        ) {
            crate::keymap::KeyChordMatch::PassThrough => Some(key),
            crate::keymap::KeyChordMatch::Completed(dispatch_event) => Some(dispatch_event),
            crate::keymap::KeyChordMatch::Pending(_)
            | crate::keymap::KeyChordMatch::Cancelled
            | crate::keymap::KeyChordMatch::Ignored => None,
        }
    }

    fn request_frame(&self) {
        self.requester.schedule_frame();
    }

    fn is_transcript_loading(&self) -> bool {
        self.pending_transcript_open.is_some()
    }

    fn note_transcript_loading_frame_drawn(&mut self) -> bool {
        if self.pending_transcript_open.is_some() {
            self.transcript_loading_frame_shown = true;
            true
        } else {
            false
        }
    }

    fn open_pending_transcript_if_ready(&mut self) {
        if !self.transcript_loading_frame_shown {
            return;
        }
        let Some(thread_id) = self.pending_transcript_open else {
            return;
        };
        let Some(SessionTranscriptState::Loaded(cells)) = self.transcript_cells.get(&thread_id)
        else {
            return;
        };
        self.overlay = Some(Overlay::new_transcript(
            cells.clone(),
            self.pager_keymap.clone(),
        ));
        self.pending_transcript_open = None;
        self.transcript_loading_frame_shown = false;
        self.request_frame();
    }

    fn begin_transcript_loading(&mut self, thread_id: ThreadId) {
        self.pending_transcript_open = Some(thread_id);
        self.transcript_loading_frame_shown = false;
        self.request_frame();
    }

    fn handle_overlay_event(&mut self, tui: &mut Tui, event: TuiEvent) -> Result<()> {
        let Some(overlay) = &mut self.overlay else {
            return Ok(());
        };
        overlay.handle_event(tui, event)?;
        if overlay.is_done() {
            self.overlay = None;
            self.request_frame();
        }
        Ok(())
    }

    fn open_selected_transcript(&mut self) {
        let Some(row) = self.filtered_rows.get(self.selected) else {
            return;
        };
        let Some(thread_id) = row.thread_id else {
            self.inline_error = Some("No transcript available for this session".to_string());
            self.request_frame();
            return;
        };

        match self.transcript_cells.get(&thread_id) {
            Some(SessionTranscriptState::Loaded(_)) => {
                self.begin_transcript_loading(thread_id);
            }
            Some(SessionTranscriptState::Loading) => {
                self.begin_transcript_loading(thread_id);
            }
            Some(SessionTranscriptState::Failed) | None => {
                self.transcript_cells
                    .insert(thread_id, SessionTranscriptState::Loading);
                self.begin_transcript_loading(thread_id);
                let (cancellation_tx, cancellation) = oneshot::channel();
                self.pending_transcript_cancellation = Some(cancellation_tx);
                (self.picker_loader)(PickerLoadRequest::Transcript {
                    thread_id,
                    cancellation,
                });
            }
        }
    }

    fn handle_transcript_loading_key(&mut self, key: KeyEvent) -> Option<SessionSelection> {
        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => Some(SessionSelection::Exit),
            key if self.list_keymap.cancel.is_pressed(key) => {
                if let Some(thread_id) = self.pending_transcript_open.take()
                    && matches!(
                        self.transcript_cells.get(&thread_id),
                        Some(SessionTranscriptState::Loading)
                    )
                {
                    self.transcript_cells.remove(&thread_id);
                }
                if let Some(cancellation) = self.pending_transcript_cancellation.take() {
                    let _ = cancellation.send(());
                }
                self.transcript_loading_frame_shown = false;
                self.request_frame();
                None
            }
            _ => None,
        }
    }

    async fn handle_key(&mut self, mut key: KeyEvent) -> Result<Option<SessionSelection>> {
        self.inline_error = None;
        if self.is_transcript_loading() {
            return Ok(self.handle_transcript_loading_key(key));
        }
        if !self.list_keymap.page_down.is_pressed(key) {
            self.pending_page_down_target = None;
        }
        if self.is_agents_dashboard()
            && self.dashboard_composer.as_ref().is_some_and(|dashboard| {
                dashboard.composer.is_empty()
                    && !dashboard.composer.popup_active()
                    && self.list_keymap.move_right.is_pressed(key)
            })
        {
            key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        }
        // The session picker is always searchable, so plain text belongs to
        // the query first. Modified list bindings still route through the
        // runtime keymap below.
        let allow_plain_char_navigation = !is_plain_text_key_event(key);
        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(Some(SessionSelection::Exit));
            }
            KeyEvent {
                code: KeyCode::Char('f'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) && self.is_agents_dashboard() => {
                self.dashboard_search_active = !self.dashboard_search_active;
                if !self.dashboard_search_active {
                    self.clear_query_preserving_selection();
                }
                self.request_frame();
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } if self.dashboard_search_active => {
                self.dashboard_search_active = false;
                self.clear_query_preserving_selection();
            }
            KeyEvent {
                code: KeyCode::Char('g'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) && self.is_agents_dashboard() => {
                self.toggle_dashboard_group_mode();
            }
            KeyEvent {
                code: KeyCode::Up,
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) && self.is_agents_dashboard() => {
                self.move_dashboard_selection(/*down*/ false);
            }
            KeyEvent {
                code: KeyCode::Down,
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) && self.is_agents_dashboard() => {
                self.move_dashboard_selection(/*down*/ true);
            }
            _ if self.is_agents_dashboard()
                && !self.dashboard_search_active
                && self.handle_dashboard_composer_key(key) =>
            {
                if let Some(selection) = self.take_dashboard_submission() {
                    return Ok(Some(selection));
                }
            }
            _ if self.list_keymap.cancel.is_pressed(key) => {
                if self.query.is_empty() {
                    return Ok(Some(SessionSelection::StartFresh));
                }
                self.clear_query_preserving_selection();
            }
            KeyEvent {
                code: KeyCode::Char('t'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_selected_transcript();
            }
            KeyEvent {
                code: KeyCode::Char('e'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_selected_expansion();
            }
            KeyEvent {
                code: KeyCode::Char('\u{0014}'),
                modifiers: KeyModifiers::NONE,
                ..
            } /* ^T */ => {
                self.open_selected_transcript();
            }
            KeyEvent {
                code: KeyCode::Char('\u{0005}'),
                modifiers: KeyModifiers::NONE,
                ..
            } /* ^E */ => {
                self.toggle_selected_expansion();
            }
            KeyEvent {
                code: KeyCode::Char('o'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_density().await;
            }
            KeyEvent {
                code: KeyCode::Char('\u{000f}'),
                modifiers: KeyModifiers::NONE,
                ..
            } /* ^O */ => {
                self.toggle_density().await;
            }
            _ if self.list_keymap.accept.is_pressed(key)
                && !matches!(self.archive_state, archive::ArchiveState::Idle) => {}
            _ if self.list_keymap.accept.is_pressed(key) => {
                if self.handle_dashboard_command() {
                    return Ok(None);
                }
                if let Some(row) = self.filtered_rows.get(self.selected) {
                    let path = row.path.clone();
                    let thread_id = match row.thread_id {
                        Some(thread_id) => Some(thread_id),
                        None => match path.as_ref() {
                            Some(path) => {
                                resolve_session_thread_id(path.as_path(), /*id_str_if_uuid*/ None)
                                    .await
                            }
                            None => None,
                        },
                    };
                    if let Some(thread_id) = thread_id {
                        if self.status == SessionStatus::Archived {
                            self.request_unarchive(thread_id);
                            return Ok(None);
                        }
                        let selection = self.action.selection(path, thread_id);
                        return Ok(Some(match selection {
                            SessionSelection::Resume(target_session)
                                if self.is_agents_dashboard() =>
                            {
                                SessionSelection::ResumeInSessionCwd(target_session)
                            }
                            selection => selection,
                        }));
                    }
                    self.inline_error = Some(match path {
                        Some(path) => {
                            format!("Failed to read session metadata from {}", path.display())
                        }
                        None if self.is_agents_dashboard() => String::from(
                            "Type a prompt below to start an agent in the selected project",
                        ),
                        None => {
                            String::from("Failed to read session metadata from selected session")
                        }
                    });
                    self.request_frame();
                }
            }
            _ if allow_plain_char_navigation && self.list_keymap.move_up.is_pressed(key) => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.ensure_selected_visible();
                }
                self.request_frame();
            }
            _ if allow_plain_char_navigation && self.list_keymap.move_down.is_pressed(key) => {
                if self.selected + 1 < self.filtered_rows.len() {
                    self.selected += 1;
                    self.ensure_selected_visible();
                }
                self.maybe_load_more_for_scroll();
                self.request_frame();
            }
            _ if allow_plain_char_navigation && self.list_keymap.page_up.is_pressed(key) => {
                let step = self.view_rows.unwrap_or(10).max(1);
                if self.selected > 0 {
                    self.selected = self.selected.saturating_sub(step);
                    self.ensure_selected_visible();
                    self.request_frame();
                }
            }
            _ if allow_plain_char_navigation && self.list_keymap.jump_top.is_pressed(key)
                && !self.filtered_rows.is_empty() => {
                    self.selected = 0;
                    self.ensure_selected_visible();
                    self.request_frame();
                }
            _ if allow_plain_char_navigation && self.list_keymap.jump_bottom.is_pressed(key)
                && !self.filtered_rows.is_empty() => {
                    self.selected = self.filtered_rows.len().saturating_sub(1);
                    self.ensure_selected_visible();
                    self.maybe_load_more_for_scroll();
                    self.request_frame();
                }
            _ if allow_plain_char_navigation && self.list_keymap.page_down.is_pressed(key)
                && !self.filtered_rows.is_empty() => {
                    let step = self.view_rows.unwrap_or(10).max(1);
                    let target = self.selected.saturating_add(step);
                    let max_index = self.filtered_rows.len().saturating_sub(1);
                    if target > max_index && self.pagination.next_cursor.is_some() {
                        self.pending_page_down_target = Some(target);
                        self.load_more_if_needed(LoadTrigger::Scroll);
                    } else {
                        self.selected = target.min(max_index);
                        self.ensure_selected_visible();
                        self.maybe_load_more_for_scroll();
                    }
                    self.request_frame();
                }
            KeyEvent {
                code: KeyCode::Tab, ..
            } => {
                self.focus_next_toolbar_control();
                self.request_frame();
            }
            KeyEvent {
                code: KeyCode::BackTab,
                ..
            } => {
                self.focus_previous_toolbar_control();
                self.request_frame();
            }
            _ if allow_plain_char_navigation
                && (self.list_keymap.move_left.is_pressed(key)
                    || self.list_keymap.move_right.is_pressed(key)) =>
            {
                self.change_focused_toolbar_value();
                self.request_frame();
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                let mut new_query = self.query.clone();
                new_query.pop();
                self.set_query(new_query);
            }
            _ if self.archive_shortcut_available()
                && crate::key_hint::ctrl(KeyCode::Char('a')).is_press(key) =>
            {
                self.request_archive_for_selected_session();
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            }
                // basic text input for search
                if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT)
                => {
                    let mut new_query = self.query.clone();
                    new_query.push(c);
                    self.set_query(new_query);
                }
            _ => {}
        }
        Ok(None)
    }

    fn handle_paste(&mut self, pasted: String) {
        if self.is_transcript_loading() {
            return;
        }
        if let Some(dashboard_composer) = self.dashboard_composer.as_mut()
            && !self.dashboard_search_active
        {
            dashboard_composer.composer.handle_paste(pasted);
            self.request_frame();
            return;
        }
        let Some(pasted) = normalize_pasted_search_query(&pasted) else {
            return;
        };
        let mut new_query = self.query.clone();
        if !new_query.is_empty() && !new_query.ends_with(char::is_whitespace) {
            new_query.push(' ');
        }
        new_query.push_str(&pasted);
        self.set_query(new_query);
    }

    fn start_initial_load(&mut self) {
        self.relative_time_reference = Some(Utc::now());
        self.reset_pagination();
        self.all_rows.clear();
        self.filtered_rows.clear();
        self.seen_rows.clear();
        self.selected = 0;
        self.pending_page_down_target = None;
        self.frozen_footer_percent = None;

        let search_token = if self.query.is_empty() {
            self.search_state = SearchState::Idle;
            None
        } else {
            let token = self.allocate_search_token();
            self.search_state = SearchState::Active { token };
            Some(token)
        };

        let request_token = self.allocate_request_token();
        let mode = self.initial_page_mode;
        self.pagination
            .start_load(request_token, search_token, mode);
        self.request_frame();

        (self.picker_loader)(PickerLoadRequest::Page(PageLoadRequest {
            cursor: None,
            request_token,
            search_token,
            mode,
            cwd_filter: self.active_cwd_filter(),
            status: self.status,
            provider_filter: self.provider_filter.clone(),
            sort_key: self.sort_key,
        }));
    }

    async fn handle_background_event(
        &mut self,
        event: BackgroundEvent,
    ) -> Result<Option<SessionSelection>> {
        match event {
            BackgroundEvent::Page {
                request_token,
                search_token,
                page,
            } => {
                let Some(pending) = self.pagination.finish_load(request_token) else {
                    return Ok(None);
                };
                let page_has_rows = matches!(&page, Ok(page) if !page.rows.is_empty());
                // Fall back only when the initial DB listing is unusable. Once SQLite returns
                // rows, its pagination is authoritative and an empty later page ends the list.
                let should_restart_from_store = pending.mode == PageLoadMode::StateDbOnly
                    && self.all_rows.is_empty()
                    && !page_has_rows;
                if should_restart_from_store {
                    let request_token = self.allocate_request_token();
                    let search_token = pending.search_token.or(search_token);
                    self.pagination.reset();
                    self.pagination.start_load(
                        request_token,
                        search_token,
                        PageLoadMode::StoreDefault,
                    );
                    (self.picker_loader)(PickerLoadRequest::Page(PageLoadRequest {
                        cursor: None,
                        request_token,
                        search_token,
                        mode: PageLoadMode::StoreDefault,
                        cwd_filter: self.active_cwd_filter(),
                        status: self.status,
                        provider_filter: self.provider_filter.clone(),
                        sort_key: self.sort_key,
                    }));
                    return Ok(None);
                }
                let page = page.map_err(color_eyre::Report::from)?;
                self.ingest_page(page);
                self.load_dashboard_composer_inventory();
                self.complete_pending_page_down();
                let completed_token = pending.search_token.or(search_token);
                self.continue_search_if_token_matches(completed_token);
            }
            BackgroundEvent::Preview { thread_id, preview } => {
                self.transcript_previews.insert(
                    thread_id,
                    match preview {
                        Ok(lines) => TranscriptPreviewState::Loaded(lines),
                        Err(_) => TranscriptPreviewState::Failed,
                    },
                );
                self.request_frame();
            }
            BackgroundEvent::Transcript {
                thread_id,
                transcript,
            } => match transcript {
                Ok(cells) => {
                    let should_open = self.pending_transcript_open == Some(thread_id);
                    self.transcript_cells
                        .insert(thread_id, SessionTranscriptState::Loaded(cells.clone()));
                    if should_open {
                        self.pending_transcript_cancellation = None;
                        self.open_pending_transcript_if_ready();
                    }
                    self.request_frame();
                }
                Err(_) => {
                    self.transcript_cells
                        .insert(thread_id, SessionTranscriptState::Failed);
                    if self.pending_transcript_open == Some(thread_id) {
                        self.pending_transcript_cancellation = None;
                        self.pending_transcript_open = None;
                        self.transcript_loading_frame_shown = false;
                        self.inline_error = Some("Could not load transcript preview".to_string());
                    }
                    self.request_frame();
                }
            },
            BackgroundEvent::Archive { thread_id, result } => {
                self.handle_archive_result(thread_id, result);
            }
            BackgroundEvent::Unarchive { thread_id, result } => {
                return Ok(self.handle_unarchive_result(thread_id, result));
            }
            BackgroundEvent::AppServer(event) => {
                if self.handle_app_server_event(event) {
                    return Ok(Some(SessionSelection::ReconnectDashboard(
                        self.dashboard_resume_state(),
                    )));
                }
            }
            BackgroundEvent::DashboardComposerInventory {
                cwd,
                skills,
                plugins,
                connectors,
            } => {
                if paths_match(&cwd, &self.dashboard_project_cwd())
                    && let Some(dashboard) = self.dashboard_composer.as_mut()
                {
                    dashboard.composer.set_skill_mentions(skills);
                    dashboard.composer.set_plugin_mentions(plugins);
                    dashboard.composer.set_connector_mentions(connectors);
                    self.request_frame();
                }
            }
        }
        Ok(None)
    }

    fn handle_app_server_event(&mut self, event: AppServerEvent) -> bool {
        match event {
            AppServerEvent::ServerNotification(notification) => match *notification {
                ServerNotification::ThreadStatusChanged(notification) => {
                    if let Ok(thread_id) = ThreadId::from_string(&notification.thread_id) {
                        if matches!(
                            notification.status,
                            codex_app_server_protocol::ThreadStatus::SystemError
                        ) {
                            self.dashboard_system_errors.insert(thread_id);
                        } else {
                            self.dashboard_system_errors.remove(&thread_id);
                        }
                    }
                    if let Ok(thread_id) = ThreadId::from_string(&notification.thread_id) {
                        self.transcript_previews.remove(&thread_id);
                    }
                    self.update_dashboard_row(&notification.thread_id, |row| {
                        row.dashboard_status = Some(crate::dashboard::status(&notification.status));
                    });
                }
                ServerNotification::ThreadNameUpdated(notification) => {
                    self.update_dashboard_row(&notification.thread_id, |row| {
                        row.thread_name = notification.thread_name;
                    });
                }
                ServerNotification::ThreadArchived(notification) => {
                    self.remove_dashboard_row(&notification.thread_id);
                }
                ServerNotification::ThreadUnarchived(_) => self.start_initial_load(),
                ServerNotification::ThreadDeleted(notification) => {
                    self.remove_dashboard_row(&notification.thread_id);
                }
                ServerNotification::ThreadStarted(notification) => {
                    if notification.thread.parent_thread_id.is_some() {
                        return false;
                    }
                    if matches!(
                        notification.thread.status,
                        codex_app_server_protocol::ThreadStatus::SystemError
                    ) && let Ok(thread_id) = ThreadId::from_string(&notification.thread.id)
                    {
                        self.dashboard_system_errors.insert(thread_id);
                    }
                    if let Some(row) = row_from_app_server_thread(notification.thread, true) {
                        if row
                            .seen_key()
                            .is_some_and(|seen_key| !self.seen_rows.insert(seen_key))
                        {
                            return false;
                        }
                        self.all_rows.push(row);
                        self.apply_filter();
                    }
                }
                _ => {}
            },
            AppServerEvent::Lagged { .. } => self.start_initial_load(),
            AppServerEvent::Disconnected { message } => {
                self.inline_error = Some(format!("Dashboard disconnected: {message}"));
                self.request_frame();
                return self.is_agents_dashboard();
            }
            AppServerEvent::ServerRequest(_) => {}
        }
        false
    }

    fn update_dashboard_row(&mut self, thread_id: &str, update: impl FnOnce(&mut Row)) {
        let selected_thread_id = self
            .filtered_rows
            .get(self.selected)
            .and_then(|row| row.thread_id);
        let Ok(thread_id) = ThreadId::from_string(thread_id) else {
            return;
        };
        let Some(row) = self
            .all_rows
            .iter_mut()
            .find(|row| row.thread_id == Some(thread_id))
        else {
            return;
        };
        update(row);
        row.updated_at = Some(Utc::now());
        self.apply_filter();
        self.restore_dashboard_selection(selected_thread_id);
    }

    fn remove_dashboard_row(&mut self, thread_id: &str) {
        let selected_thread_id = self
            .filtered_rows
            .get(self.selected)
            .and_then(|row| row.thread_id)
            .filter(|selected| selected.to_string() != thread_id);
        let Ok(thread_id) = ThreadId::from_string(thread_id) else {
            return;
        };
        self.all_rows.retain(|row| row.thread_id != Some(thread_id));
        self.seen_rows.remove(&SeenRowKey::Thread(thread_id));
        self.dashboard_system_errors.remove(&thread_id);
        self.transcript_previews.remove(&thread_id);
        self.apply_filter();
        self.restore_dashboard_selection(selected_thread_id);
    }

    fn restore_dashboard_selection(&mut self, thread_id: Option<ThreadId>) {
        if let Some(thread_id) = thread_id
            && let Some(index) = self
                .filtered_rows
                .iter()
                .position(|row| row.thread_id == Some(thread_id))
        {
            self.selected = index;
        }
        self.selected = self
            .selected
            .min(self.filtered_rows.len().saturating_sub(1));
        self.ensure_selected_visible();
    }

    fn reset_pagination(&mut self) {
        self.pagination.reset();
        self.frozen_footer_percent = None;
    }

    fn ingest_page(&mut self, page: PickerPage) {
        let PickerPage {
            rows,
            dashboard_system_errors,
            next_cursor,
            num_scanned_files,
            reached_scan_cap,
        } = page;
        self.pagination
            .complete_page(next_cursor, num_scanned_files, reached_scan_cap);
        self.dashboard_system_errors.extend(dashboard_system_errors);

        for row in rows {
            if let Some(seen_key) = row.seen_key() {
                if self.seen_rows.insert(seen_key) {
                    self.all_rows.push(row);
                }
            } else {
                self.all_rows.push(row);
            }
        }

        self.apply_filter();
    }

    fn complete_pending_page_down(&mut self) {
        let Some(target) = self.pending_page_down_target else {
            return;
        };
        if self.filtered_rows.is_empty() {
            return;
        }

        let max_index = self.filtered_rows.len().saturating_sub(1);
        if target > max_index && self.pagination.next_cursor.is_some() {
            self.load_more_if_needed(LoadTrigger::Scroll);
            return;
        }

        self.pending_page_down_target = None;
        self.selected = target.min(max_index);
        self.ensure_selected_visible();
        self.maybe_load_more_for_scroll();
        self.request_frame();
    }

    fn apply_filter(&mut self) {
        let selected_key = self
            .filtered_rows
            .get(self.selected)
            .and_then(Row::seen_key);
        let base_iter = self
            .all_rows
            .iter()
            .filter(|row| self.row_matches_filter(row));
        if self.query.is_empty() {
            self.filtered_rows = base_iter.cloned().collect();
        } else {
            let q = self.query.to_lowercase();
            self.filtered_rows = base_iter.filter(|r| r.matches_query(&q)).cloned().collect();
        }
        if self.is_agents_dashboard()
            && let Some(cwd) = self.dashboard_fallback_cwd.as_ref()
            && self.query.is_empty()
            && !self.filtered_rows.iter().any(|row| {
                row.cwd
                    .as_ref()
                    .is_some_and(|row_cwd| paths_match(row_cwd, cwd))
            })
        {
            self.filtered_rows.push(Row {
                path: None,
                preview: String::from("Start a new agent"),
                thread_id: None,
                thread_name: None,
                created_at: None,
                updated_at: None,
                cwd: Some(cwd.clone()),
                git_branch: None,
                dashboard_status: None,
            });
        }
        self.sort_dashboard_rows();
        if let Some(thread_id) = self.dashboard_restore_thread_id
            && let Some(index) = self
                .filtered_rows
                .iter()
                .position(|row| row.thread_id == Some(thread_id))
        {
            self.selected = index;
            self.dashboard_restore_thread_id = None;
            self.dashboard_restore_cwd = None;
        } else if let Some(cwd) = self.dashboard_restore_cwd.as_ref()
            && let Some(index) = self.filtered_rows.iter().position(|row| {
                row.cwd
                    .as_ref()
                    .is_some_and(|row_cwd| paths_match(row_cwd, cwd))
            })
        {
            self.selected = index;
        }
        if let Some(selected_key) = selected_key
            && let Some(index) = self
                .filtered_rows
                .iter()
                .position(|row| row.seen_key().as_ref() == Some(&selected_key))
        {
            self.selected = index;
        }
        if self.selected >= self.filtered_rows.len() {
            self.selected = self.filtered_rows.len().saturating_sub(1);
        }
        if self.filtered_rows.is_empty() {
            self.scroll_top = 0;
        }
        self.ensure_selected_visible();
        self.request_frame();
    }

    fn row_matches_filter(&self, row: &Row) -> bool {
        if self.filter_mode == SessionFilterMode::All {
            return true;
        }
        let Some(filter_cwd) = self.local_filter_cwd.as_ref() else {
            return true;
        };
        let Some(row_cwd) = row.cwd.as_ref() else {
            return false;
        };
        paths_match(row_cwd, filter_cwd)
    }

    fn set_query(&mut self, new_query: String) {
        if self.query == new_query {
            return;
        }
        self.query = new_query;
        self.selected = 0;
        self.apply_filter();
        if self.query.is_empty() {
            self.search_state = SearchState::Idle;
            return;
        }
        if !self.filtered_rows.is_empty() {
            self.search_state = SearchState::Idle;
            return;
        }
        if self.pagination.reached_scan_cap || self.pagination.next_cursor.is_none() {
            self.search_state = SearchState::Idle;
            return;
        }
        let token = self.allocate_search_token();
        self.search_state = SearchState::Active { token };
        self.load_more_if_needed(LoadTrigger::Search { token });
    }

    fn clear_query_preserving_selection(&mut self) {
        let selected_key = self
            .filtered_rows
            .get(self.selected)
            .and_then(Row::seen_key);
        self.query.clear();
        self.search_state = SearchState::Idle;
        self.apply_filter();
        if let Some(selected_key) = selected_key
            && let Some(index) = self
                .filtered_rows
                .iter()
                .position(|row| row.seen_key().as_ref() == Some(&selected_key))
        {
            self.selected = index;
            self.ensure_selected_visible();
            self.request_frame();
        }
    }

    fn continue_search_if_needed(&mut self) {
        let Some(token) = self.search_state.active_token() else {
            return;
        };
        if !self.filtered_rows.is_empty() {
            self.search_state = SearchState::Idle;
            return;
        }
        if self.pagination.reached_scan_cap || self.pagination.next_cursor.is_none() {
            self.search_state = SearchState::Idle;
            return;
        }
        self.load_more_if_needed(LoadTrigger::Search { token });
    }

    fn continue_search_if_token_matches(&mut self, completed_token: Option<usize>) {
        let Some(active) = self.search_state.active_token() else {
            return;
        };
        if let Some(token) = completed_token
            && token != active
        {
            return;
        }
        self.continue_search_if_needed();
    }

    fn ensure_selected_visible(&mut self) {
        if self.filtered_rows.is_empty() {
            self.scroll_top = 0;
            self.dashboard_scroll_offset = 0;
            return;
        }
        let viewport_rows = self.view_rows.unwrap_or(usize::MAX).max(1);
        if self.is_agents_dashboard() {
            let selected_end = self.rendered_height_between(/*start*/ 0, self.selected);
            let selected_start = if self.selected == 0 {
                0
            } else {
                self.rendered_height_between(/*start*/ 0, self.selected - 1)
                    + self.row_separator_height()
            };
            let has_more_after_selection = self.pagination.next_cursor.is_some()
                || self.selected + 1 < self.filtered_rows.len();
            let available_rows = viewport_rows
                .saturating_sub(usize::from(self.dashboard_scroll_offset > 0))
                .saturating_sub(usize::from(has_more_after_selection))
                .max(1);
            if selected_start < self.dashboard_scroll_offset {
                self.dashboard_scroll_offset = selected_start;
            } else if selected_end > self.dashboard_scroll_offset.saturating_add(available_rows) {
                self.dashboard_scroll_offset = selected_end.saturating_sub(available_rows);
            }

            self.scroll_top = (0..self.filtered_rows.len())
                .find(|row_index| {
                    self.rendered_height_between(/*start*/ 0, *row_index)
                        > self.dashboard_scroll_offset
                })
                .unwrap_or_else(|| self.filtered_rows.len().saturating_sub(1));
            return;
        }
        if self.selected < self.scroll_top {
            self.scroll_top = self.selected;
        }
        while self.rendered_height_between(self.scroll_top, self.selected)
            > self.available_content_rows(viewport_rows)
            && self.scroll_top < self.selected
        {
            self.scroll_top += 1;
        }
    }

    fn ensure_minimum_rows_for_view(&mut self, minimum_rows: usize) {
        if minimum_rows == 0 {
            return;
        }
        if self.pagination.is_loading() || self.pagination.next_cursor.is_none() {
            return;
        }
        let rendered_rows = if self.filtered_rows.is_empty() {
            0
        } else {
            self.rendered_height_between(/*start*/ 0, self.filtered_rows.len() - 1)
        };
        if rendered_rows >= self.available_content_rows(minimum_rows) {
            return;
        }
        if let Some(token) = self.search_state.active_token() {
            self.load_more_if_needed(LoadTrigger::Search { token });
        } else {
            self.load_more_if_needed(LoadTrigger::Scroll);
        }
    }

    fn update_viewport(&mut self, rows: usize, width: u16) {
        self.view_rows = if rows == 0 { None } else { Some(rows) };
        self.view_width = Some(width);
        self.ensure_selected_visible();
        self.load_visible_dashboard_previews();
    }

    fn load_visible_dashboard_previews(&mut self) {
        if !self.is_agents_dashboard() {
            return;
        }
        let visible_rows = self.view_rows.unwrap_or_default();
        if visible_rows == 0 {
            return;
        }
        let visible_end = self
            .scroll_top
            .saturating_add(visible_rows)
            .min(self.filtered_rows.len());
        let thread_ids = self.filtered_rows[self.scroll_top..visible_end]
            .iter()
            .filter_map(|row| row.thread_id)
            .filter(|thread_id| !self.transcript_previews.contains_key(thread_id))
            .collect::<Vec<_>>();
        for thread_id in thread_ids {
            self.transcript_previews
                .insert(thread_id, TranscriptPreviewState::Loading);
            (self.picker_loader)(PickerLoadRequest::Preview { thread_id });
        }
    }

    fn maybe_load_more_for_scroll(&mut self) {
        if self.pagination.is_loading() || self.pagination.next_cursor.is_none() {
            return;
        }
        if self.filtered_rows.is_empty() {
            return;
        }
        let remaining = self.filtered_rows.len().saturating_sub(self.selected + 1);
        if remaining <= LOAD_NEAR_THRESHOLD {
            self.load_more_if_needed(LoadTrigger::Scroll);
        }
    }

    fn load_more_if_needed(&mut self, trigger: LoadTrigger) {
        let Some((cursor, mode)) = self.pagination.next_page() else {
            return;
        };
        self.freeze_footer_percent();
        let request_token = self.allocate_request_token();
        let search_token = match trigger {
            LoadTrigger::Scroll => None,
            LoadTrigger::Search { token } => Some(token),
        };
        self.pagination
            .start_load(request_token, search_token, mode);
        self.request_frame();

        (self.picker_loader)(PickerLoadRequest::Page(PageLoadRequest {
            cursor: Some(cursor),
            request_token,
            search_token,
            mode,
            cwd_filter: self.active_cwd_filter(),
            status: self.status,
            provider_filter: self.provider_filter.clone(),
            sort_key: self.sort_key,
        }));
    }

    fn freeze_footer_percent(&mut self) {
        let list_height = self.view_rows.unwrap_or_default().min(u16::MAX as usize) as u16;
        self.frozen_footer_percent = Some(picker_footer_scroll_percent(self, list_height));
    }

    fn allocate_request_token(&mut self) -> usize {
        let token = self.next_request_token;
        self.next_request_token = self.next_request_token.wrapping_add(1);
        token
    }

    fn allocate_search_token(&mut self) -> usize {
        let token = self.next_search_token;
        self.next_search_token = self.next_search_token.wrapping_add(1);
        token
    }

    /// Cycles the sort order between creation time and last-updated time.
    ///
    /// Triggers a full reload because the backend must re-sort all sessions.
    /// The existing `all_rows` are cleared and pagination restarts from the
    /// beginning with the new sort key.
    fn toggle_sort_key(&mut self) {
        self.sort_key = match self.sort_key {
            ThreadSortKey::CreatedAt => ThreadSortKey::UpdatedAt,
            ThreadSortKey::UpdatedAt
            | ThreadSortKey::RecencyAt
            | ThreadSortKey::SectionPosition => ThreadSortKey::CreatedAt,
        };
        self.start_initial_load();
    }

    fn toggle_filter_mode(&mut self) {
        let next_filter_mode = self.filter_mode.toggle(self.filter_cwd.as_deref());
        if self.filter_mode == next_filter_mode {
            return;
        }
        self.filter_mode = next_filter_mode;
        self.start_initial_load();
    }

    fn toggle_status(&mut self) {
        self.status = match self.status {
            SessionStatus::Active => SessionStatus::Archived,
            SessionStatus::Archived => SessionStatus::Active,
        };
        self.start_initial_load();
    }

    fn active_cwd_filter(&self) -> Option<PathBuf> {
        match self.filter_mode {
            SessionFilterMode::Cwd => self.filter_cwd.clone(),
            SessionFilterMode::All => None,
        }
    }

    fn focus_previous_toolbar_control(&mut self) {
        self.toolbar_focus = self.toolbar_focus.previous(self.action);
    }

    fn focus_next_toolbar_control(&mut self) {
        self.toolbar_focus = self.toolbar_focus.next(self.action);
    }

    fn change_focused_toolbar_value(&mut self) {
        match self.toolbar_focus {
            ToolbarControl::Sort => self.toggle_sort_key(),
            ToolbarControl::Filter => self.toggle_filter_mode(),
            ToolbarControl::Status => self.toggle_status(),
        }
    }

    async fn toggle_density(&mut self) {
        self.density = self.density.toggle();
        self.ensure_selected_visible();
        if let Err(err) = self.persist_density().await {
            warn!(error = %err, "failed to persist session picker view mode");
            self.inline_error = Some(format!("Failed to save view mode: {err}"));
        }
        self.request_frame();
    }

    async fn persist_density(&self) -> Result<()> {
        let Some(persistence) = &self.view_persistence else {
            return Ok(());
        };

        ConfigEditsBuilder::new(&persistence.codex_home)
            .set_session_picker_view(SessionPickerViewMode::from(self.density))
            .apply()
            .await
            .map_err(|err| color_eyre::eyre::eyre!("failed to write config.toml: {err}"))?;

        Ok(())
    }

    fn toggle_selected_expansion(&mut self) {
        let Some(row) = self.filtered_rows.get(self.selected) else {
            return;
        };
        let Some(thread_id) = row.thread_id else {
            return;
        };
        if self.expanded_thread_id == Some(thread_id) {
            self.expanded_thread_id = None;
            self.request_frame();
            return;
        }
        self.expanded_thread_id = Some(thread_id);
        if let std::collections::hash_map::Entry::Vacant(e) =
            self.transcript_previews.entry(thread_id)
        {
            e.insert(TranscriptPreviewState::Loading);
            (self.picker_loader)(PickerLoadRequest::Preview { thread_id });
        }
        self.request_frame();
    }

    fn rendered_height_between(&self, start: usize, end_inclusive: usize) -> usize {
        self.filtered_rows
            .get(start..=end_inclusive)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(offset, row)| {
                let row_idx = start + offset;
                let is_selected = row_idx == self.selected;
                let is_expanded = is_selected
                    && row.thread_id.is_some()
                    && self.expanded_thread_id == row.thread_id;
                render_session_lines(
                    row,
                    self,
                    is_selected,
                    is_expanded,
                    /*is_zebra*/ false,
                    self.view_width.unwrap_or(u16::MAX),
                )
                .len()
                    + usize::from(self.starts_dashboard_group(row_idx, start))
            })
            .sum::<usize>()
            + self.row_separator_height() * end_inclusive.saturating_sub(start)
    }

    fn has_more_above(&self) -> bool {
        if self.is_agents_dashboard() {
            self.dashboard_scroll_offset > 0
        } else {
            self.scroll_top > 0
        }
    }

    fn has_more_below(&self, viewport_height: usize) -> bool {
        if self.filtered_rows.is_empty() {
            return false;
        }
        if self.pagination.next_cursor.is_some() {
            return true;
        }
        if self.is_agents_dashboard() {
            let total_height =
                self.rendered_height_between(/*start*/ 0, self.filtered_rows.len() - 1);
            let available_rows = viewport_height
                .saturating_sub(usize::from(self.has_more_above()))
                .max(1);
            return total_height > self.dashboard_scroll_offset.saturating_add(available_rows);
        }
        let capacity = self.available_content_rows(viewport_height);
        let mut used = 0usize;
        for (offset, row) in self.filtered_rows[self.scroll_top..].iter().enumerate() {
            let row_idx = self.scroll_top + offset;
            let is_selected = row_idx == self.selected;
            let is_expanded =
                is_selected && row.thread_id.is_some() && self.expanded_thread_id == row.thread_id;
            let row_height = render_session_lines(
                row,
                self,
                is_selected,
                is_expanded,
                /*is_zebra*/ false,
                self.view_width.unwrap_or(u16::MAX),
            )
            .len()
                + usize::from(self.starts_dashboard_group(row_idx, self.scroll_top));
            let separator_height = usize::from(offset > 0) * self.row_separator_height();
            if used + separator_height + row_height > capacity {
                return true;
            }
            used += separator_height + row_height;
        }
        false
    }

    fn available_content_rows(&self, viewport_height: usize) -> usize {
        viewport_height
            .saturating_sub(usize::from(self.has_more_above()))
            .saturating_sub(usize::from(
                self.pagination.next_cursor.is_some()
                    || self.selected + 1 < self.filtered_rows.len(),
            ))
            .max(1)
    }

    fn row_separator_height(&self) -> usize {
        match self.density {
            SessionListDensity::Comfortable => 1,
            SessionListDensity::Dense => 0,
        }
    }

    fn starts_dashboard_group(&self, row_index: usize, viewport_start: usize) -> bool {
        if !self.is_agents_dashboard() || row_index == viewport_start {
            return self.is_agents_dashboard();
        }
        self.dashboard_group_label(&self.filtered_rows[row_index - 1])
            != self.dashboard_group_label(&self.filtered_rows[row_index])
    }

    fn dashboard_group_label(&self, row: &Row) -> String {
        match self.dashboard_group_mode {
            DashboardGroupMode::Project => row
                .cwd
                .as_ref()
                .map(|cwd| format_directory_display(cwd, /*max_width*/ None))
                .unwrap_or_else(|| String::from("Unknown project")),
            DashboardGroupMode::Status => row
                .dashboard_status
                .map(DashboardStatus::label)
                .unwrap_or("Done")
                .to_string(),
        }
    }
}

fn row_from_app_server_thread(thread: Thread, show_dashboard_status: bool) -> Option<Row> {
    let thread_id = match ThreadId::from_string(&thread.id) {
        Ok(thread_id) => thread_id,
        Err(err) => {
            warn!(thread_id = thread.id, %err, "Skipping app-server picker row with invalid id");
            return None;
        }
    };
    let preview = thread.preview.trim();
    let dashboard_status = show_dashboard_status.then(|| crate::dashboard::status(&thread.status));
    Some(Row {
        path: thread.path,
        preview: if preview.is_empty() {
            String::from("(no message yet)")
        } else {
            preview.to_string()
        },
        thread_id: Some(thread_id),
        thread_name: thread.name,
        created_at: chrono::DateTime::from_timestamp(thread.created_at, 0)
            .map(|dt| dt.with_timezone(&Utc)),
        updated_at: chrono::DateTime::from_timestamp(thread.updated_at, 0)
            .map(|dt| dt.with_timezone(&Utc)),
        cwd: Some(thread.cwd.to_path_buf()),
        git_branch: thread.git_info.and_then(|git_info| git_info.branch),
        dashboard_status,
    })
}

fn thread_list_params(
    cursor: Option<String>,
    cwd_filter: Option<&Path>,
    status: SessionStatus,
    provider_filter: ProviderFilter,
    sort_key: ThreadSortKey,
    include_non_interactive: bool,
    use_state_db_only: bool,
) -> ThreadListParams {
    ThreadListParams {
        cursor,
        limit: Some(PAGE_SIZE as u32),
        sort_key: Some(sort_key),
        sort_direction: None,
        model_providers: match provider_filter {
            ProviderFilter::Any => None,
            ProviderFilter::MatchDefault(default_provider) => Some(vec![default_provider]),
        },
        source_kinds: Some(crate::resume_source_kinds(include_non_interactive)),
        archived: Some(status == SessionStatus::Archived),
        section_id: None,
        parent_thread_id: None,
        ancestor_thread_id: None,
        cwd: cwd_filter.map(|cwd| ThreadListCwdFilter::One(cwd.to_string_lossy().into_owned())),
        use_state_db_only,
        search_term: None,
    }
}

fn paths_match(a: &Path, b: &Path) -> bool {
    path_utils::paths_match_after_normalization(a, b)
}

#[cfg_attr(not(test), allow(dead_code))]
fn parse_timestamp_str(ts: &str) -> Option<DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.with_timezone(&Utc))
        .ok()
}

fn draw_picker(tui: &mut Tui, state: &PickerState, screen_size: Size) -> std::io::Result<()> {
    // Render full-screen overlay
    tui.draw(screen_size.height, |frame| {
        render_picker_frame(frame, state);
    })
}

fn render_picker_frame(frame: &mut crate::custom_terminal::Frame, state: &PickerState) {
    let area = frame.area();
    let bottom_height = picker_bottom_height(state, area.width, area.height);
    let [header, _header_gap, search, _search_gap, list, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(bottom_height),
    ])
    .areas(area);

    let chrome = |area: Rect| {
        Rect::new(
            area.x.saturating_add(1),
            area.y,
            area.width.saturating_sub(2),
            area.height,
        )
    };

    // Header
    let title = match state.launch_context {
        SessionPickerLaunchContext::AgentsDashboard => "Codex Agents",
        SessionPickerLaunchContext::Startup
        | SessionPickerLaunchContext::ExistingSession { .. } => state.action.title(),
    };
    let header_title = match state.launch_context {
        SessionPickerLaunchContext::AgentsDashboard => title.bold().magenta(),
        SessionPickerLaunchContext::Startup
        | SessionPickerLaunchContext::ExistingSession { .. }
            if default_bg().is_some_and(is_light) =>
        {
            title.bold().fg(best_color((0, 100, 0)))
        }
        SessionPickerLaunchContext::Startup
        | SessionPickerLaunchContext::ExistingSession { .. } => title.bold().cyan(),
    };
    let header_line: Line = vec![header_title].into();
    frame.render_widget_ref(&header_line, chrome(header));

    // Search line
    let search = chrome(search);
    frame.render_widget_ref(&search_line(state, search.width), search);

    let list_margin = list_horizontal_margin(state);
    let list = Rect::new(
        list.x.saturating_add(list_margin),
        list.y,
        list_viewport_width(list.width, state),
        list.height,
    );
    render_list(frame, list, state);
    if state.is_transcript_loading() {
        render_transcript_loading_overlay(frame, list);
    }

    if let Some(dashboard) = state.dashboard_composer.as_ref() {
        let composer_area = bottom;
        dashboard.composer.render_with_mask(
            composer_area,
            frame.buffer_mut(),
            /*mask_char*/ None,
        );
        if !state.dashboard_search_active
            && let Some((x, y)) = dashboard.composer.cursor_pos(composer_area)
        {
            frame.set_cursor_position((x, y));
        }
    } else {
        render_picker_footer(frame, bottom, state, list.height);
    }
}

fn picker_bottom_height(state: &PickerState, width: u16, height: u16) -> u16 {
    state.dashboard_composer.as_ref().map_or(4, |dashboard| {
        dashboard
            .composer
            .desired_height_with_textarea_right_reserve(width, /*textarea_right_reserve*/ 0)
            .clamp(4, height.saturating_sub(6).max(4))
    })
}

fn list_horizontal_margin(state: &PickerState) -> u16 {
    if state.is_agents_dashboard() {
        DASHBOARD_LIST_HORIZONTAL_MARGIN
    } else {
        PICKER_LIST_HORIZONTAL_MARGIN
    }
}

fn list_viewport_width(width: u16, state: &PickerState) -> u16 {
    width.saturating_sub(list_horizontal_margin(state).saturating_mul(2))
}

fn search_line(state: &PickerState, width: u16) -> Line<'_> {
    if let Some(error) = state.inline_error.as_deref() {
        return Line::from(error.red());
    }
    let search = if state.is_agents_dashboard() && !state.dashboard_search_active {
        "Ctrl+F search sessions".dim()
    } else if state.query.is_empty() {
        "Type to search".dim()
    } else {
        format!("Search: {}", state.query).into()
    };
    let search_width = UnicodeWidthStr::width(search.content.as_ref());
    let mut toolbar = toolbar_line(state, /*compact*/ false);
    if search_width.saturating_add(toolbar.width()) > usize::from(width.saturating_sub(2)) {
        toolbar = toolbar_line(state, /*compact*/ true);
    }
    let toolbar_width = toolbar.width();
    let spacer_width = width
        .saturating_sub((search_width + toolbar_width) as u16)
        .max(2) as usize;
    let available_search_width = width
        .saturating_sub(toolbar_width as u16)
        .saturating_sub(spacer_width as u16) as usize;
    let search = if search_width > available_search_width {
        let truncated = truncate_text(search.content.as_ref(), available_search_width);
        if state.query.is_empty() {
            truncated.dim()
        } else {
            truncated.into()
        }
    } else {
        search
    };

    let mut spans = vec![search, " ".repeat(spacer_width).into()];
    spans.extend(toolbar.spans);
    spans.into()
}

fn toolbar_line(state: &PickerState, compact: bool) -> Line<'static> {
    if state.is_agents_dashboard() {
        return vec![
            "Group: ".dim(),
            toolbar_value(
                state.dashboard_group_mode.label(),
                /*active*/ true,
                /*focused*/ false,
            ),
        ]
        .into();
    }
    let mut spans = Vec::new();
    let separator = if compact && matches!(state.action, SessionPickerAction::Resume) {
        " "
    } else {
        "   "
    };
    spans.extend(filter_control_spans(state, compact));
    spans.push(separator.dim());
    if matches!(state.action, SessionPickerAction::Resume) {
        let status_focused = state.toolbar_focus == ToolbarControl::Status;
        if compact {
            let active_status = match state.status {
                SessionStatus::Active => "Active",
                SessionStatus::Archived => "Archived",
            };
            spans.push(toolbar_value(
                active_status,
                /*active*/ true,
                status_focused,
            ));
        } else {
            spans.push("Status: ".dim());
            spans.push(toolbar_value(
                "Active",
                state.status == SessionStatus::Active,
                status_focused,
            ));
            spans.push(toolbar_value(
                "Archived",
                state.status == SessionStatus::Archived,
                status_focused,
            ));
        }
        spans.push(separator.dim());
    }
    spans.extend(sort_control_spans(state, compact));
    spans.into()
}

fn sort_control_spans(state: &PickerState, compact: bool) -> Vec<Span<'static>> {
    let sort_focused = state.toolbar_focus == ToolbarControl::Sort;
    if compact {
        return vec![
            "Sort:".dim(),
            toolbar_value(
                sort_key_label(state.sort_key),
                /*active*/ true,
                sort_focused,
            ),
        ];
    }
    vec![
        "Sort: ".dim(),
        toolbar_value(
            sort_key_label(ThreadSortKey::UpdatedAt),
            state.sort_key == ThreadSortKey::UpdatedAt,
            sort_focused,
        ),
        toolbar_value(
            sort_key_label(ThreadSortKey::CreatedAt),
            state.sort_key == ThreadSortKey::CreatedAt,
            sort_focused,
        ),
    ]
}

fn filter_control_spans(state: &PickerState, compact: bool) -> Vec<Span<'static>> {
    let filter_focused = state.toolbar_focus == ToolbarControl::Filter;
    if compact || state.filter_cwd.is_none() {
        return vec![
            "Filter:".dim(),
            toolbar_value(
                filter_mode_label(state.filter_mode),
                /*active*/ true,
                filter_focused,
            ),
        ];
    }
    vec![
        "Filter: ".dim(),
        toolbar_value(
            filter_mode_label(SessionFilterMode::Cwd),
            state.filter_mode == SessionFilterMode::Cwd,
            filter_focused,
        ),
        toolbar_value(
            filter_mode_label(SessionFilterMode::All),
            state.filter_mode == SessionFilterMode::All,
            filter_focused,
        ),
    ]
}

fn toolbar_value(label: &'static str, active: bool, focused: bool) -> Span<'static> {
    if active {
        let value = format!("[{label}]");
        if focused {
            value.magenta()
        } else {
            value.into()
        }
    } else {
        format!(" {label} ").dim()
    }
}

fn filter_mode_label(filter_mode: SessionFilterMode) -> &'static str {
    match filter_mode {
        SessionFilterMode::Cwd => "Cwd",
        SessionFilterMode::All => "All",
    }
}

struct PickerFooterHint {
    key: String,
    wide_label: String,
    compact_label: String,
    priority: u8,
}

fn render_picker_footer(
    frame: &mut crate::custom_terminal::Frame,
    area: Rect,
    state: &PickerState,
    list_height: u16,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let separator = Rect::new(area.x, area.y, area.width, 1);
    render_picker_footer_separator(
        frame,
        separator,
        picker_footer_progress_label(state, list_height, area.width),
    );

    let lines = footer_hint_lines(state, area.width);
    for (idx, line) in lines.into_iter().enumerate() {
        let y = area.y.saturating_add(1 + idx as u16);
        if y >= area.bottom() {
            break;
        }
        frame.render_widget_ref(&line, Rect::new(area.x, y, area.width, 1));
    }
}

fn render_picker_footer_separator(
    frame: &mut crate::custom_terminal::Frame,
    area: Rect,
    progress_label: String,
) {
    if area.width == 0 {
        return;
    }

    let separator = "─".repeat(area.width as usize);
    frame.render_widget_ref(&Line::from(separator.dim()), area);

    let progress_width = UnicodeWidthStr::width(progress_label.as_str()) as u16;
    if progress_width < area.width {
        let percent_area = Rect::new(
            area.x + area.width - progress_width - 1,
            area.y,
            progress_width,
            1,
        );
        frame.render_widget_ref(&Line::from(progress_label.dim()), percent_area);
    }
}

fn picker_footer_progress_label(state: &PickerState, list_height: u16, width: u16) -> String {
    let position = if state.filtered_rows.is_empty() {
        0
    } else {
        state.selected.saturating_add(1)
    };
    let total = if state.pagination.is_loading() {
        format!("{}…", state.filtered_rows.len())
    } else {
        state.filtered_rows.len().to_string()
    };
    let percent = picker_footer_percent(state, list_height);
    let labels = [
        format!(" {position} / {total} · {percent}% "),
        format!(" {position}/{total} · {percent}% "),
        format!(" {percent}% "),
    ];
    labels
        .into_iter()
        .find(|label| UnicodeWidthStr::width(label.as_str()) < width as usize)
        .unwrap_or_default()
}

fn picker_footer_percent(state: &PickerState, list_height: u16) -> u8 {
    if state.pagination.is_loading() {
        return state.frozen_footer_percent.unwrap_or_else(|| {
            if state.filtered_rows.is_empty() {
                0
            } else {
                picker_footer_scroll_percent(state, list_height)
            }
        });
    }

    picker_footer_scroll_percent(state, list_height)
}

fn picker_footer_scroll_percent(state: &PickerState, list_height: u16) -> u8 {
    if state.filtered_rows.is_empty() {
        return 100;
    }

    let content_rows = state.available_content_rows(list_height as usize);
    let total_height =
        state.rendered_height_between(/*start*/ 0, state.filtered_rows.len() - 1);
    let max_scroll = total_height.saturating_sub(content_rows);
    if max_scroll == 0 {
        return 100;
    }
    let remaining_height =
        state.rendered_height_between(state.scroll_top, state.filtered_rows.len() - 1);
    if remaining_height <= content_rows {
        return 100;
    }

    let skipped_height = if state.scroll_top == 0 {
        0
    } else {
        state.rendered_height_between(/*start*/ 0, state.scroll_top - 1)
    };
    (((skipped_height.min(max_scroll)) as f32 / max_scroll as f32) * 100.0).round() as u8
}

fn footer_hint_lines(state: &PickerState, width: u16) -> Vec<Line<'static>> {
    if state.is_transcript_loading() {
        let hints = [
            PickerFooterHint {
                key: "loading".to_string(),
                wide_label: String::from("transcript"),
                compact_label: String::from("transcript"),
                priority: 0,
            },
            PickerFooterHint {
                key: "ctrl+c".to_string(),
                wide_label: String::from("quit"),
                compact_label: String::from("quit"),
                priority: 1,
            },
        ];
        let line = fit_footer_hints(&hints, FooterHintLabelMode::Wide, width)
            .or_else(|| fit_footer_hints(&hints, FooterHintLabelMode::Compact, width))
            .or_else(|| fit_footer_hints(&hints, FooterHintLabelMode::KeyOnly, width))
            .unwrap_or_default();
        return vec![line, Line::default()];
    }

    let action_label = if state.status == SessionStatus::Archived {
        "restore"
    } else {
        state.action.action_label()
    };
    let (esc_label, esc_compact_label) = if state.query.is_empty() {
        match state.launch_context {
            SessionPickerLaunchContext::Startup | SessionPickerLaunchContext::AgentsDashboard => {
                ("start new", "new")
            }
            SessionPickerLaunchContext::ExistingSession { .. } => ("exit", "exit"),
        }
    } else {
        ("clear search", "clear")
    };
    let ctrl_c_label = match state.launch_context {
        SessionPickerLaunchContext::Startup | SessionPickerLaunchContext::AgentsDashboard => "quit",
        SessionPickerLaunchContext::ExistingSession { .. } => "exit",
    };
    let density_label = match state.density {
        SessionListDensity::Comfortable => "dense view",
        SessionListDensity::Dense => "comfortable view",
    };
    let density_compact_label = match state.density {
        SessionListDensity::Comfortable => "dense",
        SessionListDensity::Dense => "comfy",
    };
    let mut first_row_hints = Vec::new();
    if let Some(accept) = state.list_keymap.primary_hint(ListAction::Accept) {
        first_row_hints.push(PickerFooterHint {
            key: accept.display_label(),
            wide_label: action_label.to_string(),
            compact_label: action_label.to_string(),
            priority: 0,
        });
    }
    if !state.filtered_rows.is_empty() && state.archive_shortcut_available() {
        first_row_hints.push(PickerFooterHint {
            key: "ctrl+a".to_string(),
            wide_label: String::from("archive"),
            compact_label: String::from("archive"),
            priority: 2,
        });
    }
    if let Some(cancel) = state.list_keymap.primary_hint(ListAction::Cancel) {
        first_row_hints.push(PickerFooterHint {
            key: cancel.display_label(),
            wide_label: esc_label.to_string(),
            compact_label: esc_compact_label.to_string(),
            priority: 1,
        });
    }
    if state.is_agents_dashboard() {
        first_row_hints.push(PickerFooterHint {
            key: "ctrl+g".to_string(),
            wide_label: String::from("change grouping"),
            compact_label: String::from("group"),
            priority: 3,
        });
    }
    first_row_hints.push(PickerFooterHint {
        key: "ctrl+c".to_string(),
        wide_label: ctrl_c_label.to_string(),
        compact_label: ctrl_c_label.to_string(),
        priority: 2,
    });
    if !state.is_agents_dashboard() {
        first_row_hints.push(PickerFooterHint {
            key: "tab".to_string(),
            wide_label: String::from("focus sort/filter"),
            compact_label: String::from("focus"),
            priority: 7,
        });
        let option_keys = [ListAction::MoveLeft, ListAction::MoveRight]
            .into_iter()
            .filter_map(|action| state.list_keymap.primary_hint(action))
            .map(super::key_hint::ShortcutHint::display_label)
            .collect::<Vec<_>>()
            .join("/");
        if !option_keys.is_empty() {
            first_row_hints.push(PickerFooterHint {
                key: option_keys,
                wide_label: String::from("change option"),
                compact_label: String::from("option"),
                priority: 8,
            });
        }
    }
    let mut second_row_hints = vec![
        PickerFooterHint {
            key: "ctrl+o".to_string(),
            wide_label: density_label.to_string(),
            compact_label: density_compact_label.to_string(),
            priority: 3,
        },
        PickerFooterHint {
            key: "ctrl+t".to_string(),
            wide_label: String::from("transcript"),
            compact_label: String::from("preview"),
            priority: 4,
        },
        PickerFooterHint {
            key: "ctrl+e".to_string(),
            wide_label: String::from("expand"),
            compact_label: String::from("exp"),
            priority: 6,
        },
    ];
    let browse_keys = [ListAction::MoveUp, ListAction::MoveDown]
        .into_iter()
        .filter_map(|action| state.list_keymap.primary_hint(action))
        .map(super::key_hint::ShortcutHint::display_label)
        .collect::<Vec<_>>()
        .join("/");
    if !browse_keys.is_empty() {
        second_row_hints.push(PickerFooterHint {
            key: browse_keys,
            wide_label: String::from("browse"),
            compact_label: String::from("browse"),
            priority: 5,
        });
    }

    vec![
        hint_line_for_row(&first_row_hints, width),
        hint_line_for_row(&second_row_hints, width),
    ]
}

fn hint_line_for_row(hints: &[PickerFooterHint], width: u16) -> Line<'static> {
    if width >= FOOTER_COMPACT_BREAKPOINT
        && let Some(line) = fit_footer_hints(hints, FooterHintLabelMode::Wide, width)
    {
        return line;
    }
    if let Some(line) = fit_footer_hints(hints, FooterHintLabelMode::Compact, width) {
        return line;
    }
    if let Some(line) = fit_footer_hints(hints, FooterHintLabelMode::KeyOnly, width) {
        return line;
    }

    let mut retained = (0..hints.len()).collect::<Vec<_>>();
    retained.sort_by_key(|idx| hints[*idx].priority);
    for retain_count in (1..=retained.len()).rev() {
        let mut candidate_indices = retained[..retain_count].to_vec();
        candidate_indices.sort_unstable();
        let candidate = candidate_indices
            .iter()
            .map(|idx| &hints[*idx])
            .collect::<Vec<_>>();
        if let Some(line) = fit_footer_hint_refs(&candidate, FooterHintLabelMode::KeyOnly, width) {
            return line;
        }
    }
    Line::default()
}

fn render_transcript_loading_overlay(frame: &mut crate::custom_terminal::Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let message = "Loading transcript…";
    let message_width = UnicodeWidthStr::width(message) as u16;
    let overlay_width = if area.width >= message_width.saturating_add(10) {
        message_width + 10
    } else {
        area.width
    };
    let overlay_height = if area.height >= 3 { 3 } else { 1 };
    let overlay = Rect::new(
        area.x + area.width.saturating_sub(overlay_width) / 2,
        area.y + area.height.saturating_sub(overlay_height) / 2,
        overlay_width,
        overlay_height,
    );
    let style = transcript_loading_overlay_style();
    for y in overlay.y..overlay.bottom() {
        for x in overlay.x..overlay.right() {
            frame.buffer[(x, y)].set_symbol(" ").set_style(style);
        }
    }

    let message = truncate_text(message, overlay.width as usize);
    let message_width = UnicodeWidthStr::width(message.as_str()) as u16;
    let line = Rect::new(
        overlay.x + overlay.width.saturating_sub(message_width) / 2,
        overlay.y + overlay.height / 2,
        message_width.min(overlay.width),
        1,
    );
    frame.render_widget_ref(&Line::from(message.bold()), line);
}

fn transcript_loading_overlay_style() -> Style {
    let Some(bg) = default_bg() else {
        return Style::default().bg(Color::DarkGray);
    };
    let (overlay, alpha) = if is_light(bg) {
        ((0, 0, 0), 0.08)
    } else {
        ((255, 255, 255), 0.14)
    };
    Style::default().bg(best_color(blend(overlay, bg, alpha)))
}

#[derive(Clone, Copy)]
enum FooterHintLabelMode {
    Wide,
    Compact,
    KeyOnly,
}

fn fit_footer_hints(
    hints: &[PickerFooterHint],
    mode: FooterHintLabelMode,
    width: u16,
) -> Option<Line<'static>> {
    let hint_refs = hints.iter().collect::<Vec<_>>();
    fit_footer_hint_refs(&hint_refs, mode, width)
}

fn fit_footer_hint_refs(
    hints: &[&PickerFooterHint],
    mode: FooterHintLabelMode,
    width: u16,
) -> Option<Line<'static>> {
    let gap_width = FOOTER_HINT_GAP;
    if footer_hints_width(hints, mode, gap_width) > width as usize {
        return None;
    }

    let mut spans = vec![
        " ".repeat(FOOTER_HINT_LEFT_PADDING)
            .set_style(footer_hint_label_style()),
    ];
    for (idx, hint) in hints.iter().enumerate() {
        if idx > 0 {
            spans.push(" ".repeat(gap_width).set_style(footer_hint_label_style()));
        }
        spans.push(hint.key.clone().set_style(footer_hint_key_style()));
        let label = match mode {
            FooterHintLabelMode::Wide => Some(hint.wide_label.as_str()),
            FooterHintLabelMode::Compact => Some(hint.compact_label.as_str()),
            FooterHintLabelMode::KeyOnly => None,
        };
        if let Some(label) = label {
            spans.push(" ".set_style(footer_hint_label_style()));
            spans.push(label.to_string().set_style(footer_hint_label_style()));
        }
    }
    Some(spans.into())
}

fn footer_hint_key_style() -> Style {
    if default_bg().is_some_and(is_light) {
        Style::default().fg(Color::Black)
    } else {
        Style::default()
    }
}

fn footer_hint_label_style() -> Style {
    if default_bg().is_some_and(is_light) {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().dim()
    }
}

fn footer_hints_width(
    hints: &[&PickerFooterHint],
    mode: FooterHintLabelMode,
    gap_width: usize,
) -> usize {
    FOOTER_HINT_LEFT_PADDING
        + hints
            .iter()
            .enumerate()
            .map(|(idx, hint)| {
                let label_width = match mode {
                    FooterHintLabelMode::Wide => {
                        1 + UnicodeWidthStr::width(hint.wide_label.as_str())
                    }
                    FooterHintLabelMode::Compact => {
                        1 + UnicodeWidthStr::width(hint.compact_label.as_str())
                    }
                    FooterHintLabelMode::KeyOnly => 0,
                };
                let hint_width = UnicodeWidthStr::width(hint.key.as_str()) + label_width;
                if idx == 0 {
                    hint_width
                } else {
                    hint_width + gap_width
                }
            })
            .sum::<usize>()
}

fn render_list(frame: &mut crate::custom_terminal::Frame, area: Rect, state: &PickerState) {
    if area.height == 0 {
        return;
    }
    Clear.render(area, frame.buffer);

    let rows = &state.filtered_rows;
    if rows.is_empty() {
        let message = render_empty_state_line(state);
        frame.render_widget_ref(&message, area);
        return;
    }

    let show_more_above = state.has_more_above();
    let show_more_below = state.has_more_below(area.height as usize);
    let content_area = Rect::new(
        area.x,
        area.y.saturating_add(u16::from(show_more_above)),
        area.width,
        area.height
            .saturating_sub(u16::from(show_more_above))
            .saturating_sub(u16::from(show_more_below)),
    );
    if show_more_above {
        frame.render_widget_ref(
            &more_line("↑ more"),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }

    if state.is_agents_dashboard() {
        let mut lines = Vec::new();
        for (row_idx, row) in rows.iter().enumerate() {
            if state.starts_dashboard_group(row_idx, /*viewport_start*/ 0) {
                let label = state.dashboard_group_label(row);
                lines.push(vec![label.bold(), "  ─".dim()].into());
            }
            let is_selected = row_idx == state.selected;
            let is_expanded =
                is_selected && row.thread_id.is_some() && state.expanded_thread_id == row.thread_id;
            lines.extend(render_session_lines(
                row,
                state,
                is_selected,
                is_expanded,
                /*is_zebra*/ false,
                area.width,
            ));
            if state.density == SessionListDensity::Comfortable && row_idx + 1 < rows.len() {
                lines.push(Line::default());
            }
        }

        let offset = state.dashboard_scroll_offset.min(lines.len());
        for (line, y) in lines[offset..]
            .iter()
            .zip(content_area.y..content_area.bottom())
        {
            frame.render_widget_ref(line, Rect::new(area.x, y, area.width, 1));
        }
        if show_more_below {
            let label = if state.pagination.is_loading() {
                "↓ loading more"
            } else {
                "↓ more"
            };
            frame.render_widget_ref(
                &more_line(label),
                Rect::new(
                    area.x,
                    area.y.saturating_add(area.height.saturating_sub(1)),
                    area.width,
                    1,
                ),
            );
        }
        return;
    }

    let start = state.scroll_top.min(rows.len().saturating_sub(1));
    let mut y = content_area.y;
    for (idx, row) in rows[start..].iter().enumerate() {
        if y >= content_area.y.saturating_add(content_area.height) {
            break;
        }
        let row_idx = start + idx;
        if state.starts_dashboard_group(row_idx, start) {
            if y >= content_area.y.saturating_add(content_area.height) {
                break;
            }
            let label = state.dashboard_group_label(row);
            let header: Line = vec![label.bold(), "  ─".dim()].into();
            frame.render_widget_ref(&header, Rect::new(area.x, y, area.width, 1));
            y = y.saturating_add(1);
        }
        let is_selected = row_idx == state.selected;
        let is_expanded =
            is_selected && row.thread_id.is_some() && state.expanded_thread_id == row.thread_id;
        let is_zebra = row_idx.is_multiple_of(2);
        for line in render_session_lines(row, state, is_selected, is_expanded, is_zebra, area.width)
        {
            if y >= content_area.y.saturating_add(content_area.height) {
                break;
            }
            frame.render_widget_ref(&line, Rect::new(area.x, y, area.width, 1));
            y = y.saturating_add(1);
        }
        if state.density == SessionListDensity::Comfortable
            && y < content_area.y.saturating_add(content_area.height)
            && start + idx + 1 < rows.len()
        {
            y = y.saturating_add(1);
        }
    }

    if state.pagination.is_loading() && y < content_area.y.saturating_add(content_area.height) {
        let loading_line: Line = vec!["  ".into(), "Loading older sessions…".italic().dim()].into();
        let rect = Rect::new(area.x, y, area.width, 1);
        frame.render_widget_ref(&loading_line, rect);
    }
    if show_more_below {
        let label = if state.pagination.is_loading() {
            "↓ loading more"
        } else {
            "↓ more"
        };
        frame.render_widget_ref(
            &more_line(label),
            Rect::new(
                area.x,
                area.y.saturating_add(area.height.saturating_sub(1)),
                area.width,
                1,
            ),
        );
    }
}

fn more_line(label: &'static str) -> Line<'static> {
    vec![label.dim()].into()
}

fn render_session_lines(
    row: &Row,
    state: &PickerState,
    is_selected: bool,
    is_expanded: bool,
    is_zebra: bool,
    width: u16,
) -> Vec<Line<'static>> {
    match state.density {
        SessionListDensity::Comfortable => {
            render_comfortable_session_lines(row, state, is_selected, is_expanded, is_zebra, width)
        }
        SessionListDensity::Dense => {
            render_dense_session_lines(row, state, is_selected, is_expanded, is_zebra, width)
        }
    }
}

fn render_comfortable_session_lines(
    row: &Row,
    state: &PickerState,
    is_selected: bool,
    is_expanded: bool,
    is_zebra: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let dashboard = state.is_agents_dashboard();
    let marker = if dashboard {
        dashboard_selection_marker(is_selected, is_expanded)
    } else {
        selection_marker(is_selected, is_expanded)
    };
    let status_width = usize::from(dashboard) * DASHBOARD_STATUS_COLUMN_WIDTH;
    let title = truncate_text(
        row.display_preview(),
        width.saturating_sub(2) as usize - status_width.min(width.saturating_sub(2) as usize),
    );
    let title = if dashboard {
        title.set_style(dashboard_row_text_style(is_selected))
    } else if is_selected {
        selected_session_title_span(title)
    } else {
        title.into()
    };
    let mut title_spans = vec![marker];
    if dashboard {
        title_spans.push(dashboard_status_column(row.dashboard_status));
    }
    title_spans.push(title);
    let title_line = Line::from(title_spans);
    let mut lines = vec![title_line];
    if state.is_agents_dashboard()
        && !is_expanded
        && let Some(thread_id) = row.thread_id
        && let Some(subtitle) = if state.dashboard_system_errors.contains(&thread_id) {
            Some("Thread stopped because of a system error".red())
        } else {
            dashboard_subtitle(state.transcript_previews.get(&thread_id))
        }
    {
        lines.push(apply_dashboard_text_style(
            vec!["  ".into(), subtitle].into(),
            is_selected,
        ));
    }
    let row_style = if dashboard {
        None
    } else if is_selected {
        Some(dense_selected_style())
    } else if is_zebra {
        Some(dense_zebra_style())
    } else {
        None
    };
    if !dashboard && let Some(style) = row_style {
        lines = apply_session_row_background(lines, style, width);
    }
    if is_expanded {
        let transcript_lines = render_transcript_preview_lines(row, state, width);
        if dashboard {
            lines.extend(
                transcript_lines
                    .into_iter()
                    .map(|line| apply_dashboard_text_style(line, is_selected)),
            );
        } else {
            lines.extend(transcript_lines);
        }
        return lines;
    }

    let reference = state.relative_time_reference.unwrap_or_else(Utc::now);
    let created = format_relative_time(reference, row.created_at);
    let updated = format_relative_time(reference, row.updated_at.or(row.created_at));
    let branch = row.git_branch.as_deref();
    let cwd = row
        .cwd
        .as_ref()
        .map(|path| format_directory_display(path, /*max_width*/ None));
    let footer_lines = render_footer_lines(
        state.sort_key,
        &created,
        &updated,
        branch,
        cwd.as_deref(),
        state.filter_mode == SessionFilterMode::All
            && !(state.is_agents_dashboard()
                && state.dashboard_group_mode == DashboardGroupMode::Project),
        width,
    );
    if dashboard {
        lines.extend(
            footer_lines
                .into_iter()
                .map(|line| apply_dashboard_text_style(line, is_selected)),
        );
    } else if let Some(style) = row_style {
        lines.extend(apply_session_row_background(footer_lines, style, width));
    } else {
        lines.extend(footer_lines);
    }
    lines
}

fn dashboard_subtitle(preview: Option<&TranscriptPreviewState>) -> Option<Span<'static>> {
    match preview {
        Some(TranscriptPreviewState::Loading) => Some("Loading recent activity…".italic().dim()),
        Some(TranscriptPreviewState::Failed) => None,
        Some(TranscriptPreviewState::Loaded(lines)) => lines
            .iter()
            .rev()
            .find(|line| matches!(line.speaker, TranscriptPreviewSpeaker::Assistant))
            .or_else(|| lines.last())
            .map(|line| line.text.clone().dim()),
        None => None,
    }
}

fn apply_session_row_background(
    lines: Vec<Line<'static>>,
    style: Style,
    width: u16,
) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| apply_line_background(line, style, width))
        .collect()
}

fn apply_line_background(mut line: Line<'static>, style: Style, width: u16) -> Line<'static> {
    let padding = (width as usize).saturating_sub(line.width());
    if padding > 0 {
        line.spans.push(" ".repeat(padding).set_style(style));
    }
    line.style = line.style.patch(style);
    for span in &mut line.spans {
        span.style = span.style.patch(style);
    }
    line
}

fn render_dense_session_lines(
    row: &Row,
    state: &PickerState,
    is_selected: bool,
    is_expanded: bool,
    is_zebra: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let marker = if state.is_agents_dashboard() {
        dashboard_selection_marker(is_selected, is_expanded)
    } else {
        selection_marker(is_selected, is_expanded)
    };
    let reference = state.relative_time_reference.unwrap_or_else(Utc::now);
    let created = format_relative_time(reference, row.created_at);
    let updated = format_relative_time(reference, row.updated_at.or(row.created_at));
    let date = match state.sort_key {
        ThreadSortKey::CreatedAt => created,
        ThreadSortKey::UpdatedAt | ThreadSortKey::RecencyAt | ThreadSortKey::SectionPosition => {
            updated
        }
    };
    let title = row.display_preview().to_string();
    let mut lines = vec![dense_summary_line(DenseSummaryInput {
        marker,
        date: &date,
        title: &title,
        dashboard_status: row.dashboard_status,
        is_dashboard: state.is_agents_dashboard(),
        is_selected,
        is_zebra,
        width,
    })];
    if is_expanded {
        let transcript_lines = render_transcript_preview_lines(row, state, width);
        if state.is_agents_dashboard() {
            lines.extend(
                transcript_lines
                    .into_iter()
                    .map(|line| apply_dashboard_text_style(line, is_selected)),
            );
        } else {
            lines.extend(transcript_lines);
        }
    }
    lines
}

fn dashboard_status_column(status: Option<DashboardStatus>) -> Span<'static> {
    let Some(status) = status else {
        return " ".repeat(DASHBOARD_STATUS_COLUMN_WIDTH).dim();
    };
    let label = format!("[{}]", status.label());
    let padding = DASHBOARD_STATUS_COLUMN_WIDTH.saturating_sub(label.width());
    let label = format!("{label}{}", " ".repeat(padding));
    match status {
        DashboardStatus::NeedsInput => label.red().bold(),
        DashboardStatus::Working => label.cyan(),
        DashboardStatus::Idle => label.green(),
        DashboardStatus::Done => label.dim(),
    }
}

fn dashboard_row_text_style(is_selected: bool) -> Style {
    if is_selected {
        Style::default().fg(if default_bg().is_some_and(is_light) {
            Color::Black
        } else {
            Color::White
        })
    } else {
        Style::default().dim()
    }
}

fn dashboard_selection_marker(is_selected: bool, is_expanded: bool) -> Span<'static> {
    let marker = match (is_selected, is_expanded) {
        (true, true) => "⌄ ",
        (true, false) => "❯ ",
        (false, _) => "  ",
    };
    marker.set_style(dashboard_row_text_style(is_selected))
}

fn apply_dashboard_text_style(mut line: Line<'static>, is_selected: bool) -> Line<'static> {
    let style = dashboard_row_text_style(is_selected);
    for span in &mut line.spans {
        span.style = if is_selected {
            style
        } else {
            span.style.patch(style)
        };
    }
    line
}

struct DenseSummaryInput<'a> {
    marker: Span<'static>,
    date: &'a str,
    title: &'a str,
    dashboard_status: Option<DashboardStatus>,
    is_dashboard: bool,
    is_selected: bool,
    is_zebra: bool,
    width: u16,
}

fn dense_summary_line(input: DenseSummaryInput<'_>) -> Line<'static> {
    let marker_width = input.marker.width();
    let available = (input.width as usize).saturating_sub(marker_width);
    let status_width = usize::from(input.is_dashboard) * DASHBOARD_STATUS_COLUMN_WIDTH;
    let columns = dense_columns(available.saturating_sub(status_width));
    let title = if input.is_dashboard {
        truncate_text(input.title, columns.title_width)
            .set_style(dashboard_row_text_style(input.is_selected))
    } else if input.is_selected {
        selected_session_title_span(dense_column_text(input.title, columns.title_width))
    } else {
        dense_column_text(input.title, columns.title_width).into()
    };

    let date = if input.is_dashboard {
        dense_column_text(input.date, columns.date_width)
            .set_style(dashboard_row_text_style(input.is_selected))
    } else {
        dense_column_text(input.date, columns.date_width).dim()
    };
    let mut spans = vec![input.marker, date];
    if input.is_dashboard {
        spans.push(dashboard_status_column(input.dashboard_status));
    }
    spans.push(title);
    let mut line = Line::from(spans);
    if !input.is_dashboard && input.is_selected {
        let padding = (input.width as usize).saturating_sub(line.width());
        if padding > 0 {
            line.spans
                .push(" ".repeat(padding).set_style(dense_selected_style()));
        }
        line = line.style(dense_selected_style());
    } else if !input.is_dashboard && input.is_zebra {
        let padding = (input.width as usize).saturating_sub(line.width());
        if padding > 0 {
            line.spans
                .push(" ".repeat(padding).set_style(dense_zebra_style()));
        }
        line = line.style(dense_zebra_style());
    }
    line
}

struct DenseColumns {
    date_width: usize,
    title_width: usize,
}

fn dense_columns(width: usize) -> DenseColumns {
    let date_width = SESSION_META_DATE_WIDTH;
    DenseColumns {
        date_width,
        title_width: width.saturating_sub(date_width),
    }
}

fn dense_zebra_style() -> Style {
    dense_row_background_style(/*selected*/ false)
}

fn dense_selected_style() -> Style {
    selected_session_style().patch(dense_row_background_style(/*selected*/ true))
}

fn dense_row_background_style(selected: bool) -> Style {
    let Some(bg) = default_bg() else {
        return Style::default();
    };
    let (overlay, alpha) = if is_light(bg) {
        ((0, 0, 0), if selected { 0.12 } else { 0.04 })
    } else {
        ((255, 255, 255), if selected { 0.12 } else { 0.055 })
    };
    Style::default().bg(best_color(blend(overlay, bg, alpha)))
}

fn dense_column_text(text: &str, width: usize) -> String {
    let text = truncate_text(text, width.saturating_sub(1));
    let padding = width.saturating_sub(UnicodeWidthStr::width(text.as_str()));
    format!("{text}{}", " ".repeat(padding))
}

fn selection_marker(is_selected: bool, is_expanded: bool) -> Span<'static> {
    match (is_selected, is_expanded) {
        (true, true) => "⌄ ".set_style(selected_session_style().bold()),
        (true, false) => "❯ ".set_style(selected_session_style().bold()),
        (false, _) => "  ".into(),
    }
}

fn selected_session_style() -> Style {
    if default_bg().is_some_and(is_light) {
        Style::default().fg(Color::Magenta)
    } else {
        Style::default().fg(Color::Yellow)
    }
}

fn selected_session_title_span(title: String) -> Span<'static> {
    title.set_style(selected_session_style())
}

fn render_footer_lines(
    sort_key: ThreadSortKey,
    created: &str,
    updated: &str,
    branch: Option<&str>,
    cwd: Option<&str>,
    show_cwd: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let date = match sort_key {
        ThreadSortKey::CreatedAt => created,
        ThreadSortKey::UpdatedAt | ThreadSortKey::RecencyAt | ThreadSortKey::SectionPosition => {
            updated
        }
    };
    let mut parts = vec![FooterPart::Date(date.to_string())];
    if show_cwd {
        parts.push(FooterPart::Cwd(cwd.map(str::to_string)));
    }
    parts.push(FooterPart::Branch(branch.map(str::to_string)));
    pack_footer_parts(parts, width)
}

enum FooterPart {
    Date(String),
    Branch(Option<String>),
    Cwd(Option<String>),
}

impl FooterPart {
    fn text(&self) -> &str {
        match self {
            FooterPart::Date(text) => text,
            FooterPart::Branch(Some(text)) | FooterPart::Cwd(Some(text)) => text,
            FooterPart::Branch(None) => "no branch",
            FooterPart::Cwd(None) => "no cwd",
        }
    }

    fn prefix(&self) -> Option<&'static str> {
        match self {
            FooterPart::Date(_) => None,
            FooterPart::Branch(_) => Some(SESSION_META_BRANCH_ICON),
            FooterPart::Cwd(_) => Some(SESSION_META_CWD_ICON),
        }
    }
}

fn pack_footer_parts(parts: Vec<FooterPart>, width: u16) -> Vec<Line<'static>> {
    let available_width = width as usize;
    if available_width <= SESSION_META_INDENT_WIDTH {
        return Vec::new();
    }
    let cwd_width = cwd_column_width(available_width);
    let all_parts_width = footer_parts_width(&parts, cwd_width);
    if all_parts_width <= available_width {
        return vec![footer_line(parts, available_width, cwd_width)];
    }

    let mut lines = Vec::with_capacity(parts.len());
    let mut current_parts = Vec::new();
    for part in parts {
        let mut candidate_parts = std::mem::take(&mut current_parts);
        candidate_parts.push(part);
        if candidate_parts.len() > 1
            && footer_parts_width(&candidate_parts, cwd_width) > available_width
        {
            let previous_parts = candidate_parts
                .drain(..candidate_parts.len().saturating_sub(1))
                .collect();
            lines.push(footer_line(previous_parts, available_width, cwd_width));
        }
        current_parts = candidate_parts;
    }
    if !current_parts.is_empty() {
        lines.push(footer_line(current_parts, available_width, cwd_width));
    }
    lines
}

fn cwd_column_width(width: usize) -> usize {
    let available = width.saturating_sub(
        SESSION_META_INDENT_WIDTH + SESSION_META_DATE_WIDTH + 2 * SESSION_META_FIELD_GAP_WIDTH,
    );
    (available / 2).clamp(SESSION_META_MIN_CWD_WIDTH, SESSION_META_MAX_CWD_WIDTH)
}

fn footer_parts_width(parts: &[FooterPart], cwd_width: usize) -> usize {
    let content_width: usize = parts
        .iter()
        .enumerate()
        .map(|(idx, part)| footer_part_width(part, idx + 1 < parts.len(), cwd_width))
        .sum();
    SESSION_META_INDENT_WIDTH + content_width
}

fn footer_part_width(part: &FooterPart, padded: bool, cwd_width: usize) -> usize {
    let prefix_width = part.prefix().map_or(0, UnicodeWidthStr::width);
    let prefix_gap_width = usize::from(part.prefix().is_some() && !part.text().is_empty());
    let text_width = UnicodeWidthStr::width(part.text());
    let actual_width = prefix_width + prefix_gap_width + text_width;
    match part {
        FooterPart::Date(_) if padded => SESSION_META_DATE_WIDTH.max(actual_width),
        FooterPart::Cwd(_) if padded => cwd_width,
        _ => actual_width,
    }
}

fn footer_line(parts: Vec<FooterPart>, width: usize, cwd_width: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec!["  ".into()];
    let mut remaining_width = width.saturating_sub(SESSION_META_INDENT_WIDTH);
    let part_count = parts.len();
    for (idx, part) in parts.into_iter().enumerate() {
        if idx > 0 {
            let gap_width = SESSION_META_FIELD_GAP_WIDTH.min(remaining_width);
            if gap_width > 0 {
                spans.push(" ".repeat(gap_width).dim());
                remaining_width = remaining_width.saturating_sub(gap_width);
            }
        }
        let padded = idx + 1 < part_count;
        let target_width = match part {
            FooterPart::Date(_) if padded => Some(SESSION_META_DATE_WIDTH),
            FooterPart::Cwd(_) if padded => Some(cwd_width),
            FooterPart::Date(_) | FooterPart::Branch(_) | FooterPart::Cwd(_) => None,
        };
        let used_width = push_footer_part(&mut spans, part, target_width, remaining_width);
        remaining_width = remaining_width.saturating_sub(used_width);
        if let Some(target_width) = target_width {
            let padding = target_width.saturating_sub(used_width);
            if padding > 0 {
                spans.push(" ".repeat(padding).dim());
                remaining_width = remaining_width.saturating_sub(padding);
            }
        }
    }
    spans.into()
}

fn push_footer_part(
    spans: &mut Vec<Span<'static>>,
    part: FooterPart,
    target_width: Option<usize>,
    available_width: usize,
) -> usize {
    let text = part.text().to_string();
    let Some(prefix) = part.prefix() else {
        let text = truncate_text(&text, available_width);
        let width = UnicodeWidthStr::width(text.as_str());
        spans.push(text.dim());
        return width;
    };

    let prefix_width = UnicodeWidthStr::width(prefix);
    if available_width <= prefix_width {
        let prefix = truncate_text(prefix, available_width);
        let width = UnicodeWidthStr::width(prefix.as_str());
        spans.push(prefix.dim());
        return width;
    }

    spans.push(prefix.dim());
    let mut used_width = prefix_width;
    if !text.is_empty() && used_width < available_width {
        spans.push(" ".dim());
        used_width += 1;
    }
    let text_width = target_width
        .unwrap_or(available_width)
        .saturating_sub(used_width)
        .min(available_width.saturating_sub(used_width));
    let text = truncate_text(&text, text_width);
    let rendered_text_width = UnicodeWidthStr::width(text.as_str());
    match part {
        FooterPart::Branch(None) | FooterPart::Cwd(None) => spans.push(text.dim().italic()),
        _ => spans.push(text.dim()),
    }
    used_width + rendered_text_width
}

fn render_transcript_preview_lines(
    row: &Row,
    state: &PickerState,
    width: u16,
) -> Vec<Line<'static>> {
    let mut details = render_expanded_session_details(row, state, width);
    let Some(thread_id) = row.thread_id else {
        return details;
    };
    let preview_lines = match state.transcript_previews.get(&thread_id) {
        Some(TranscriptPreviewState::Loading) => {
            vec![vec!["  │ ".dim(), "Loading recent transcript...".italic().dim()].into()]
        }
        Some(TranscriptPreviewState::Failed) => vec![
            vec![
                "  │ ".dim(),
                "Could not load transcript preview".italic().red(),
            ]
            .into(),
        ],
        Some(TranscriptPreviewState::Loaded(lines)) => {
            render_conversation_preview_lines(lines, width)
        }
        None => Vec::new(),
    };
    details.extend(preview_lines);
    details
}

fn render_expanded_session_details(
    row: &Row,
    state: &PickerState,
    width: u16,
) -> Vec<Line<'static>> {
    let reference = state.relative_time_reference.unwrap_or_else(Utc::now);
    let session = match (row.thread_name.as_deref(), row.thread_id) {
        (Some(thread_name), Some(thread_id)) => format!("{thread_name} ({thread_id})"),
        (Some(thread_name), None) => thread_name.to_string(),
        (None, Some(thread_id)) => thread_id.to_string(),
        (None, None) => "-".to_string(),
    };
    let directory = row
        .cwd
        .as_ref()
        .map(|path| format_directory_display(path, /*max_width*/ None))
        .unwrap_or_else(|| "-".to_string());
    let branch = row
        .git_branch
        .as_ref()
        .map(|branch| format!("{SESSION_META_BRANCH_ICON} {branch}"))
        .unwrap_or_else(|| format!("{SESSION_META_BRANCH_ICON} no branch"));

    vec![
        expanded_detail_line("Session:", &session, width),
        expanded_time_detail_line("Created:", reference, row.created_at, width),
        expanded_time_detail_line(
            "Updated:",
            reference,
            row.updated_at.or(row.created_at),
            width,
        ),
        expanded_detail_line("Directory:", &directory, width),
        expanded_detail_line("Branch:", &branch, width),
        vec!["  │".dim()].into(),
        vec!["  │ ".dim(), "Conversation:".dim()].into(),
    ]
}

fn render_conversation_preview_lines(
    lines: &[TranscriptPreviewLine],
    width: u16,
) -> Vec<Line<'static>> {
    if lines.is_empty() {
        return vec![
            vec![
                "  └ ".dim(),
                "No transcript preview available".italic().dim(),
            ]
            .into(),
        ];
    }

    let mut rendered = Vec::new();
    for line in lines {
        rendered.extend(render_transcript_content_lines(line, width));
    }
    let rendered_len = rendered.len();
    rendered
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            let prefix = if idx + 1 == rendered_len {
                "  └ "
            } else {
                "  │ "
            };
            prefix_transcript_line(prefix, line)
        })
        .collect()
}

fn render_transcript_content_lines(line: &TranscriptPreviewLine, width: u16) -> Vec<Line<'static>> {
    let content_width = width.saturating_sub(4) as usize;
    let lines = match line.speaker {
        TranscriptPreviewSpeaker::User => vec![conversation_content_line(
            Line::from(line.text.clone()),
            conversation_user_style(),
        )],
        TranscriptPreviewSpeaker::Assistant => {
            let mut lines = Vec::new();
            append_markdown(
                &line.text, /*width*/ None, /*cwd*/ None, &mut lines,
            );
            for line in &mut lines {
                *line = conversation_content_line(line.clone(), conversation_assistant_style());
            }
            lines
        }
    };
    adaptive_wrap_lines(lines, RtOptions::new(content_width.max(/*other*/ 1)))
}

fn conversation_content_line(mut line: Line<'static>, style: Style) -> Line<'static> {
    line.style = line.style.patch(style);
    for span in &mut line.spans {
        span.style = span.style.patch(style);
    }
    line
}

fn prefix_transcript_line(prefix: &'static str, line: Line<'static>) -> Line<'static> {
    let mut spans = vec![prefix.set_style(transcript_prefix_style(&line))];
    spans.extend(line.spans);
    Line::from(spans).style(line.style)
}

fn transcript_prefix_style(line: &Line<'_>) -> Style {
    let style = line
        .spans
        .iter()
        .find(|span| !span.content.trim().is_empty())
        .map(|span| line.style.patch(span.style))
        .unwrap_or(line.style);
    connector_style_from_content(style)
}

fn connector_style_from_content(style: Style) -> Style {
    Style {
        fg: style.fg,
        bg: style.bg,
        ..Style::default()
    }
}

fn conversation_assistant_style() -> Style {
    if default_bg().is_some_and(is_light) {
        Style::default().fg(Color::Gray)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn conversation_user_style() -> Style {
    if default_bg().is_some_and(is_light) {
        Style::default().fg(Color::DarkGray).italic()
    } else {
        Style::default().fg(Color::Gray).italic()
    }
}

fn expanded_detail_line(label: &'static str, value: &str, width: u16) -> Line<'static> {
    const LABEL_WIDTH: usize = 10;
    let prefix_width = 4;
    let gap_width = 2;
    let value_width = (width as usize)
        .saturating_sub(prefix_width + LABEL_WIDTH + gap_width)
        .max(1);
    vec![
        "  │ ".dim(),
        format!("{label:<LABEL_WIDTH$}").dim(),
        "  ".dim(),
        truncate_text(value, value_width).into(),
    ]
    .into()
}

fn expanded_time_detail_line(
    label: &'static str,
    reference: DateTime<Utc>,
    ts: Option<DateTime<Utc>>,
    width: u16,
) -> Line<'static> {
    let Some(ts) = ts else {
        return expanded_detail_line(label, "-", width);
    };
    let value = format!(
        "{} · {}",
        format_relative_time_long(reference, ts),
        format_timestamp(ts)
    );
    expanded_detail_line(label, &value, width)
}

fn format_relative_time(reference: DateTime<Utc>, ts: Option<DateTime<Utc>>) -> String {
    let Some(ts) = ts else {
        return "-".to_string();
    };
    let seconds = (reference - ts).num_seconds().max(0);
    if seconds == 0 {
        return "now".to_string();
    }
    if seconds < 60 {
        return format!("{seconds}s ago");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    format!("{days}d ago")
}

fn format_relative_time_long(reference: DateTime<Utc>, ts: DateTime<Utc>) -> String {
    let seconds = (reference - ts).num_seconds().max(0);
    if seconds == 0 {
        return "now".to_string();
    }
    if seconds < 60 {
        return plural_time(seconds, "second");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return plural_time(minutes, "minute");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return plural_time(hours, "hour");
    }
    plural_time(hours / 24, "day")
}

fn plural_time(value: i64, unit: &str) -> String {
    if value == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{value} {unit}s ago")
    }
}

fn format_timestamp(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn render_empty_state_line(state: &PickerState) -> Line<'static> {
    if !state.query.is_empty() {
        if state.search_state.is_active()
            || (state.pagination.is_loading() && state.pagination.next_cursor.is_some())
        {
            return vec!["Searching…".italic().dim()].into();
        }
        if state.pagination.reached_scan_cap {
            let msg = format!(
                "Search scanned first {} sessions; more may exist",
                state.pagination.num_scanned_files
            );
            return vec![Span::from(msg).italic().dim()].into();
        }
        return vec!["No results for your search".italic().dim()].into();
    }

    if state.pagination.is_loading() {
        if state.all_rows.is_empty() && state.pagination.num_scanned_files == 0 {
            return vec!["Loading sessions…".italic().dim()].into();
        }
        return vec!["Loading older sessions…".italic().dim()].into();
    }

    vec!["No sessions yet".italic().dim()].into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use codex_app_server_protocol::ThreadSourceKind;
    use codex_config::CONFIG_TOML_FILE;
    use codex_protocol::ThreadId;
    use codex_utils_absolute_path::test_support::PathBufExt;
    use codex_utils_absolute_path::test_support::test_path_buf;

    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::Mutex;
    use tempfile::tempdir;

    fn page(
        rows: Vec<Row>,
        next_cursor: Option<&str>,
        num_scanned_files: usize,
        reached_scan_cap: bool,
    ) -> PickerPage {
        PickerPage {
            rows,
            dashboard_system_errors: HashSet::new(),
            next_cursor: next_cursor.map(|cursor| PageCursor::AppServer(cursor.to_string())),
            num_scanned_files,
            reached_scan_cap,
        }
    }

    fn page_only_loader(loader: impl Fn(PageLoadRequest) + Send + Sync + 'static) -> PickerLoader {
        Arc::new(move |request| {
            if let PickerLoadRequest::Page(request) = request {
                loader(request);
            }
        })
    }

    async fn deliver_page(
        state: &mut PickerState,
        request: &PageLoadRequest,
        page: std::io::Result<PickerPage>,
    ) {
        state
            .handle_background_event(BackgroundEvent::Page {
                request_token: request.request_token,
                search_token: request.search_token,
                page,
            })
            .await
            .expect("page event");
    }

    fn ok_page(rows: Vec<Row>, next_cursor: Option<&str>) -> std::io::Result<PickerPage> {
        let n = rows.len();
        Ok(page(rows, next_cursor, n, /*reached_scan_cap*/ false))
    }

    fn make_row(path: &str, ts: &str, preview: &str) -> Row {
        let timestamp = parse_timestamp_str(ts).expect("timestamp should parse");
        Row {
            path: Some(PathBuf::from(path)),
            preview: preview.to_string(),
            thread_id: None,
            thread_name: None,
            created_at: Some(timestamp),
            updated_at: Some(timestamp),
            cwd: None,
            git_branch: None,
            dashboard_status: None,
        }
    }

    fn make_dashboard_row(cwd: &str, ts: &str, preview: &str, status: DashboardStatus) -> Row {
        let mut row = make_row(
            &format!("/tmp/{}.jsonl", preview.replace(' ', "-")),
            ts,
            preview,
        );
        row.thread_id = Some(ThreadId::new());
        row.cwd = Some(PathBuf::from(cwd));
        row.dashboard_status = Some(status);
        row
    }

    fn dashboard_state(rows: Vec<Row>) -> PickerState {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.launch_context = SessionPickerLaunchContext::AgentsDashboard;
        let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
        state.initialize_dashboard_composer(
            /*enhanced_keys_supported*/ true,
            /*disable_paste_burst*/ true,
            AppEventSender::new(app_event_tx),
            Path::new("/work/invocation"),
            &RuntimeKeymap::defaults(),
        );
        state.relative_time_reference =
            Some(parse_timestamp_str("2026-04-28T16:30:00Z").expect("relative time reference"));
        state.all_rows = rows;
        state.apply_filter();
        state
    }

    fn render_dashboard_list_snapshot(state: &PickerState, width: u16, height: u16) -> String {
        use crate::custom_terminal::Terminal;
        use crate::test_backend::VT100Backend;

        let backend = VT100Backend::new(width, height);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, width, height));
        {
            let mut frame = terminal.get_frame();
            let area = frame.area();
            render_list(&mut frame, area, state);
        }
        terminal.flush().expect("flush");
        terminal.backend().to_string()
    }

    fn render_dashboard_snapshot(state: &PickerState, width: u16, height: u16) -> String {
        use crate::custom_terminal::Terminal;
        use crate::test_backend::VT100Backend;

        let backend = VT100Backend::new(width, height);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, width, height));
        {
            let mut frame = terminal.get_frame();
            render_picker_frame(&mut frame, state);
        }
        terminal.flush().expect("flush");
        terminal.backend().to_string()
    }

    #[tokio::test]
    async fn dashboard_grouping_orders_rows_and_preserves_selection() {
        let selected = make_dashboard_row(
            "/work/older-project",
            "2026-04-28T15:00:00Z",
            "waiting for approval",
            DashboardStatus::NeedsInput,
        );
        let selected_id = selected.thread_id;
        let rows = vec![
            make_dashboard_row(
                "/work/newer-project",
                "2026-04-28T16:20:00Z",
                "running tests",
                DashboardStatus::Working,
            ),
            selected,
            make_dashboard_row(
                "/work/newer-project",
                "2026-04-28T14:00:00Z",
                "finished refactor",
                DashboardStatus::Done,
            ),
        ];
        let mut state = dashboard_state(rows);
        state.selected = state
            .filtered_rows
            .iter()
            .position(|row| row.thread_id == selected_id)
            .expect("selected row");

        state
            .handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL))
            .await
            .expect("group toggle");

        assert_eq!(state.dashboard_group_mode, DashboardGroupMode::Status);
        assert_eq!(
            state
                .filtered_rows
                .iter()
                .map(|row| row.dashboard_status.unwrap_or(DashboardStatus::Done))
                .collect::<Vec<_>>(),
            vec![
                DashboardStatus::NeedsInput,
                DashboardStatus::Working,
                DashboardStatus::Done,
                DashboardStatus::Done,
            ]
        );
        assert_eq!(state.filtered_rows[state.selected].thread_id, selected_id);
    }

    #[tokio::test]
    async fn dashboard_group_slash_command_changes_mode() {
        let mut state = dashboard_state(Vec::new());
        state
            .dashboard_composer
            .as_mut()
            .expect("dashboard composer")
            .composer
            .insert_str("/group status");

        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("group command");

        assert!(selection.is_none());
        assert_eq!(state.dashboard_group_mode, DashboardGroupMode::Status);
        assert!(
            state
                .dashboard_composer
                .as_ref()
                .expect("dashboard composer")
                .composer
                .is_empty()
        );
    }

    #[tokio::test]
    async fn dashboard_session_only_slash_command_explains_requirement() {
        let mut state = dashboard_state(Vec::new());
        state
            .dashboard_composer
            .as_mut()
            .expect("dashboard composer")
            .composer
            .insert_str("/status");

        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("status command");

        assert!(selection.is_none());
        assert_eq!(
            state.inline_error.as_deref(),
            Some("Open a session before running /status")
        );
    }

    #[tokio::test]
    async fn dashboard_mention_command_keeps_editing_in_composer() {
        let mut state = dashboard_state(Vec::new());
        state
            .dashboard_composer
            .as_mut()
            .expect("dashboard composer")
            .composer
            .insert_str("/mention");

        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("mention command");

        assert!(selection.is_none());
        assert_eq!(
            state
                .dashboard_composer
                .as_ref()
                .expect("dashboard composer")
                .composer
                .current_text_with_pending(),
            "@"
        );
    }

    #[tokio::test]
    async fn dashboard_nonempty_enter_starts_in_selected_project_with_full_payload() {
        let image_path = PathBuf::from("/tmp/dashboard-image.png");
        let rows = vec![make_dashboard_row(
            "/work/selected",
            "2026-04-28T16:20:00Z",
            "selected session",
            DashboardStatus::Idle,
        )];
        let mut state = dashboard_state(rows);
        let composer = &mut state
            .dashboard_composer
            .as_mut()
            .expect("dashboard composer")
            .composer;
        composer.insert_str("ship the dashboard");
        composer.attach_image(image_path.clone());

        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("submit dashboard prompt")
            .expect("session selection");

        let SessionSelection::StartFreshIn { cwd, user_message } = selection else {
            panic!("expected selected-project fresh session");
        };
        assert_eq!(cwd, PathBuf::from("/work/selected"));
        assert_eq!(
            user_message,
            crate::chatwidget::UserMessage {
                text: String::from("ship the dashboard[Image #1]"),
                local_images: vec![crate::bottom_pane::LocalImageAttachment {
                    placeholder: String::from("[Image #1]"),
                    path: image_path,
                }],
                remote_image_urls: Vec::new(),
                text_elements: vec![codex_protocol::user_input::TextElement::new(
                    (18..28).into(),
                    Some(String::from("[Image #1]")),
                )],
                mention_bindings: Vec::new(),
            }
        );
    }

    #[tokio::test]
    async fn dashboard_paste_burst_flush_keeps_prompt_current_and_allows_submit() {
        let mut state = dashboard_state(vec![make_dashboard_row(
            "/work/selected",
            "2026-04-28T16:20:00Z",
            "selected session",
            DashboardStatus::Idle,
        )]);
        state
            .dashboard_composer
            .as_mut()
            .expect("dashboard composer")
            .composer
            .set_disable_paste_burst(false);

        for character in ['1', '2', '3'] {
            let selection = state
                .handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))
                .await
                .expect("type dashboard prompt");
            assert!(selection.is_none());
        }

        tokio::time::sleep(ChatComposer::recommended_paste_flush_delay()).await;
        assert!(
            state
                .dashboard_composer
                .as_mut()
                .expect("dashboard composer")
                .composer
                .flush_paste_burst_if_due()
        );
        assert_eq!(
            state
                .dashboard_composer
                .as_ref()
                .expect("dashboard composer")
                .composer
                .current_text_with_pending(),
            "123"
        );
        assert_snapshot!(
            "agents_dashboard_composer_after_paste_burst_flush",
            render_dashboard_snapshot(&state, /*width*/ 80, /*height*/ 20)
        );

        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("submit dashboard prompt")
            .expect("session selection");
        let SessionSelection::StartFreshIn { cwd, user_message } = selection else {
            panic!("expected selected-project fresh session");
        };
        assert_eq!(
            (cwd, user_message),
            (
                PathBuf::from("/work/selected"),
                crate::chatwidget::UserMessage {
                    text: String::from("123"),
                    local_images: Vec::new(),
                    remote_image_urls: Vec::new(),
                    text_elements: Vec::new(),
                    mention_bindings: Vec::new(),
                },
            )
        );
    }

    #[tokio::test]
    async fn dashboard_empty_enter_resumes_selected_session() {
        let row = make_dashboard_row(
            "/work/selected",
            "2026-04-28T16:20:00Z",
            "selected session",
            DashboardStatus::Idle,
        );
        let thread_id = row.thread_id.expect("thread id");
        let mut state = dashboard_state(vec![row]);

        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("open dashboard session")
            .expect("session selection");

        assert!(matches!(
            selection,
            SessionSelection::ResumeInSessionCwd(SessionTarget {
                thread_id: selected_thread_id,
                ..
            }) if selected_thread_id == thread_id
        ));
    }

    #[tokio::test]
    async fn dashboard_empty_right_resumes_selected_session() {
        let row = make_dashboard_row(
            "/work/selected",
            "2026-04-28T16:20:00Z",
            "selected session",
            DashboardStatus::Idle,
        );
        let thread_id = row.thread_id.expect("thread id");
        let mut state = dashboard_state(vec![row]);

        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
            .await
            .expect("open dashboard session")
            .expect("session selection");

        assert!(matches!(
            selection,
            SessionSelection::ResumeInSessionCwd(SessionTarget {
                thread_id: selected_thread_id,
                ..
            }) if selected_thread_id == thread_id
        ));
    }

    #[tokio::test]
    async fn dashboard_composer_popup_has_priority_over_row_navigation() {
        let mut state = dashboard_state(vec![
            make_dashboard_row(
                "/work/first",
                "2026-04-28T16:20:00Z",
                "first",
                DashboardStatus::Working,
            ),
            make_dashboard_row(
                "/work/second",
                "2026-04-28T16:10:00Z",
                "second",
                DashboardStatus::Idle,
            ),
        ]);
        let composer = &mut state
            .dashboard_composer
            .as_mut()
            .expect("dashboard composer")
            .composer;
        composer.insert_str("/");
        composer.sync_popups();
        assert!(composer.popup_active());

        state
            .handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .await
            .expect("move slash command selection");

        assert_eq!(state.selected, 0);
    }

    #[tokio::test]
    async fn dashboard_ctrl_navigation_changes_selection_with_a_draft() {
        let mut state = dashboard_state(vec![
            make_dashboard_row(
                "/work/first",
                "2026-04-28T16:20:00Z",
                "first",
                DashboardStatus::Working,
            ),
            make_dashboard_row(
                "/work/second",
                "2026-04-28T16:10:00Z",
                "second",
                DashboardStatus::Idle,
            ),
        ]);
        state
            .dashboard_composer
            .as_mut()
            .expect("dashboard composer")
            .composer
            .insert_str("draft");

        state
            .handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL))
            .await
            .expect("move selection");

        assert_eq!(state.selected, 1);
        assert_eq!(
            state
                .dashboard_composer
                .as_ref()
                .expect("dashboard composer")
                .composer
                .current_text_with_pending(),
            "draft"
        );
    }

    #[tokio::test]
    async fn dashboard_search_filters_sessions_without_replacing_composer_draft() {
        let mut state = dashboard_state(vec![
            make_dashboard_row(
                "/work/first",
                "2026-04-28T16:20:00Z",
                "alpha task",
                DashboardStatus::Working,
            ),
            make_dashboard_row(
                "/work/second",
                "2026-04-28T16:10:00Z",
                "beta task",
                DashboardStatus::Idle,
            ),
        ]);
        state
            .dashboard_composer
            .as_mut()
            .expect("dashboard composer")
            .composer
            .insert_str("draft prompt");

        state
            .handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL))
            .await
            .expect("activate search");
        for character in "beta".chars() {
            state
                .handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))
                .await
                .expect("type search");
        }

        assert_eq!(
            state
                .filtered_rows
                .iter()
                .map(Row::display_preview)
                .collect::<Vec<_>>(),
            vec!["beta task"]
        );
        assert_eq!(
            state
                .dashboard_composer
                .as_ref()
                .expect("dashboard composer")
                .composer
                .current_text_with_pending(),
            "draft prompt"
        );
    }

    #[test]
    fn dashboard_keeps_invocation_directory_as_empty_project_fallback() {
        let state = dashboard_state(Vec::new());

        assert_eq!(
            state
                .filtered_rows
                .iter()
                .map(|row| (row.cwd.as_deref(), row.thread_id, row.display_preview(),))
                .collect::<Vec<_>>(),
            vec![(
                Some(Path::new("/work/invocation")),
                None,
                "Start a new agent",
            )]
        );
    }

    #[test]
    fn dashboard_lazily_requests_and_caches_visible_subtitles() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let request_sink = Arc::clone(&requests);
        let loader: PickerLoader = Arc::new(move |request| {
            request_sink.lock().expect("request log").push(request);
        });
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.launch_context = SessionPickerLaunchContext::AgentsDashboard;
        state.all_rows = vec![
            make_dashboard_row(
                "/work/first",
                "2026-04-28T16:20:00Z",
                "first",
                DashboardStatus::Working,
            ),
            make_dashboard_row(
                "/work/second",
                "2026-04-28T16:10:00Z",
                "second",
                DashboardStatus::Idle,
            ),
        ];
        state.apply_filter();

        state.update_viewport(/*rows*/ 1, /*width*/ 80);
        state.update_viewport(/*rows*/ 1, /*width*/ 80);

        let requested = requests
            .lock()
            .expect("request log")
            .iter()
            .filter_map(|request| match request {
                PickerLoadRequest::Preview { thread_id } => Some(*thread_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            requested,
            vec![state.filtered_rows[0].thread_id.expect("thread")]
        );

        let thread_id = requested[0];
        state.transcript_previews.insert(
            thread_id,
            TranscriptPreviewState::Loaded(vec![TranscriptPreviewLine {
                speaker: TranscriptPreviewSpeaker::Assistant,
                text: String::from("Latest assistant update"),
            }]),
        );
        assert_eq!(
            dashboard_subtitle(state.transcript_previews.get(&thread_id)),
            Some("Latest assistant update".dim())
        );
    }

    #[test]
    fn dashboard_reloads_composer_inventory_when_selected_project_changes() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let request_sink = Arc::clone(&requests);
        let loader: PickerLoader = Arc::new(move |request| {
            request_sink.lock().expect("request log").push(request);
        });
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.launch_context = SessionPickerLaunchContext::AgentsDashboard;
        state.all_rows = vec![
            make_dashboard_row(
                "/work/first",
                "2026-04-28T16:20:00Z",
                "first",
                DashboardStatus::Working,
            ),
            make_dashboard_row(
                "/work/second",
                "2026-04-28T16:10:00Z",
                "second",
                DashboardStatus::Idle,
            ),
        ];
        state.apply_filter();

        state.load_dashboard_composer_inventory();
        state.move_dashboard_selection(/*down*/ true);
        state.move_dashboard_selection(/*down*/ false);

        let requested = requests
            .lock()
            .expect("request log")
            .iter()
            .filter_map(|request| match request {
                PickerLoadRequest::DashboardComposerInventory { cwd } => Some(cwd.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            requested,
            vec![
                PathBuf::from("/work/first"),
                PathBuf::from("/work/second"),
                PathBuf::from("/work/first"),
            ]
        );
    }

    #[test]
    fn dashboard_applies_live_status_name_and_archive_notifications() {
        let row = make_dashboard_row(
            "/work/project",
            "2026-04-28T16:00:00Z",
            "original preview",
            DashboardStatus::Idle,
        );
        let thread_id = row.thread_id.expect("thread id");
        let mut state = dashboard_state(vec![row]);

        state.handle_app_server_event(AppServerEvent::ServerNotification(Box::new(
            ServerNotification::ThreadStatusChanged(
                codex_app_server_protocol::ThreadStatusChangedNotification {
                    thread_id: thread_id.to_string(),
                    status: codex_app_server_protocol::ThreadStatus::Active {
                        active_flags: vec![
                            codex_app_server_protocol::ThreadActiveFlag::WaitingOnApproval,
                        ],
                    },
                },
            ),
        )));
        state.handle_app_server_event(AppServerEvent::ServerNotification(Box::new(
            ServerNotification::ThreadNameUpdated(
                codex_app_server_protocol::ThreadNameUpdatedNotification {
                    thread_id: thread_id.to_string(),
                    thread_name: Some(String::from("renamed session")),
                },
            ),
        )));

        assert_eq!(
            (
                state.filtered_rows[0].dashboard_status,
                state.filtered_rows[0].thread_name.as_deref(),
            ),
            (Some(DashboardStatus::NeedsInput), Some("renamed session"))
        );

        state.handle_app_server_event(AppServerEvent::ServerNotification(Box::new(
            ServerNotification::ThreadArchived(
                codex_app_server_protocol::ThreadArchivedNotification {
                    thread_id: thread_id.to_string(),
                },
            ),
        )));

        assert_eq!(
            state
                .filtered_rows
                .iter()
                .map(Row::display_preview)
                .collect::<Vec<_>>(),
            vec!["Start a new agent"]
        );
    }

    #[test]
    fn dashboard_system_error_uses_error_specific_subtitle() {
        let row = make_dashboard_row(
            "/work/selected",
            "2026-04-28T16:20:00Z",
            "selected session",
            DashboardStatus::Working,
        );
        let thread_id = row.thread_id.expect("thread id");
        let mut state = dashboard_state(vec![row]);

        state.handle_app_server_event(AppServerEvent::ServerNotification(Box::new(
            ServerNotification::ThreadStatusChanged(
                codex_app_server_protocol::ThreadStatusChangedNotification {
                    thread_id: thread_id.to_string(),
                    status: codex_app_server_protocol::ThreadStatus::SystemError,
                },
            ),
        )));

        assert_eq!(
            state.filtered_rows[0].dashboard_status,
            Some(DashboardStatus::NeedsInput)
        );
        assert_eq!(
            render_comfortable_session_lines(
                &state.filtered_rows[0],
                &state,
                /*is_selected*/ false,
                /*is_expanded*/ false,
                /*is_zebra*/ false,
                /*width*/ 80,
            )[1]
            .to_string(),
            "  Thread stopped because of a system error"
        );
    }

    #[test]
    fn dashboard_initial_page_system_error_uses_error_specific_subtitle() {
        let row = make_dashboard_row(
            "/work/selected",
            "2026-04-28T16:20:00Z",
            "selected session",
            DashboardStatus::NeedsInput,
        );
        let thread_id = row.thread_id.expect("thread id");
        let mut state = dashboard_state(Vec::new());

        state.ingest_page(PickerPage {
            rows: vec![row],
            dashboard_system_errors: HashSet::from([thread_id]),
            next_cursor: None,
            num_scanned_files: 1,
            reached_scan_cap: false,
        });

        assert_eq!(
            render_comfortable_session_lines(
                &state.filtered_rows[0],
                &state,
                /*is_selected*/ false,
                /*is_expanded*/ false,
                /*is_zebra*/ false,
                /*width*/ 80,
            )[1]
            .to_string(),
            "  Thread stopped because of a system error"
        );
    }

    #[test]
    fn dashboard_status_change_invalidates_cached_subtitle() {
        let row = make_dashboard_row(
            "/work/project",
            "2026-04-28T16:00:00Z",
            "original preview",
            DashboardStatus::Working,
        );
        let thread_id = row.thread_id.expect("thread id");
        let mut state = dashboard_state(vec![row]);
        state.transcript_previews.insert(
            thread_id,
            TranscriptPreviewState::Loaded(vec![TranscriptPreviewLine {
                speaker: TranscriptPreviewSpeaker::Assistant,
                text: String::from("stale assistant update"),
            }]),
        );

        state.handle_app_server_event(AppServerEvent::ServerNotification(Box::new(
            ServerNotification::ThreadStatusChanged(
                codex_app_server_protocol::ThreadStatusChangedNotification {
                    thread_id: thread_id.to_string(),
                    status: codex_app_server_protocol::ThreadStatus::Idle,
                },
            ),
        )));

        assert!(!state.transcript_previews.contains_key(&thread_id));
    }

    #[tokio::test]
    async fn dashboard_disconnect_preserves_view_state_for_reconnect() {
        let row = make_dashboard_row(
            "/work/selected",
            "2026-04-28T16:20:00Z",
            "selected session",
            DashboardStatus::Working,
        );
        let selected_thread_id = row.thread_id;
        let mut state = dashboard_state(vec![row]);
        state.dashboard_group_mode = DashboardGroupMode::Status;
        state
            .dashboard_composer
            .as_mut()
            .expect("dashboard composer")
            .composer
            .insert_str("preserved draft");

        let selection = state
            .handle_background_event(BackgroundEvent::AppServer(AppServerEvent::Disconnected {
                message: String::from("daemon restarted"),
            }))
            .await
            .expect("disconnect handling");

        let Some(SessionSelection::ReconnectDashboard(resume_state)) = selection else {
            panic!("expected dashboard reconnect state");
        };
        assert_eq!(
            resume_state,
            DashboardResumeState {
                draft: String::from("preserved draft"),
                group_mode: DashboardGroupMode::Status,
                selected_thread_id,
                selected_cwd: Some(PathBuf::from("/work/selected")),
            }
        );
        assert_eq!(
            state.inline_error.as_deref(),
            Some("Dashboard disconnected: daemon restarted")
        );
    }

    #[test]
    fn agents_dashboard_project_grouping_snapshot() {
        let mut state = dashboard_state(vec![
            make_dashboard_row(
                "/Users/majd/Projects/claudex",
                "2026-04-28T16:29:18Z",
                "Build the agents dashboard",
                DashboardStatus::Working,
            ),
            make_dashboard_row(
                "/Users/majd/Projects/claudex",
                "2026-04-28T15:54:00Z",
                "Review daemon lifecycle",
                DashboardStatus::NeedsInput,
            ),
            make_dashboard_row(
                "/Users/majd/Projects/site",
                "2026-04-28T14:30:00Z",
                "Update landing page",
                DashboardStatus::Idle,
            ),
            make_dashboard_row(
                "/Users/majd/Projects/archive",
                "2026-04-27T11:00:00Z",
                "Old completed task",
                DashboardStatus::Done,
            ),
        ]);
        state.update_viewport(/*rows*/ 18, /*width*/ 92);

        assert_snapshot!(
            "agents_dashboard_project_grouping",
            render_dashboard_list_snapshot(&state, /*width*/ 92, /*height*/ 18)
        );
    }

    #[test]
    fn agents_dashboard_scrolls_by_rendered_lines_snapshot() {
        let mut state = dashboard_state(vec![
            make_dashboard_row(
                "/Users/majd/Projects/claudex",
                "2026-04-28T16:29:18Z",
                "Build the agents dashboard",
                DashboardStatus::Working,
            ),
            make_dashboard_row(
                "/Users/majd/Projects/claudex",
                "2026-04-28T16:20:00Z",
                "Polish dashboard scrolling",
                DashboardStatus::Working,
            ),
            make_dashboard_row(
                "/Users/majd/Projects/claudex",
                "2026-04-28T16:10:00Z",
                "Add dashboard snapshots",
                DashboardStatus::Idle,
            ),
        ]);
        state.update_viewport(/*rows*/ 7, /*width*/ 92);
        state.move_dashboard_selection(/*down*/ true);

        assert_eq!(state.dashboard_scroll_offset, 2);
        assert_eq!(state.scroll_top, 0);
        assert_snapshot!(
            "agents_dashboard_scrolls_by_rendered_lines",
            render_dashboard_list_snapshot(&state, /*width*/ 92, /*height*/ 7)
        );
    }

    #[test]
    fn agents_dashboard_composer_snapshot() {
        let mut state = dashboard_state(vec![make_dashboard_row(
            "/Users/majd/Projects/claudex",
            "2026-04-28T16:29:18Z",
            "Build the agents dashboard",
            DashboardStatus::Working,
        )]);
        state
            .dashboard_composer
            .as_mut()
            .expect("dashboard composer")
            .composer
            .insert_str("Add project-aware session creation");
        state.update_viewport(/*rows*/ 12, /*width*/ 80);

        assert_snapshot!(
            "agents_dashboard_composer",
            render_dashboard_snapshot(&state, /*width*/ 80, /*height*/ 20)
        );
    }

    #[test]
    fn agents_dashboard_status_grouping_narrow_snapshot() {
        let mut state = dashboard_state(vec![
            make_dashboard_row(
                "/a/very/long/project/path/that/will/not/fit",
                "2026-04-28T16:29:18Z",
                "Approve deployment to production",
                DashboardStatus::NeedsInput,
            ),
            make_dashboard_row(
                "/Users/majd/Projects/claudex",
                "2026-04-28T16:20:00Z",
                "Implement live status notifications",
                DashboardStatus::Working,
            ),
            make_dashboard_row(
                "/Users/majd/Projects/site",
                "2026-04-28T14:30:00Z",
                "Waiting for next prompt",
                DashboardStatus::Idle,
            ),
            make_dashboard_row(
                "/Users/majd/Projects/archive",
                "2026-04-27T11:00:00Z",
                "Finished cleanup",
                DashboardStatus::Done,
            ),
        ]);
        state.dashboard_group_mode = DashboardGroupMode::Status;
        state.sort_dashboard_rows();
        state.update_viewport(/*rows*/ 15, /*width*/ 48);

        assert_snapshot!(
            "agents_dashboard_status_grouping_narrow",
            render_dashboard_list_snapshot(&state, /*width*/ 48, /*height*/ 15)
        );
    }

    #[test]
    fn agents_dashboard_dense_columns_snapshot() {
        let mut state = dashboard_state(vec![
            make_dashboard_row(
                "/work/one",
                "2026-04-28T16:29:18Z",
                "Short title",
                DashboardStatus::NeedsInput,
            ),
            make_dashboard_row(
                "/work/two",
                "2026-04-28T16:20:00Z",
                "A much longer title that still begins in the same column",
                DashboardStatus::Working,
            ),
            make_dashboard_row(
                "/work/three",
                "2026-04-28T16:10:00Z",
                "Idle task",
                DashboardStatus::Idle,
            ),
            make_dashboard_row(
                "/work/four",
                "2026-04-28T16:00:00Z",
                "Completed task",
                DashboardStatus::Done,
            ),
        ]);
        state.density = SessionListDensity::Dense;
        state.update_viewport(/*rows*/ 12, /*width*/ 88);

        assert_snapshot!(
            "agents_dashboard_dense_columns",
            render_dashboard_list_snapshot(&state, /*width*/ 88, /*height*/ 12)
        );
    }

    #[test]
    fn agents_dashboard_rows_use_status_colors_and_selection_contrast() {
        let statuses = [
            DashboardStatus::NeedsInput,
            DashboardStatus::Working,
            DashboardStatus::Idle,
            DashboardStatus::Done,
        ];
        let mut state = dashboard_state(
            statuses
                .into_iter()
                .enumerate()
                .map(|(index, status)| {
                    make_dashboard_row(
                        &format!("/work/{index}"),
                        "2026-04-28T16:20:00Z",
                        &format!("task {index}"),
                        status,
                    )
                })
                .collect(),
        );
        state.sort_dashboard_rows();

        let rendered = state
            .filtered_rows
            .iter()
            .filter(|row| row.dashboard_status.is_some())
            .enumerate()
            .map(|(index, row)| {
                render_comfortable_session_lines(
                    row,
                    &state,
                    index == 0,
                    /*is_expanded*/ false,
                    /*is_zebra*/ false,
                    /*width*/ 80,
                )[0]
                .clone()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            rendered
                .iter()
                .map(|line| (
                    line.spans[..2].iter().map(Span::width).sum::<usize>(),
                    line.spans[1].style.fg,
                    line.spans[2].style.fg,
                    line.spans[2]
                        .style
                        .add_modifier
                        .contains(ratatui::style::Modifier::DIM),
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    16,
                    Some(Color::Red),
                    dashboard_row_text_style(true).fg,
                    false
                ),
                (16, Some(Color::Cyan), None, true),
                (16, Some(Color::Green), None, true),
                (16, None, None, true),
            ]
        );
    }

    #[test]
    fn agents_dashboard_empty_snapshot() {
        let mut state = dashboard_state(Vec::new());
        state.update_viewport(/*rows*/ 4, /*width*/ 48);

        assert_snapshot!(
            "agents_dashboard_empty",
            render_dashboard_list_snapshot(&state, /*width*/ 48, /*height*/ 4)
        );
    }

    #[test]
    fn agents_dashboard_system_error_snapshot() {
        let row = make_dashboard_row(
            "/Users/majd/Projects/claudex",
            "2026-04-28T16:29:18Z",
            "Recover disconnected daemon",
            DashboardStatus::NeedsInput,
        );
        let thread_id = row.thread_id.expect("thread id");
        let mut state = dashboard_state(vec![row]);
        state.dashboard_system_errors.insert(thread_id);
        state.update_viewport(/*rows*/ 6, /*width*/ 72);

        assert_snapshot!(
            "agents_dashboard_system_error",
            render_dashboard_list_snapshot(&state, /*width*/ 72, /*height*/ 6)
        );
    }

    fn local_db_first_state() -> (PickerState, Arc<Mutex<Vec<PageLoadRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let request_sink = Arc::clone(&requests);
        let loader = page_only_loader(move |request| {
            request_sink.lock().unwrap().push(request);
        });
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.initial_page_mode = PageLoadMode::StateDbOnly;
        (state, requests)
    }

    fn footer_lines_text(state: &PickerState, width: u16) -> String {
        footer_hint_lines(state, width)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn footer_snapshot(state: &PickerState, width: u16, list_height: u16) -> String {
        use crate::custom_terminal::Terminal;
        use crate::test_backend::VT100Backend;

        let backend = VT100Backend::new(width, /*height*/ 4);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, width, 4));

        {
            let mut frame = terminal.get_frame();
            let area = frame.area();
            render_picker_footer(&mut frame, area, state, list_height);
        }
        terminal.flush().expect("flush");

        terminal
            .backend()
            .to_string()
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn row_display_preview_prefers_thread_name() {
        let row = Row {
            path: Some(PathBuf::from("/tmp/a.jsonl")),
            preview: String::from("first message"),
            thread_id: None,
            thread_name: Some(String::from("My session")),
            created_at: None,
            updated_at: None,
            cwd: None,
            git_branch: None,
            dashboard_status: None,
        };

        assert_eq!(row.display_preview(), "My session");
    }

    #[test]
    fn local_picker_thread_list_params_include_cwd_filter() {
        let cwd_filter = picker_cwd_filter(
            Path::new("/tmp/project"),
            /*show_all*/ false,
            /*uses_remote_workspace*/ false,
            /*remote_cwd_override*/ None,
        );
        let params = thread_list_params(
            Some(String::from("cursor-1")),
            cwd_filter.as_deref(),
            SessionStatus::Active,
            ProviderFilter::MatchDefault(String::from("openai")),
            ThreadSortKey::UpdatedAt,
            /*include_non_interactive*/ false,
            /*use_state_db_only*/ true,
        );

        assert_eq!(
            params.cwd,
            Some(ThreadListCwdFilter::One(String::from("/tmp/project")))
        );
        assert!(params.use_state_db_only);
    }

    #[tokio::test]
    async fn local_db_first_treats_empty_later_page_as_end_of_db_listing() {
        let (mut state, requests) = local_db_first_state();
        state.start_initial_load();
        let db_request = requests.lock().unwrap()[0].clone();
        deliver_page(
            &mut state,
            &db_request,
            ok_page(
                vec![make_row(
                    "/tmp/indexed.jsonl",
                    "2025-01-03T00:00:00Z",
                    "indexed metadata",
                )],
                Some("db-cursor"),
            ),
        )
        .await;

        state.maybe_load_more_for_scroll();
        let later_db_request = requests.lock().unwrap()[1].clone();
        assert_eq!(later_db_request.mode, PageLoadMode::StateDbOnly);
        deliver_page(
            &mut state,
            &later_db_request,
            ok_page(Vec::new(), /*next_cursor*/ None),
        )
        .await;

        assert_eq!(requests.lock().unwrap().len(), 2);
        assert_eq!(state.all_rows[0].preview, "indexed metadata");
        assert!(state.pagination.next_cursor.is_none());
    }

    #[tokio::test]
    async fn local_db_first_falls_back_when_initial_db_page_is_empty() {
        let (mut state, requests) = local_db_first_state();
        state.start_initial_load();
        let db_request = requests.lock().unwrap()[0].clone();
        deliver_page(
            &mut state,
            &db_request,
            ok_page(Vec::new(), /*next_cursor*/ None),
        )
        .await;

        let fallback_request = requests.lock().unwrap()[1].clone();
        assert_eq!(fallback_request.mode, PageLoadMode::StoreDefault);
        assert!(fallback_request.cursor.is_none());
        state.relative_time_reference =
            Some(parse_timestamp_str("2025-01-04T00:00:00Z").expect("timestamp"));
        state.update_viewport(/*rows*/ 12, /*width*/ 80);
        let render_state = |state: &PickerState| {
            use crate::custom_terminal::Terminal;
            use crate::test_backend::VT100Backend;

            let backend = VT100Backend::new(/*width*/ 80, /*height*/ 12);
            let mut terminal = Terminal::with_options(backend).expect("terminal");
            terminal.set_viewport_area(Rect::new(
                /*x*/ 0, /*y*/ 0, /*width*/ 80, /*height*/ 12,
            ));
            {
                let mut frame = terminal.get_frame();
                let area = frame.area();
                render_list(&mut frame, area, state);
            }
            terminal.flush().expect("flush");
            terminal.backend().to_string()
        };
        let loading_snapshot = render_state(&state);

        deliver_page(
            &mut state,
            &fallback_request,
            ok_page(
                vec![
                    make_row(
                        "/tmp/first.jsonl",
                        "2025-01-03T00:00:00Z",
                        "first store result",
                    ),
                    make_row(
                        "/tmp/repaired.jsonl",
                        "2025-01-03T00:00:00Z",
                        "repaired store result",
                    ),
                    make_row(
                        "/tmp/third.jsonl",
                        "2025-01-03T00:00:00Z",
                        "third store result",
                    ),
                ],
                /*next_cursor*/ None,
            ),
        )
        .await;

        assert_eq!(
            state
                .all_rows
                .iter()
                .map(|row| row.preview.as_str())
                .collect::<Vec<_>>(),
            vec![
                "first store result",
                "repaired store result",
                "third store result"
            ]
        );
        assert_eq!(state.pagination.num_scanned_files, 3);
        let repaired_snapshot = render_state(&state);
        assert_snapshot!(
            "resume_picker_db_fallback_transition",
            format!(
                "---- loading fallback ----\n{loading_snapshot}\n---- repaired results ----\n{repaired_snapshot}"
            )
        );
    }

    #[tokio::test]
    async fn local_db_first_accepts_db_thread_id_without_rollout_validation() {
        let (mut state, requests) = local_db_first_state();
        state.start_initial_load();
        let db_request = requests.lock().unwrap()[0].clone();
        let thread_id = ThreadId::new();
        let mut row = make_row(
            "/tmp/missing-rollout.jsonl",
            "2025-01-01T00:00:00Z",
            "indexed metadata",
        );
        row.thread_id = Some(thread_id);
        deliver_page(
            &mut state,
            &db_request,
            ok_page(vec![row], /*next_cursor*/ None),
        )
        .await;

        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("enter should not abort the picker");
        assert!(matches!(
            selection,
            Some(SessionSelection::Resume(SessionTarget {
                thread_id: selected_thread_id,
                ..
            })) if selected_thread_id == thread_id
        ));
    }

    #[test]
    fn row_search_matches_metadata_fields() {
        let thread_id =
            ThreadId::from_string("019dabc1-0ef5-7431-b81c-03037f51f62c").expect("thread id");
        let row = Row {
            path: Some(PathBuf::from("/tmp/a.jsonl")),
            preview: String::from("first message"),
            thread_id: Some(thread_id),
            thread_name: Some(String::from("My session")),
            created_at: None,
            updated_at: None,
            cwd: Some(PathBuf::from("/tmp/codex-session-picker")),
            git_branch: Some(String::from("fcoury/session-picker")),
            dashboard_status: None,
        };

        assert!(row.matches_query("session-picker"));
        assert!(row.matches_query("fcoury"));
        assert!(row.matches_query(&thread_id.to_string()[..8]));
    }

    #[test]
    fn relative_time_formats_zero_seconds_as_now() {
        let reference = DateTime::parse_from_rfc3339("2026-05-02T12:00:00Z")
            .expect("valid timestamp")
            .with_timezone(&Utc);

        assert_eq!(format_relative_time(reference, Some(reference)), "now");
        assert_eq!(
            format_relative_time(reference, Some(reference - Duration::seconds(1))),
            "1s ago"
        );
    }

    #[test]
    fn long_relative_time_uses_words() {
        let reference = DateTime::parse_from_rfc3339("2026-05-02T12:00:00Z")
            .expect("valid timestamp")
            .with_timezone(&Utc);

        assert_eq!(format_relative_time_long(reference, reference), "now");
        assert_eq!(
            format_relative_time_long(reference, reference - Duration::minutes(20)),
            "20 minutes ago"
        );
        assert_eq!(
            format_relative_time_long(reference, reference - Duration::hours(1)),
            "1 hour ago"
        );
    }

    #[test]
    fn expanded_session_details_include_metadata() {
        let thread_id =
            ThreadId::from_string("019dabc1-0ef5-7431-b81c-03037f51f62c").expect("thread id");
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.relative_time_reference = parse_timestamp_str("2026-05-02T14:48:19Z");
        let row = Row {
            path: Some(PathBuf::from("/tmp/a.jsonl")),
            preview: String::from("first message"),
            thread_id: Some(thread_id),
            thread_name: Some(String::from("feat(tui): add raw scrollback mode")),
            created_at: parse_timestamp_str("2026-05-02T14:31:08Z"),
            updated_at: parse_timestamp_str("2026-05-02T14:48:19Z"),
            cwd: Some(PathBuf::from("/Users/felipe.coury/code/codex")),
            git_branch: Some(String::from("codex/raw-scrollback-mode")),
            dashboard_status: None,
        };

        let rendered = render_expanded_session_details(&row, &state, /*width*/ 120)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let expected_directory =
            format_directory_display(row.cwd.as_deref().expect("cwd"), /*max_width*/ None);

        assert!(rendered.contains(
            "Session:    feat(tui): add raw scrollback mode (019dabc1-0ef5-7431-b81c-03037f51f62c)"
        ));
        assert!(rendered.contains("Created:    17 minutes ago · 2026-05-02 14:31:08"));
        assert!(rendered.contains("Updated:    now · 2026-05-02 14:48:19"));
        assert!(rendered.contains(&format!("Directory:  {expected_directory}")));
        assert!(rendered.contains("Branch:      codex/raw-scrollback-mode"));
        assert!(rendered.contains("Conversation:"));
    }

    #[test]
    fn footer_prioritizes_active_sort_timestamp() {
        let updated = render_footer_lines(
            ThreadSortKey::UpdatedAt,
            "5h ago",
            "3h ago",
            Some("main"),
            Some("tmp/codex"),
            /*show_cwd*/ true,
            /*width*/ 80,
        );
        let created = render_footer_lines(
            ThreadSortKey::CreatedAt,
            "5h ago",
            "3h ago",
            Some("main"),
            Some("tmp/codex"),
            /*show_cwd*/ true,
            /*width*/ 80,
        );

        assert_eq!(updated.len(), 1);
        assert_eq!(created.len(), 1);
        assert!(updated[0].to_string().starts_with("  3h ago"));
        assert!(created[0].to_string().starts_with("  5h ago"));
        assert!(!updated[0].to_string().contains("created 5h ago"));
        assert!(!created[0].to_string().contains("updated 3h ago"));
        assert_metadata_order(&updated[0], "⌁ tmp/codex", " main");
        assert_metadata_order(&created[0], "⌁ tmp/codex", " main");
    }

    #[test]
    fn footer_marks_missing_branch() {
        let footer = render_footer_lines(
            ThreadSortKey::UpdatedAt,
            "5h ago",
            "3h ago",
            /*branch*/ None,
            Some("/tmp/codex"),
            /*show_cwd*/ true,
            /*width*/ 80,
        );

        assert_eq!(footer.len(), 1);
        let rendered = footer[0].to_string();
        assert!(rendered.contains("⌁ /tmp/codex"));
        assert!(rendered.contains(" no branch"));
        assert_metadata_order(&footer[0], "⌁ /tmp/codex", " no branch");
    }

    #[test]
    fn footer_branch_expands_when_line_has_room() {
        let branch = "etraut/animations-false-improvements";
        let footer = render_footer_lines(
            ThreadSortKey::UpdatedAt,
            "5h ago",
            "4h ago",
            Some(branch),
            Some("~/code/codex.etraut-animations-false-improvements/codex-rs"),
            /*show_cwd*/ true,
            /*width*/ 140,
        );

        assert_eq!(footer.len(), 1);
        assert!(footer[0].to_string().contains(branch));
    }

    #[test]
    fn footer_cwd_truncates_to_responsive_column() {
        let cwd = "~/code/codex.owner-extremely-long-worktree-name-that-needs-truncating/codex-rs";
        let branch = "owner/branch";
        let footer = render_footer_lines(
            ThreadSortKey::UpdatedAt,
            "5h ago",
            "4h ago",
            Some(branch),
            Some(cwd),
            /*show_cwd*/ true,
            /*width*/ 80,
        );

        assert_eq!(footer.len(), 1);
        let footer = footer[0].to_string();
        assert!(!footer.contains(cwd));
        assert!(footer.contains("⌁ ~/code/codex."));
        assert!(footer.contains("..."));
        assert!(footer.contains(" owner/branch"));
    }

    #[test]
    fn footer_omits_cwd_when_hidden() {
        let footer = render_footer_lines(
            ThreadSortKey::UpdatedAt,
            "5h ago",
            "4h ago",
            Some("owner/branch"),
            Some("~/code/codex.owner-worktree/codex-rs"),
            /*show_cwd*/ false,
            /*width*/ 80,
        );

        assert_eq!(footer.len(), 1);
        let footer = footer[0].to_string();
        assert!(footer.contains("4h ago"));
        assert!(footer.contains(" owner/branch"));
        assert!(!footer.contains("⌁"));
        assert!(!footer.contains("~/code"));
    }

    fn assert_metadata_order(line: &Line<'_>, first: &str, second: &str) {
        let rendered = line.to_string();
        let first_index = rendered.find(first).expect("first metadata item");
        let second_index = rendered.find(second).expect("second metadata item");
        assert!(first_index < second_index);
    }

    #[test]
    fn remote_thread_list_params_omit_provider_filter() {
        let params = thread_list_params(
            Some(String::from("cursor-1")),
            Some(Path::new("repo/on/server")),
            SessionStatus::Active,
            ProviderFilter::Any,
            ThreadSortKey::UpdatedAt,
            /*include_non_interactive*/ false,
            /*use_state_db_only*/ false,
        );

        assert_eq!(params.cursor, Some(String::from("cursor-1")));
        assert_eq!(params.model_providers, None);
        assert_eq!(
            params.source_kinds,
            Some(vec![ThreadSourceKind::Cli, ThreadSourceKind::VsCode])
        );
        assert_eq!(
            params.cwd,
            Some(ThreadListCwdFilter::One(String::from("repo/on/server")))
        );
    }

    #[test]
    fn remote_thread_list_params_can_include_non_interactive_sources() {
        let params = thread_list_params(
            Some(String::from("cursor-1")),
            /*cwd_filter*/ None,
            SessionStatus::Active,
            ProviderFilter::Any,
            ThreadSortKey::UpdatedAt,
            /*include_non_interactive*/ true,
            /*use_state_db_only*/ false,
        );

        assert_eq!(params.cursor, Some(String::from("cursor-1")));
        assert_eq!(params.model_providers, None);
        let source_kinds = crate::resume_source_kinds(/*include_non_interactive*/ true);
        assert_eq!(params.source_kinds, Some(source_kinds));
    }

    #[test]
    fn remote_picker_sends_cwd_filter_without_local_post_filtering() {
        let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let request_sink = recorded_requests.clone();
        let loader = page_only_loader(move |req: PageLoadRequest| {
            request_sink.lock().unwrap().push(req);
        });
        let remote_cwd = Some(PathBuf::from("/srv/link-project"));
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::Any,
            /*show_all*/ false,
            remote_cwd.clone(),
            SessionPickerAction::Resume,
        );
        state.local_filter_cwd =
            local_picker_cwd_filter(&remote_cwd, /*uses_remote_workspace*/ true);

        state.start_initial_load();

        {
            let guard = recorded_requests.lock().unwrap();
            assert_eq!(guard.len(), 1);
            assert_eq!(guard[0].cwd_filter, remote_cwd);
            assert_eq!(guard[0].mode, PageLoadMode::StoreDefault);
        }

        let row = Row {
            path: None,
            preview: String::from("remote session"),
            thread_id: Some(ThreadId::new()),
            thread_name: None,
            created_at: None,
            updated_at: None,
            cwd: Some(PathBuf::from("/srv/real-project")),
            git_branch: None,
            dashboard_status: None,
        };

        assert!(state.row_matches_filter(&row));
    }

    #[test]
    fn remote_picker_does_not_filter_rows_by_local_cwd() {
        let loader = page_only_loader(|_| {});
        let state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::Any,
            /*show_all*/ false,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        let row = Row {
            path: None,
            preview: String::from("remote session"),
            thread_id: Some(ThreadId::new()),
            thread_name: None,
            created_at: None,
            updated_at: None,
            cwd: Some(PathBuf::from("/srv/remote-project")),
            git_branch: None,
            dashboard_status: None,
        };

        assert!(state.row_matches_filter(&row));
    }

    #[test]
    fn resume_table_snapshot() {
        use crate::custom_terminal::Terminal;
        use crate::test_backend::VT100Backend;

        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );

        let now = parse_timestamp_str("2026-04-28T16:30:00Z").expect("timestamp");
        let rows = vec![
            Row {
                path: Some(PathBuf::from("/tmp/a.jsonl")),
                preview: String::from("Fix resume picker timestamps"),
                thread_id: None,
                thread_name: None,
                created_at: Some(now - Duration::minutes(16)),
                updated_at: Some(now - Duration::seconds(42)),
                cwd: None,
                git_branch: None,
                dashboard_status: None,
            },
            Row {
                path: Some(PathBuf::from("/tmp/b.jsonl")),
                preview: String::from("Investigate lazy pagination cap"),
                thread_id: None,
                thread_name: None,
                created_at: Some(now - Duration::hours(1)),
                updated_at: Some(now - Duration::minutes(35)),
                cwd: None,
                git_branch: None,
                dashboard_status: None,
            },
            Row {
                path: Some(PathBuf::from("/tmp/c.jsonl")),
                preview: String::from("Explain the codebase"),
                thread_id: None,
                thread_name: None,
                created_at: Some(now - Duration::hours(2)),
                updated_at: Some(now - Duration::hours(2)),
                cwd: None,
                git_branch: None,
                dashboard_status: None,
            },
        ];
        state.all_rows = rows.clone();
        state.filtered_rows = rows;
        state.relative_time_reference = Some(now);
        state.selected = 1;
        state.scroll_top = 0;
        state.update_viewport(/*rows*/ 12, /*width*/ 80);

        let width: u16 = 80;
        let height: u16 = 12;
        let backend = VT100Backend::new(width, height);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, width, height));

        {
            let mut frame = terminal.get_frame();
            let area = frame.area();
            render_list(&mut frame, area, &state);
        }
        terminal.flush().expect("flush");

        let snapshot = terminal.backend().to_string();
        assert_snapshot!("resume_picker_table", snapshot);
    }

    #[test]
    fn resume_search_error_snapshot() {
        use crate::custom_terminal::Terminal;
        use crate::test_backend::VT100Backend;

        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.inline_error = Some(String::from(
            "Failed to read session metadata from /tmp/missing.jsonl",
        ));

        let width: u16 = 80;
        let height: u16 = 1;
        let backend = VT100Backend::new(width, height);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, width, height));

        {
            let mut frame = terminal.get_frame();
            let line = search_line(&state, frame.area().width);
            frame.render_widget_ref(&line, frame.area());
        }
        terminal.flush().expect("flush");

        let snapshot = terminal.backend().to_string();
        assert_snapshot!("resume_picker_search_error", snapshot);
    }

    #[test]
    fn hint_line_switches_esc_label_for_search_mode() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );

        assert!(footer_lines_text(&state, /*width*/ 220).contains("esc start new"));

        state.query = String::from("picker");

        assert!(footer_lines_text(&state, /*width*/ 220).contains("esc clear search"));
    }

    #[test]
    fn hint_line_labels_cancel_keys_as_exit_for_existing_session_resume_picker() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.launch_context = SessionPickerLaunchContext::ExistingSession {
            current_thread_id: None,
        };

        let wide = footer_lines_text(&state, /*width*/ 220);
        assert!(wide.contains("esc exit"));
        assert!(wide.contains("ctrl+c exit"));

        let compact = footer_lines_text(&state, /*width*/ 119);
        assert!(compact.contains("esc exit"));
        assert!(compact.contains("ctrl+c exit"));

        state.query = String::from("picker");

        assert!(footer_lines_text(&state, /*width*/ 220).contains("esc clear search"));
    }

    #[test]
    fn hint_line_switches_density_label() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );

        assert!(footer_lines_text(&state, /*width*/ 220).contains("ctrl+o dense view"));
        assert!(footer_lines_text(&state, /*width*/ 220).contains("ctrl+t transcript"));
        assert!(footer_lines_text(&state, /*width*/ 220).contains("ctrl+e expand"));
        state.list_keymap.move_left = vec![crate::key_hint::ctrl(KeyCode::Char('h'))];
        state.list_keymap.move_right = vec![crate::key_hint::ctrl(KeyCode::Char('l'))];
        let remapped_footer = footer_lines_text(&state, /*width*/ 220);
        assert!(
            remapped_footer.contains("ctrl + h/ctrl + l change option"),
            "{remapped_footer}"
        );
        state.list_keymap.move_left.clear();
        state.list_keymap.move_right.clear();
        assert!(!footer_lines_text(&state, /*width*/ 220).contains("change option"));

        state.density = SessionListDensity::Dense;

        assert!(footer_lines_text(&state, /*width*/ 220).contains("ctrl+o comfortable view"));
    }

    #[test]
    fn hint_line_compacts_on_narrow_width() {
        let loader = page_only_loader(|_| {});
        let state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );

        let rendered = footer_lines_text(&state, /*width*/ 119);

        assert!(rendered.contains("esc new"));
        assert!(rendered.contains("tab focus"));
        assert!(rendered.contains("←/→ option"));
        assert!(rendered.contains("ctrl+o dense"));
        assert!(rendered.contains("ctrl+t preview"));
        assert!(rendered.contains("ctrl+e exp"));
        assert!(!rendered.contains("focus sort/filter"));
    }

    #[test]
    fn hint_line_snapshot_uses_distributed_wide_footer() {
        let loader = page_only_loader(|_| {});
        let state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );

        assert_snapshot!(
            "resume_picker_footer_wide",
            footer_snapshot(&state, /*width*/ 220, /*list_height*/ 20)
        );
    }

    #[test]
    fn hint_line_snapshot_uses_compact_footer() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.query = String::from("picker");
        state.density = SessionListDensity::Dense;

        assert_snapshot!(
            "resume_picker_footer_compact",
            footer_snapshot(&state, /*width*/ 96, /*list_height*/ 20)
        );
    }

    #[test]
    fn hint_line_prioritizes_keybinds_when_very_narrow() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.density = SessionListDensity::Dense;

        let width = 38;
        let lines = footer_hint_lines(&state, width);
        let rendered = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(lines.iter().all(|line| line.width() <= width as usize));
        assert!(rendered.contains("enter"));
        assert!(rendered.contains("esc"));
        assert!(rendered.contains("ctrl+c"));
        assert!(rendered.contains("ctrl+o"));
        assert!(rendered.contains("ctrl+t"));
        assert!(rendered.contains("ctrl+e"));
        assert!(rendered.contains("↑/↓"));
    }

    #[test]
    fn hint_line_shows_loading_transcript_mode() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.pending_transcript_open = Some(ThreadId::new());

        let rendered = footer_lines_text(&state, /*width*/ 80);

        assert!(rendered.contains("loading transcript"));
        assert!(rendered.contains("ctrl+c quit"));
        assert!(!rendered.contains("enter"));
    }

    #[test]
    fn picker_footer_percent_reports_scroll_progress() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.filtered_rows = (0..10)
            .map(|idx| {
                make_row(
                    &format!("/tmp/{idx}.jsonl"),
                    "2026-05-02T12:00:00Z",
                    &format!("row {idx}"),
                )
            })
            .collect();

        state.scroll_top = 0;
        assert_eq!(picker_footer_percent(&state, /*list_height*/ 6), 0);

        state.scroll_top = state.filtered_rows.len() - 1;
        assert_eq!(picker_footer_percent(&state, /*list_height*/ 6), 100);
    }

    #[test]
    fn picker_footer_progress_label_shows_position_total_and_percent() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.filtered_rows = (0..10)
            .map(|idx| {
                make_row(
                    &format!("/tmp/{idx}.jsonl"),
                    "2026-05-02T12:00:00Z",
                    &format!("row {idx}"),
                )
            })
            .collect();
        state.selected = 2;

        let label = picker_footer_progress_label(&state, /*list_height*/ 6, /*width*/ 80);

        assert_eq!(label, " 3 / 10 · 0% ");
        assert!(!label.contains('-'));
    }

    #[test]
    fn picker_footer_progress_label_uses_known_count_when_more_pages_exist() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.filtered_rows = (0..10)
            .map(|idx| {
                make_row(
                    &format!("/tmp/{idx}.jsonl"),
                    "2026-05-02T12:00:00Z",
                    &format!("row {idx}"),
                )
            })
            .collect();
        state.selected = 2;
        state.pagination.next_cursor = Some(PageCursor::AppServer(String::from("cursor-1")));

        let label = picker_footer_progress_label(&state, /*list_height*/ 6, /*width*/ 80);

        assert_eq!(label, " 3 / 10 · 0% ");
    }

    #[test]
    fn picker_footer_progress_label_freezes_percent_while_loading() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.filtered_rows = (0..10)
            .map(|idx| {
                make_row(
                    &format!("/tmp/{idx}.jsonl"),
                    "2026-05-02T12:00:00Z",
                    &format!("row {idx}"),
                )
            })
            .collect();
        state.selected = 9;
        state.scroll_top = 9;
        state.pagination.next_cursor = Some(PageCursor::AppServer(String::from("cursor-1")));
        state.pagination.start_load(
            /*request_token*/ 1,
            /*search_token*/ None,
            PageLoadMode::StoreDefault,
        );
        state.frozen_footer_percent = Some(37);

        let label = picker_footer_progress_label(&state, /*list_height*/ 6, /*width*/ 80);

        assert_eq!(label, " 10 / 10… · 37% ");
    }

    #[test]
    fn picker_footer_percent_is_complete_when_not_scrollable() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );

        assert_eq!(picker_footer_percent(&state, /*list_height*/ 20), 100);

        state.filtered_rows = vec![make_row(
            "/tmp/1.jsonl",
            "2026-05-02T12:00:00Z",
            "single row",
        )];
        assert_eq!(picker_footer_percent(&state, /*list_height*/ 20), 100);
    }

    #[tokio::test]
    async fn ctrl_o_toggles_density_without_typing_into_search() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.query = String::from("pick");

        state
            .handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
            .await
            .unwrap();

        assert_eq!(state.density, SessionListDensity::Dense);
        assert_eq!(state.query, "pick");
    }

    #[tokio::test]
    async fn ctrl_t_requests_selected_session_transcript() {
        let thread_id = ThreadId::new();
        let recorded_requests: Arc<Mutex<Vec<ThreadId>>> = Arc::new(Mutex::new(Vec::new()));
        let request_sink = recorded_requests.clone();
        let loader: PickerLoader = Arc::new(move |request| {
            if let PickerLoadRequest::Transcript { thread_id, .. } = request {
                request_sink.lock().unwrap().push(thread_id);
            }
        });
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.filtered_rows = vec![Row {
            path: None,
            preview: String::from("preview"),
            thread_id: Some(thread_id),
            thread_name: None,
            created_at: None,
            updated_at: None,
            cwd: None,
            git_branch: None,
            dashboard_status: None,
        }];

        state
            .handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))
            .await
            .unwrap();

        assert_eq!(state.density, SessionListDensity::Comfortable);
        assert_eq!(*recorded_requests.lock().unwrap(), vec![thread_id]);
        assert_eq!(state.pending_transcript_open, Some(thread_id));
        assert!(matches!(
            state.transcript_cells.get(&thread_id),
            Some(SessionTranscriptState::Loading)
        ));
    }

    #[tokio::test]
    async fn transcript_loading_consumes_picker_input() {
        let loader = page_only_loader(|_| {});
        let thread_id = ThreadId::new();
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.filtered_rows = vec![
            Row {
                path: None,
                preview: String::from("one"),
                thread_id: Some(ThreadId::new()),
                thread_name: None,
                created_at: None,
                updated_at: None,
                cwd: None,
                git_branch: None,
                dashboard_status: None,
            },
            Row {
                path: None,
                preview: String::from("two"),
                thread_id: Some(ThreadId::new()),
                thread_name: None,
                created_at: None,
                updated_at: None,
                cwd: None,
                git_branch: None,
                dashboard_status: None,
            },
        ];
        state.pending_transcript_open = Some(thread_id);

        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .await
            .unwrap();

        assert!(selection.is_none());
        assert_eq!(state.selected, 0);
        assert_eq!(state.query, "");

        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
            .await
            .unwrap();

        assert!(selection.is_none());
        assert_eq!(state.query, "");
    }

    #[tokio::test]
    async fn escape_cancels_transcript_loading_and_restores_picker_navigation() {
        let thread_id = ThreadId::new();
        let cancellation = Arc::new(Mutex::new(None));
        let cancellation_sink = cancellation.clone();
        let loader: PickerLoader = Arc::new(move |request| {
            if let PickerLoadRequest::Transcript { cancellation, .. } = request {
                *cancellation_sink.lock().unwrap() = Some(cancellation);
            }
        });
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        let mut first = make_row("/tmp/1.jsonl", "2026-05-02T12:00:00Z", "one");
        first.thread_id = Some(thread_id);
        state.filtered_rows = vec![
            first,
            make_row("/tmp/2.jsonl", "2026-05-02T12:00:00Z", "two"),
        ];

        state
            .handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert_eq!(state.pending_transcript_open, Some(thread_id));

        state
            .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(state.pending_transcript_open, None);
        assert!(!state.transcript_cells.contains_key(&thread_id));
        assert!(
            cancellation
                .lock()
                .unwrap()
                .as_mut()
                .expect("transcript cancellation receiver")
                .try_recv()
                .is_ok()
        );

        state
            .handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(state.selected, 1);
    }

    #[tokio::test]
    async fn transcript_loading_still_allows_ctrl_c_exit() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.pending_transcript_open = Some(ThreadId::new());

        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .await
            .unwrap();

        assert!(matches!(selection, Some(SessionSelection::Exit)));
    }

    #[test]
    fn transcript_loading_overlay_snapshot() {
        use crate::custom_terminal::Terminal;
        use crate::test_backend::VT100Backend;

        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        let thread_id = ThreadId::new();
        state.pending_transcript_open = Some(thread_id);
        state.filtered_rows = vec![
            Row {
                path: None,
                preview: String::from("Find pending threads and emails"),
                thread_id: Some(thread_id),
                thread_name: None,
                created_at: None,
                updated_at: None,
                cwd: None,
                git_branch: None,
                dashboard_status: None,
            },
            Row {
                path: None,
                preview: String::from("Plan raw scrollback mode"),
                thread_id: Some(ThreadId::new()),
                thread_name: None,
                created_at: None,
                updated_at: None,
                cwd: None,
                git_branch: None,
                dashboard_status: None,
            },
        ];
        state.update_viewport(/*rows*/ 7, /*width*/ 80);

        let width: u16 = 80;
        let height: u16 = 7;
        let backend = VT100Backend::new(width, height);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, width, height));

        {
            let mut frame = terminal.get_frame();
            let area = frame.area();
            render_list(&mut frame, area, &state);
            render_transcript_loading_overlay(&mut frame, area);
        }
        terminal.flush().expect("flush");

        let snapshot = terminal
            .backend()
            .to_string()
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n");
        assert_snapshot!("resume_picker_transcript_loading_overlay", snapshot);
    }

    #[tokio::test]
    async fn raw_ctrl_t_requests_selected_session_transcript() {
        let thread_id = ThreadId::new();
        let recorded_requests: Arc<Mutex<Vec<ThreadId>>> = Arc::new(Mutex::new(Vec::new()));
        let request_sink = recorded_requests.clone();
        let loader: PickerLoader = Arc::new(move |request| {
            if let PickerLoadRequest::Transcript { thread_id, .. } = request {
                request_sink.lock().unwrap().push(thread_id);
            }
        });
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.filtered_rows = vec![Row {
            path: None,
            preview: String::from("preview"),
            thread_id: Some(thread_id),
            thread_name: None,
            created_at: None,
            updated_at: None,
            cwd: None,
            git_branch: None,
            dashboard_status: None,
        }];

        state
            .handle_key(KeyEvent::new(KeyCode::Char('\u{0014}'), KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(*recorded_requests.lock().unwrap(), vec![thread_id]);
    }

    #[tokio::test]
    async fn ctrl_t_on_row_without_thread_id_shows_inline_error() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.filtered_rows = vec![Row {
            path: Some(PathBuf::from("/tmp/a.jsonl")),
            preview: String::from("preview"),
            thread_id: None,
            thread_name: None,
            created_at: None,
            updated_at: None,
            cwd: None,
            git_branch: None,
            dashboard_status: None,
        }];

        state
            .handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))
            .await
            .unwrap();

        assert_eq!(
            state.inline_error.as_deref(),
            Some("No transcript available for this session")
        );
    }

    #[tokio::test]
    async fn loaded_transcript_waits_for_loading_frame_before_opening_overlay() {
        use crate::history_cell::PlainHistoryCell;

        let thread_id = ThreadId::new();
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.pending_transcript_open = Some(thread_id);
        let cells: TranscriptCells =
            vec![Arc::new(PlainHistoryCell::new(vec!["transcript".into()]))];

        state
            .handle_background_event(BackgroundEvent::Transcript {
                thread_id,
                transcript: Ok(cells),
            })
            .await
            .unwrap();

        assert!(state.overlay.is_none());
        assert_eq!(state.pending_transcript_open, Some(thread_id));
        assert!(matches!(
            state.transcript_cells.get(&thread_id),
            Some(SessionTranscriptState::Loaded(_))
        ));

        assert!(state.note_transcript_loading_frame_drawn());
        state.open_pending_transcript_if_ready();

        assert!(matches!(state.overlay, Some(Overlay::Transcript(_))));
        assert_eq!(state.pending_transcript_open, None);
    }

    #[tokio::test]
    async fn cached_transcript_still_shows_loading_frame_before_opening_overlay() {
        use crate::history_cell::PlainHistoryCell;

        let thread_id = ThreadId::new();
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.filtered_rows = vec![Row {
            path: None,
            preview: String::from("preview"),
            thread_id: Some(thread_id),
            thread_name: None,
            created_at: None,
            updated_at: None,
            cwd: None,
            git_branch: None,
            dashboard_status: None,
        }];
        state.transcript_cells.insert(
            thread_id,
            SessionTranscriptState::Loaded(vec![Arc::new(PlainHistoryCell::new(vec![
                "transcript".into(),
            ]))]),
        );

        state
            .handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))
            .await
            .unwrap();

        assert!(state.overlay.is_none());
        assert_eq!(state.pending_transcript_open, Some(thread_id));

        assert!(state.note_transcript_loading_frame_drawn());
        state.open_pending_transcript_if_ready();

        assert!(matches!(state.overlay, Some(Overlay::Transcript(_))));
        assert_eq!(state.pending_transcript_open, None);
    }

    #[tokio::test]
    async fn ctrl_o_persists_density_preference() {
        let tmp = tempdir().expect("tmpdir");
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.view_persistence = Some(SessionPickerViewPersistence {
            codex_home: tmp.path().to_path_buf(),
        });

        state
            .handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
            .await
            .unwrap();

        assert_eq!(state.density, SessionListDensity::Dense);
        let contents =
            std::fs::read_to_string(tmp.path().join(CONFIG_TOML_FILE)).expect("read config");
        assert_eq!(
            contents,
            r#"[tui]
session_picker_view = "dense"
"#
        );
    }

    #[tokio::test]
    async fn ctrl_o_keeps_toggled_density_when_persistence_fails() {
        let tmp = tempdir().expect("tmpdir");
        let codex_home_file = tmp.path().join("codex-home-file");
        std::fs::write(&codex_home_file, "not a directory").expect("write codex home file");
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.view_persistence = Some(SessionPickerViewPersistence {
            codex_home: codex_home_file,
        });

        state
            .handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
            .await
            .unwrap();

        assert_eq!(state.density, SessionListDensity::Dense);
        assert!(
            state
                .inline_error
                .as_deref()
                .is_some_and(|error| error.contains("Failed to save view mode")),
            "expected persistence error, got {:?}",
            state.inline_error
        );
    }

    #[tokio::test]
    async fn raw_ctrl_o_toggles_density_without_typing_into_search() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.query = String::from("pick");

        state
            .handle_key(KeyEvent::new(KeyCode::Char('\u{000f}'), KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(state.density, SessionListDensity::Dense);
        assert_eq!(state.query, "pick");
    }

    #[tokio::test]
    async fn space_appends_to_search_query() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.query = String::from("resize");

        state
            .handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))
            .await
            .unwrap();
        state
            .handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(state.query, "resize r");
        assert_eq!(state.expanded_thread_id, None);
    }

    #[tokio::test]
    async fn ctrl_e_toggles_selected_session_expansion() {
        let thread_id = ThreadId::new();
        let recorded_requests: Arc<Mutex<Vec<ThreadId>>> = Arc::new(Mutex::new(Vec::new()));
        let request_sink = recorded_requests.clone();
        let loader: PickerLoader = Arc::new(move |request| {
            if let PickerLoadRequest::Preview { thread_id } = request {
                request_sink.lock().unwrap().push(thread_id);
            }
        });
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.filtered_rows = vec![Row {
            path: None,
            preview: String::from("preview"),
            thread_id: Some(thread_id),
            thread_name: None,
            created_at: None,
            updated_at: None,
            cwd: None,
            git_branch: None,
            dashboard_status: None,
        }];

        state
            .handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL))
            .await
            .unwrap();

        assert_eq!(state.expanded_thread_id, Some(thread_id));
        assert_eq!(*recorded_requests.lock().unwrap(), vec![thread_id]);

        state
            .handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL))
            .await
            .unwrap();

        assert_eq!(state.expanded_thread_id, None);
    }

    #[tokio::test]
    async fn raw_ctrl_e_toggles_selected_session_expansion() {
        let thread_id = ThreadId::new();
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.filtered_rows = vec![Row {
            path: None,
            preview: String::from("preview"),
            thread_id: Some(thread_id),
            thread_name: None,
            created_at: None,
            updated_at: None,
            cwd: None,
            git_branch: None,
            dashboard_status: None,
        }];

        state
            .handle_key(KeyEvent::new(KeyCode::Char('\u{0005}'), KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(state.expanded_thread_id, Some(thread_id));
    }

    #[test]
    fn search_line_renders_sort_and_filter_tabs() {
        use crate::custom_terminal::Terminal;
        use crate::test_backend::VT100Backend;

        let loader = page_only_loader(|_| {});
        let state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ false,
            Some(PathBuf::from("/tmp/project")),
            SessionPickerAction::Resume,
        );

        let width: u16 = 100;
        let backend = VT100Backend::new(width, /*height*/ 1);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, width, 1));

        {
            let mut frame = terminal.get_frame();
            let line = search_line(&state, frame.area().width);
            frame.render_widget_ref(&line, frame.area());
        }
        terminal.flush().expect("flush");

        assert_snapshot!(
            "resume_picker_search_line_sort_filter_tabs",
            terminal.backend().to_string()
        );
    }

    #[test]
    fn search_line_compacts_toolbar_on_narrow_width() {
        let loader = page_only_loader(|_| {});
        let state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ false,
            Some(PathBuf::from("/tmp/project")),
            SessionPickerAction::Resume,
        );

        let line = search_line(&state, /*width*/ 40).to_string();

        assert!(line.contains("Filter:[Cwd]"));
        assert!(line.contains("[Active]"));
        assert!(line.contains("Sort:[Updated]"));
        assert!(line.find("Filter:[Cwd]") < line.find("Sort:[Updated]"));
    }

    fn dense_snapshot_row() -> Row {
        Row {
            path: Some(PathBuf::from("/tmp/a.jsonl")),
            preview: String::from(
                "Propose session picker redesign with enough title text to exercise truncation",
            ),
            thread_id: Some(
                ThreadId::from_string("019dabc1-0ef5-7431-b81c-03037f51f62c").expect("thread id"),
            ),
            thread_name: None,
            created_at: parse_timestamp_str("2026-04-28T16:30:00Z"),
            updated_at: parse_timestamp_str("2026-04-28T17:45:00Z"),
            cwd: Some(PathBuf::from(
                "/Users/felipe.coury/code/codex.fcoury-session-picker/codex-rs",
            )),
            git_branch: Some(String::from("fcoury/session-picker")),
            dashboard_status: None,
        }
    }

    fn render_dense_row_snapshot(
        show_all: bool,
        filter_cwd: Option<PathBuf>,
        width: u16,
    ) -> String {
        use crate::custom_terminal::Terminal;
        use crate::test_backend::VT100Backend;

        let loader = page_only_loader(|_| {});
        let row = dense_snapshot_row();
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            show_all,
            filter_cwd,
            SessionPickerAction::Resume,
        );
        state.density = SessionListDensity::Dense;
        state.all_rows = vec![row.clone()];
        state.filtered_rows = vec![row];
        state.relative_time_reference =
            Some(parse_timestamp_str("2026-04-28T18:00:00Z").expect("timestamp"));

        let backend = VT100Backend::new(width, /*height*/ 3);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, width, 3));

        {
            let mut frame = terminal.get_frame();
            let area = frame.area();
            render_list(&mut frame, area, &state);
        }
        terminal.flush().expect("flush");

        terminal.backend().to_string()
    }

    #[test]
    fn dense_session_snapshot_omits_cwd_in_cwd_filter() {
        assert_snapshot!(
            "resume_picker_dense_cwd",
            render_dense_row_snapshot(
                /*show_all*/ false,
                Some(PathBuf::from(
                    "/Users/felipe.coury/code/codex.fcoury-session-picker/codex-rs"
                )),
                /*width*/ 100,
            )
        );
    }

    #[test]
    fn dense_session_snapshot_includes_cwd_in_all_filter() {
        assert_snapshot!(
            "resume_picker_dense_all",
            render_dense_row_snapshot(
                /*show_all*/ true, /*filter_cwd*/ None, /*width*/ 120,
            )
        );
    }

    #[test]
    fn dense_session_snapshot_auto_hides_cwd_when_narrow() {
        assert_snapshot!(
            "resume_picker_dense_all_auto_hidden_cwd",
            render_dense_row_snapshot(
                /*show_all*/ true, /*filter_cwd*/ None, /*width*/ 100,
            )
        );
    }

    #[test]
    fn dense_session_snapshot_forces_cwd_when_narrow() {
        assert_snapshot!(
            "resume_picker_dense_all_forced_cwd",
            render_dense_row_snapshot(
                /*show_all*/ true, /*filter_cwd*/ None, /*width*/ 48,
            )
        );
    }

    #[test]
    fn dense_session_snapshot_drops_metadata_when_narrow() {
        assert_snapshot!(
            "resume_picker_dense_narrow",
            render_dense_row_snapshot(
                /*show_all*/ true, /*filter_cwd*/ None, /*width*/ 48,
            )
        );
    }

    #[test]
    fn dense_session_line_prefers_thread_name_over_preview() {
        let mut row = dense_snapshot_row();
        row.preview = String::from("Raw conversation preview");
        row.thread_name = Some(String::from("Named session"));

        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.relative_time_reference =
            Some(parse_timestamp_str("2026-04-28T18:00:00Z").expect("timestamp"));

        let rendered = render_dense_session_lines(
            &row, &state, /*is_selected*/ false, /*is_expanded*/ false,
            /*is_zebra*/ false, /*width*/ 100,
        )
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");

        assert!(rendered.contains("Named session"));
        assert!(!rendered.contains("Raw conversation preview"));
    }

    #[test]
    fn dense_selected_summary_line_uses_full_width_selection_style() {
        let line = dense_summary_line(DenseSummaryInput {
            marker: selection_marker(/*is_selected*/ true, /*is_expanded*/ false),
            date: "15m ago",
            title: "Selected dense row",
            dashboard_status: None,
            is_dashboard: false,
            is_selected: true,
            is_zebra: false,
            width: 80,
        });

        assert_eq!(line.width(), 80);
        assert_eq!(line.style.fg, selected_session_style().fg);
        assert_eq!(line.spans[0].content, "❯ ");
    }

    #[test]
    fn dense_zebra_summary_line_uses_full_width_background() {
        let line = dense_summary_line(DenseSummaryInput {
            marker: selection_marker(/*is_selected*/ false, /*is_expanded*/ false),
            date: "15m ago",
            title: "Zebra dense row",
            dashboard_status: None,
            is_dashboard: false,
            is_selected: false,
            is_zebra: true,
            width: 80,
        });

        assert_eq!(line.width(), 80);
        assert_eq!(line.style.bg, dense_zebra_style().bg);
    }

    #[test]
    fn comfortable_zebra_lines_use_full_width_background() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.relative_time_reference =
            Some(parse_timestamp_str("2026-05-02T12:00:00Z").expect("timestamp"));
        let row = make_row(
            "/tmp/a.jsonl",
            "2026-05-02T11:45:00Z",
            "Zebra comfortable row",
        );

        let lines = render_comfortable_session_lines(
            &row, &state, /*is_selected*/ false, /*is_expanded*/ false,
            /*is_zebra*/ true, /*width*/ 100,
        );

        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|line| line.width() == 100));
        assert!(
            lines
                .iter()
                .all(|line| line.style.bg == dense_zebra_style().bg)
        );
    }

    #[test]
    fn dense_session_snapshot_uses_no_blank_lines_between_rows() {
        use crate::custom_terminal::Terminal;
        use crate::test_backend::VT100Backend;

        let loader = page_only_loader(|_| {});
        let mut first = dense_snapshot_row();
        first.preview = String::from("First dense row");
        let mut second = dense_snapshot_row();
        second.preview = String::from("Second dense row");
        second.git_branch = Some(String::from("fcoury/other-branch"));
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ false,
            Some(PathBuf::from(
                "/Users/felipe.coury/code/codex.fcoury-session-picker/codex-rs",
            )),
            SessionPickerAction::Resume,
        );
        state.density = SessionListDensity::Dense;
        state.all_rows = vec![first.clone(), second.clone()];
        state.filtered_rows = vec![first, second];
        state.selected = 1;
        state.relative_time_reference =
            Some(parse_timestamp_str("2026-04-28T18:00:00Z").expect("timestamp"));

        let backend = VT100Backend::new(/*width*/ 80, /*height*/ 2);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, 80, 2));

        {
            let mut frame = terminal.get_frame();
            let area = frame.area();
            render_list(&mut frame, area, &state);
        }
        terminal.flush().expect("flush");

        assert_snapshot!(
            "resume_picker_dense_no_blank_lines",
            terminal.backend().to_string()
        );
    }

    #[test]
    fn expanded_session_snapshot() {
        use crate::custom_terminal::Terminal;
        use crate::test_backend::VT100Backend;

        let loader = page_only_loader(|_| {});
        let thread_id =
            ThreadId::from_string("019dabc1-0ef5-7431-b81c-03037f51f62c").expect("thread id");
        let row = Row {
            path: Some(PathBuf::from("/tmp/a.jsonl")),
            preview: String::from("Investigate picker expansion"),
            thread_id: Some(thread_id),
            thread_name: None,
            created_at: parse_timestamp_str("2026-04-28T16:30:00Z"),
            updated_at: parse_timestamp_str("2026-04-28T17:45:00Z"),
            cwd: Some(PathBuf::from("/tmp/codex")),
            git_branch: Some(String::from("fcoury/session-picker")),
            dashboard_status: None,
        };
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.all_rows = vec![row.clone()];
        state.filtered_rows = vec![row];
        state.relative_time_reference =
            Some(parse_timestamp_str("2026-04-28T18:00:00Z").expect("timestamp"));
        state.expanded_thread_id = Some(thread_id);
        state.transcript_previews.insert(
            thread_id,
            TranscriptPreviewState::Loaded(vec![
                TranscriptPreviewLine {
                    speaker: TranscriptPreviewSpeaker::User,
                    text: String::from("Show me the recent transcript"),
                },
                TranscriptPreviewLine {
                    speaker: TranscriptPreviewSpeaker::Assistant,
                    text: String::from("Here are the *last* few lines."),
                },
            ]),
        );

        let width: u16 = 90;
        let height: u16 = 11;
        let backend = VT100Backend::new(width, height);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, width, height));

        {
            let mut frame = terminal.get_frame();
            let area = frame.area();
            render_list(&mut frame, area, &state);
        }
        terminal.flush().expect("flush");

        let rendered = terminal
            .backend()
            .to_string()
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n");

        assert_snapshot!("resume_picker_expanded_session", rendered);
    }

    #[test]
    fn narrow_session_snapshot() {
        use crate::custom_terminal::Terminal;
        use crate::test_backend::VT100Backend;

        let loader = page_only_loader(|_| {});
        let row = Row {
            path: Some(PathBuf::from("/tmp/a.jsonl")),
            preview: String::from("Investigate picker expansion"),
            thread_id: Some(
                ThreadId::from_string("019dabc1-0ef5-7431-b81c-03037f51f62c").expect("thread id"),
            ),
            thread_name: None,
            created_at: parse_timestamp_str("2026-04-28T16:30:00Z"),
            updated_at: parse_timestamp_str("2026-04-28T17:45:00Z"),
            cwd: Some(PathBuf::from("/tmp/codex")),
            git_branch: Some(String::from("fcoury/session-picker")),
            dashboard_status: None,
        };
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.all_rows = vec![row.clone()];
        state.filtered_rows = vec![row];
        state.relative_time_reference =
            Some(parse_timestamp_str("2026-04-28T18:00:00Z").expect("timestamp"));

        let width: u16 = 58;
        let height: u16 = 6;
        let backend = VT100Backend::new(width, height);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, width, height));

        {
            let mut frame = terminal.get_frame();
            let area = frame.area();
            render_list(&mut frame, area, &state);
        }
        terminal.flush().expect("flush");

        assert_snapshot!(
            "resume_picker_narrow_session",
            terminal.backend().to_string()
        );
    }

    #[test]
    fn session_list_more_indicators_snapshot() {
        use crate::custom_terminal::Terminal;
        use crate::test_backend::VT100Backend;

        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        let now = parse_timestamp_str("2026-04-28T16:30:00Z").expect("timestamp");
        state.all_rows = (0..5)
            .map(|idx| Row {
                path: Some(PathBuf::from(format!("/tmp/{idx}.jsonl"))),
                preview: format!("item-{idx}"),
                thread_id: None,
                thread_name: None,
                created_at: Some(now - Duration::hours(idx)),
                updated_at: Some(now - Duration::minutes(idx * 5)),
                cwd: None,
                git_branch: None,
                dashboard_status: None,
            })
            .collect();
        state.filtered_rows = state.all_rows.clone();
        state.relative_time_reference = Some(now);
        state.selected = 2;
        state.scroll_top = 1;
        state.update_viewport(/*rows*/ 6, /*width*/ 80);

        let width: u16 = 80;
        let height: u16 = 6;
        let backend = VT100Backend::new(width, height);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, width, height));

        {
            let mut frame = terminal.get_frame();
            let area = frame.area();
            render_list(&mut frame, area, &state);
        }
        terminal.flush().expect("flush");

        assert_snapshot!(
            "resume_picker_more_indicators",
            terminal.backend().to_string()
        );
    }

    #[test]
    fn density_toggle_clears_stale_more_indicator() {
        use crate::custom_terminal::Terminal;
        use crate::test_backend::VT100Backend;

        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        let now = parse_timestamp_str("2026-04-28T16:30:00Z").expect("timestamp");
        state.all_rows = (0..4)
            .map(|idx| Row {
                path: Some(PathBuf::from(format!("/tmp/{idx}.jsonl"))),
                preview: format!("item-{idx}"),
                thread_id: None,
                thread_name: None,
                created_at: Some(now - Duration::hours(idx)),
                updated_at: Some(now - Duration::minutes(idx * 5)),
                cwd: None,
                git_branch: None,
                dashboard_status: None,
            })
            .collect();
        state.filtered_rows = state.all_rows.clone();
        state.relative_time_reference = Some(now);

        let width: u16 = 80;
        let height: u16 = 6;
        let backend = VT100Backend::new(width, height);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, width, height));

        state.update_viewport(height as usize, width);
        {
            let mut frame = terminal.get_frame();
            let area = frame.area();
            render_list(&mut frame, area, &state);
        }
        terminal.flush().expect("flush");
        assert!(terminal.backend().to_string().contains("↓ more"));

        state.density = SessionListDensity::Dense;
        state.update_viewport(height as usize, width);
        {
            let mut frame = terminal.get_frame();
            let area = frame.area();
            render_list(&mut frame, area, &state);
        }
        terminal.flush().expect("flush");

        assert!(!terminal.backend().to_string().contains("↓ more"));
    }

    #[test]
    fn pageless_scrolling_deduplicates_and_keeps_order() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );

        state.reset_pagination();
        state.ingest_page(page(
            vec![
                make_row("/tmp/a.jsonl", "2025-01-03T00:00:00Z", "third"),
                make_row("/tmp/b.jsonl", "2025-01-02T00:00:00Z", "second"),
            ],
            Some("2025-01-02T00:00:00Z"),
            /*num_scanned_files*/ 2,
            /*reached_scan_cap*/ false,
        ));

        state.ingest_page(page(
            vec![
                make_row("/tmp/a.jsonl", "2025-01-03T00:00:00Z", "duplicate"),
                make_row("/tmp/c.jsonl", "2025-01-01T00:00:00Z", "first"),
            ],
            Some("2025-01-01T00:00:00Z"),
            /*num_scanned_files*/ 2,
            /*reached_scan_cap*/ false,
        ));

        state.ingest_page(page(
            vec![make_row("/tmp/d.jsonl", "2024-12-31T23:00:00Z", "very old")],
            /*next_cursor*/ None,
            /*num_scanned_files*/ 1,
            /*reached_scan_cap*/ false,
        ));

        let previews: Vec<_> = state
            .filtered_rows
            .iter()
            .map(|row| row.preview.as_str())
            .collect();
        assert_eq!(previews, vec!["third", "second", "first", "very old"]);

        let unique_paths = state
            .filtered_rows
            .iter()
            .map(|row| row.path.clone())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique_paths.len(), 4);
    }

    #[test]
    fn ensure_minimum_rows_prefetches_when_underfilled() {
        let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let request_sink = recorded_requests.clone();
        let loader = page_only_loader(move |req: PageLoadRequest| {
            request_sink.lock().unwrap().push(req);
        });

        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.reset_pagination();
        state.ingest_page(page(
            vec![
                make_row("/tmp/a.jsonl", "2025-01-01T00:00:00Z", "one"),
                make_row("/tmp/b.jsonl", "2025-01-02T00:00:00Z", "two"),
            ],
            Some("2025-01-03T00:00:00Z"),
            /*num_scanned_files*/ 2,
            /*reached_scan_cap*/ false,
        ));

        assert!(recorded_requests.lock().unwrap().is_empty());
        state.ensure_minimum_rows_for_view(/*minimum_rows*/ 10);
        let guard = recorded_requests.lock().unwrap();
        assert_eq!(guard.len(), 1);
        assert!(guard[0].search_token.is_none());
    }

    #[test]
    fn ensure_minimum_rows_does_not_prefetch_when_comfortable_cards_fill_view() {
        let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let request_sink = recorded_requests.clone();
        let loader = page_only_loader(move |req: PageLoadRequest| {
            request_sink.lock().unwrap().push(req);
        });

        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.reset_pagination();
        state.ingest_page(page(
            vec![
                make_row("/tmp/a.jsonl", "2025-01-01T00:00:00Z", "one"),
                make_row("/tmp/b.jsonl", "2025-01-02T00:00:00Z", "two"),
                make_row("/tmp/c.jsonl", "2025-01-03T00:00:00Z", "three"),
                make_row("/tmp/d.jsonl", "2025-01-04T00:00:00Z", "four"),
            ],
            Some("2025-01-05T00:00:00Z"),
            /*num_scanned_files*/ 4,
            /*reached_scan_cap*/ false,
        ));
        state.update_viewport(/*rows*/ 6, /*width*/ 80);

        state.ensure_minimum_rows_for_view(/*minimum_rows*/ 6);

        assert!(recorded_requests.lock().unwrap().is_empty());
    }

    #[test]
    fn ensure_minimum_rows_still_prefetches_when_dense_rows_underfill_view() {
        let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let request_sink = recorded_requests.clone();
        let loader = page_only_loader(move |req: PageLoadRequest| {
            request_sink.lock().unwrap().push(req);
        });

        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.density = SessionListDensity::Dense;
        state.reset_pagination();
        state.ingest_page(page(
            vec![
                make_row("/tmp/a.jsonl", "2025-01-01T00:00:00Z", "one"),
                make_row("/tmp/b.jsonl", "2025-01-02T00:00:00Z", "two"),
            ],
            Some("2025-01-03T00:00:00Z"),
            /*num_scanned_files*/ 2,
            /*reached_scan_cap*/ false,
        ));
        state.update_viewport(/*rows*/ 10, /*width*/ 80);

        state.ensure_minimum_rows_for_view(/*minimum_rows*/ 10);

        let guard = recorded_requests.lock().unwrap();
        assert_eq!(guard.len(), 1);
        assert!(guard[0].search_token.is_none());
    }

    #[test]
    fn list_viewport_width_matches_rendered_list_inset() {
        let dashboard = dashboard_state(Vec::new());
        let mut regular = dashboard_state(Vec::new());
        regular.launch_context = SessionPickerLaunchContext::Startup;

        assert_eq!(list_viewport_width(/*width*/ 80, &regular), 76);
        assert_eq!(list_viewport_width(/*width*/ 3, &regular), 0);
        assert_eq!(list_viewport_width(/*width*/ 80, &dashboard), 78);
        assert_eq!(list_viewport_width(/*width*/ 3, &dashboard), 1);
    }

    #[tokio::test]
    async fn toggle_sort_key_reloads_with_new_sort() {
        let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let request_sink = recorded_requests.clone();
        let loader = page_only_loader(move |req: PageLoadRequest| {
            request_sink.lock().unwrap().push(req);
        });

        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );

        state.start_initial_load();
        {
            let guard = recorded_requests.lock().unwrap();
            assert_eq!(guard.len(), 1);
            assert_eq!(guard[0].sort_key, ThreadSortKey::UpdatedAt);
        }

        state
            .handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        state
            .handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        state
            .handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL))
            .await
            .unwrap();

        let guard = recorded_requests.lock().unwrap();
        assert_eq!(guard.len(), 2);
        assert_eq!(guard[1].sort_key, ThreadSortKey::CreatedAt);
    }

    #[tokio::test]
    async fn default_filter_focus_arrows_reload_with_new_filter() {
        let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let request_sink = recorded_requests.clone();
        let loader = page_only_loader(move |req: PageLoadRequest| {
            request_sink.lock().unwrap().push(req);
        });

        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ false,
            Some(PathBuf::from("/tmp/project")),
            SessionPickerAction::Resume,
        );

        state.start_initial_load();
        {
            let guard = recorded_requests.lock().unwrap();
            assert_eq!(guard.len(), 1);
            assert_eq!(guard[0].cwd_filter, Some(PathBuf::from("/tmp/project")));
        }

        state
            .handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
            .await
            .unwrap();

        let guard = recorded_requests.lock().unwrap();
        assert_eq!(guard.len(), 2);
        assert_eq!(guard[1].cwd_filter, None);
    }

    #[tokio::test]
    async fn all_filter_can_switch_back_to_cwd_when_cwd_candidate_exists() {
        let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let request_sink = recorded_requests.clone();
        let loader = page_only_loader(move |req: PageLoadRequest| {
            request_sink.lock().unwrap().push(req);
        });

        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            Some(PathBuf::from("/tmp/project")),
            SessionPickerAction::Resume,
        );

        state.start_initial_load();
        {
            let guard = recorded_requests.lock().unwrap();
            assert_eq!(guard.len(), 1);
            assert_eq!(guard[0].cwd_filter, None);
        }

        state
            .handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
            .await
            .unwrap();

        let guard = recorded_requests.lock().unwrap();
        assert_eq!(guard.len(), 2);
        assert_eq!(guard[1].cwd_filter, Some(PathBuf::from("/tmp/project")));
    }

    #[tokio::test]
    async fn status_changes_when_directory_filter_is_unavailable() {
        let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let request_sink = recorded_requests.clone();
        let loader = page_only_loader(move |req: PageLoadRequest| {
            request_sink.lock().unwrap().push(req);
        });

        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::Any,
            /*show_all*/ false,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );

        assert_eq!(
            search_line(&state, /*width*/ 80)
                .to_string()
                .matches("Cwd")
                .count(),
            0
        );

        state.start_initial_load();
        state
            .handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
            .await
            .unwrap();
        state
            .handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        state
            .handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
            .await
            .unwrap();

        let guard = recorded_requests.lock().unwrap();
        assert_eq!(guard.len(), 2);
        assert_eq!(guard[0].status, SessionStatus::Active);
        assert_eq!(guard[0].cwd_filter, None);
        assert_eq!(guard[1].status, SessionStatus::Archived);
        assert_eq!(guard[1].cwd_filter, None);
    }

    #[tokio::test]
    async fn page_navigation_uses_view_rows() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );

        let mut items = Vec::new();
        for idx in 0..20 {
            let ts = format!("2025-01-{:02}T00:00:00Z", idx + 1);
            let preview = format!("item-{idx}");
            let path = format!("/tmp/item-{idx}.jsonl");
            items.push(make_row(&path, &ts, &preview));
        }

        state.reset_pagination();
        state.ingest_page(page(
            items, /*next_cursor*/ None, /*num_scanned_files*/ 20,
            /*reached_scan_cap*/ false,
        ));
        state.update_viewport(/*rows*/ 5, /*width*/ 80);

        assert_eq!(state.selected, 0);
        state
            .handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(state.selected, 5);

        state
            .handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(state.selected, 10);

        state
            .handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(state.selected, 5);

        state
            .handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(state.selected, 19);

        state
            .handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(state.selected, 0);
    }

    #[tokio::test]
    async fn page_and_jump_navigation_use_list_keymap() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.list_keymap.page_down = vec![crate::key_hint::ctrl(KeyCode::Char('d'))];
        state.list_keymap.page_up = vec![crate::key_hint::ctrl(KeyCode::Char('u'))];
        state.list_keymap.jump_bottom = vec![crate::key_hint::ctrl(KeyCode::Char('y'))];
        state.list_keymap.jump_top = vec![crate::key_hint::ctrl(KeyCode::Char('a'))];

        let mut items = Vec::new();
        for idx in 0..20 {
            let ts = format!("2025-01-{:02}T00:00:00Z", idx + 1);
            let preview = format!("item-{idx}");
            let path = format!("/tmp/item-{idx}.jsonl");
            items.push(make_row(&path, &ts, &preview));
        }

        state.reset_pagination();
        state.ingest_page(page(
            items, /*next_cursor*/ None, /*num_scanned_files*/ 20,
            /*reached_scan_cap*/ false,
        ));
        state.update_viewport(/*rows*/ 5, /*width*/ 80);

        state
            .handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(state.selected, 0);

        state
            .handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert_eq!(state.selected, 5);

        state
            .handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert_eq!(state.selected, 0);

        state
            .handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert_eq!(state.selected, 19);

        state
            .handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert_eq!(state.selected, 0);
    }

    #[tokio::test]
    async fn ctrl_c_exits_even_when_cancel_is_remapped_to_ctrl_c() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.list_keymap.cancel = vec![crate::key_hint::ctrl(KeyCode::Char('c'))];

        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .await
            .unwrap();

        assert!(matches!(selection, Some(SessionSelection::Exit)));
    }

    #[tokio::test]
    async fn end_jumps_to_last_known_row_and_starts_loading_more() {
        let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let request_sink = recorded_requests.clone();
        let loader = page_only_loader(move |req: PageLoadRequest| {
            request_sink.lock().unwrap().push(req);
        });
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );

        let items = (0..10)
            .map(|idx| {
                make_row(
                    &format!("/tmp/{idx}.jsonl"),
                    "2026-05-02T12:00:00Z",
                    &format!("row {idx}"),
                )
            })
            .collect();
        state.reset_pagination();
        state.ingest_page(page(
            items,
            Some("cursor-1"),
            /*num_scanned_files*/ 10,
            /*reached_scan_cap*/ false,
        ));
        state.update_viewport(/*rows*/ 5, /*width*/ 80);

        state
            .handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(state.selected, 9);
        assert!(state.pagination.is_loading());
        assert_eq!(recorded_requests.lock().unwrap().len(), 1);
        assert_eq!(
            picker_footer_progress_label(&state, /*list_height*/ 5, /*width*/ 80),
            " 10 / 10… · 100% "
        );
    }

    #[tokio::test]
    async fn enter_on_row_without_resolvable_thread_id_shows_inline_error() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );

        let row = Row {
            path: Some(PathBuf::from("/tmp/missing.jsonl")),
            preview: String::from("missing metadata"),
            thread_id: None,
            thread_name: None,
            created_at: None,
            updated_at: None,
            cwd: None,
            git_branch: None,
            dashboard_status: None,
        };
        state.all_rows = vec![row.clone()];
        state.filtered_rows = vec![row];

        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("enter should not abort the picker");

        assert!(selection.is_none());
        assert_eq!(
            state.inline_error,
            Some(String::from(
                "Failed to read session metadata from /tmp/missing.jsonl"
            ))
        );
    }

    #[tokio::test]
    async fn enter_on_pathless_thread_uses_thread_id() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        let thread_id = ThreadId::new();
        let row = Row {
            path: None,
            preview: String::from("pathless thread"),
            thread_id: Some(thread_id),
            thread_name: None,
            created_at: None,
            updated_at: None,
            cwd: None,
            git_branch: None,
            dashboard_status: None,
        };
        state.all_rows = vec![row.clone()];
        state.filtered_rows = vec![row];

        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .expect("enter should not abort the picker");

        match selection {
            Some(SessionSelection::Resume(SessionTarget {
                path: None,
                thread_id: selected_thread_id,
            })) => assert_eq!(selected_thread_id, thread_id),
            other => panic!("unexpected selection: {other:?}"),
        }
    }

    #[test]
    fn app_server_row_keeps_pathless_threads() {
        let thread_id = ThreadId::new();
        let thread = Thread {
            id: thread_id.to_string(),
            extra: None,
            session_id: thread_id.to_string(),
            forked_from_id: None,
            parent_thread_id: None,
            preview: String::from("remote thread"),
            ephemeral: false,
            section: None,
            section_entered_at: None,
            history_mode: Default::default(),
            model_provider: String::from("openai"),
            created_at: 1,
            updated_at: 2,
            recency_at: Some(2),
            status: codex_app_server_protocol::ThreadStatus::Idle,
            path: None,
            cwd: test_path_buf("/tmp").abs(),
            cli_version: String::from("0.0.0"),
            source: codex_app_server_protocol::SessionSource::Cli,
            can_accept_direct_input: None,
            thread_source: None,
            agent_nickname: None,
            agent_role: None,
            git_info: None,
            name: Some(String::from("Named thread")),
            turns: Vec::new(),
        };

        let mut child_thread = thread.clone();
        child_thread.parent_thread_id = Some(ThreadId::new().to_string());
        assert!(dashboard_thread_is_root(&thread));
        assert!(!dashboard_thread_is_root(&child_thread));
        assert!(thread_visible_in_picker(&child_thread, false));
        assert!(!thread_visible_in_picker(&child_thread, true));

        let row = row_from_app_server_thread(thread, /*show_dashboard_status*/ false)
            .expect("row should be preserved");

        assert_eq!(row.path, None);
        assert_eq!(row.thread_id, Some(thread_id));
        assert_eq!(row.thread_name, Some(String::from("Named thread")));
    }

    #[test]
    fn thread_to_transcript_cells_renders_core_message_types() {
        use crate::thread_transcript::thread_to_transcript_cells;

        let thread_id = ThreadId::new();
        let thread = Thread {
            id: thread_id.to_string(),
            extra: None,
            session_id: thread_id.to_string(),
            forked_from_id: None,
            parent_thread_id: None,
            preview: String::from("preview"),
            ephemeral: false,
            section: None,
            section_entered_at: None,
            history_mode: Default::default(),
            model_provider: String::from("openai"),
            created_at: 1,
            updated_at: 2,
            recency_at: Some(2),
            status: codex_app_server_protocol::ThreadStatus::Idle,
            path: None,
            cwd: test_path_buf("/tmp").abs(),
            cli_version: String::from("0.0.0"),
            source: codex_app_server_protocol::SessionSource::Cli,
            can_accept_direct_input: None,
            thread_source: None,
            agent_nickname: None,
            agent_role: None,
            git_info: None,
            name: None,
            turns: vec![codex_app_server_protocol::Turn {
                id: String::from("turn-1"),
                items_view: codex_app_server_protocol::TurnItemsView::Full,
                items: vec![
                    ThreadItem::UserMessage {
                        id: String::from("user-1"),
                        client_id: None,
                        content: vec![codex_app_server_protocol::UserInput::Text {
                            text: String::from("hello from user"),
                            text_elements: Vec::new(),
                        }],
                    },
                    ThreadItem::AgentMessage {
                        id: String::from("agent-1"),
                        text: String::from("hello from assistant"),
                        phase: None,
                        memory_citation: None,
                    },
                    ThreadItem::Plan {
                        id: String::from("plan-1"),
                        text: String::from("1. Do the thing"),
                    },
                ],
                status: codex_app_server_protocol::TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            }],
        };

        let rendered = thread_to_transcript_cells(
            thread,
            RawReasoningVisibility::Visible,
            /*codex_home*/ None,
        )
        .into_iter()
        .flat_map(|cell| cell.transcript_lines(/*width*/ 80))
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");

        assert!(rendered.contains("hello from user"));
        assert!(rendered.contains("hello from assistant"));
        assert!(rendered.contains("Proposed Plan"));
        assert!(rendered.contains("Do the thing"));
    }

    #[test]
    fn thread_to_transcript_cells_hides_raw_reasoning_when_not_enabled() {
        use crate::thread_transcript::thread_to_transcript_cells;

        let thread_id = ThreadId::new();
        let thread = Thread {
            id: thread_id.to_string(),
            extra: None,
            session_id: thread_id.to_string(),
            forked_from_id: None,
            parent_thread_id: None,
            preview: String::from("preview"),
            ephemeral: false,
            section: None,
            section_entered_at: None,
            history_mode: Default::default(),
            model_provider: String::from("openai"),
            created_at: 1,
            updated_at: 2,
            recency_at: Some(2),
            status: codex_app_server_protocol::ThreadStatus::Idle,
            path: None,
            cwd: test_path_buf("/tmp").abs(),
            cli_version: String::from("0.0.0"),
            source: codex_app_server_protocol::SessionSource::Cli,
            can_accept_direct_input: None,
            thread_source: None,
            agent_nickname: None,
            agent_role: None,
            git_info: None,
            name: None,
            turns: vec![codex_app_server_protocol::Turn {
                id: String::from("turn-1"),
                items_view: codex_app_server_protocol::TurnItemsView::Full,
                items: vec![ThreadItem::Reasoning {
                    id: String::from("reasoning-1"),
                    summary: Vec::new(),
                    content: vec![String::from("private raw chain of thought")],
                }],
                status: codex_app_server_protocol::TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            }],
        };

        let hidden = thread_to_transcript_cells(
            thread.clone(),
            RawReasoningVisibility::Hidden,
            /*codex_home*/ None,
        )
        .into_iter()
        .flat_map(|cell| cell.transcript_lines(/*width*/ 80))
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
        let visible = thread_to_transcript_cells(
            thread,
            RawReasoningVisibility::Visible,
            /*codex_home*/ None,
        )
        .into_iter()
        .flat_map(|cell| cell.transcript_lines(/*width*/ 80))
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");

        assert!(!hidden.contains("private raw chain of thought"));
        assert!(visible.contains("private raw chain of thought"));
    }

    #[test]
    fn thread_to_transcript_cells_shows_raw_reasoning_over_summary_when_enabled() {
        use crate::thread_transcript::thread_to_transcript_cells;

        let thread_id = ThreadId::new();
        let thread = Thread {
            id: thread_id.to_string(),
            extra: None,
            session_id: thread_id.to_string(),
            forked_from_id: None,
            parent_thread_id: None,
            preview: String::from("preview"),
            ephemeral: false,
            section: None,
            section_entered_at: None,
            history_mode: Default::default(),
            model_provider: String::from("openai"),
            created_at: 1,
            updated_at: 2,
            recency_at: Some(2),
            status: codex_app_server_protocol::ThreadStatus::Idle,
            path: None,
            cwd: test_path_buf("/tmp").abs(),
            cli_version: String::from("0.0.0"),
            source: codex_app_server_protocol::SessionSource::Cli,
            can_accept_direct_input: None,
            thread_source: None,
            agent_nickname: None,
            agent_role: None,
            git_info: None,
            name: None,
            turns: vec![codex_app_server_protocol::Turn {
                id: String::from("turn-1"),
                items_view: codex_app_server_protocol::TurnItemsView::Full,
                items: vec![ThreadItem::Reasoning {
                    id: String::from("reasoning-1"),
                    summary: vec![String::from("public summary")],
                    content: vec![String::from("raw reasoning content")],
                }],
                status: codex_app_server_protocol::TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            }],
        };

        let rendered = thread_to_transcript_cells(
            thread,
            RawReasoningVisibility::Visible,
            /*codex_home*/ None,
        )
        .into_iter()
        .flat_map(|cell| cell.transcript_lines(/*width*/ 80))
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");

        assert!(rendered.contains("raw reasoning content"));
        assert!(!rendered.contains("public summary"));
    }

    #[tokio::test]
    async fn moving_to_last_card_scrolls_when_cards_exceed_viewport() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );

        let mut items = Vec::new();
        for idx in 0..3 {
            let ts = format!("2025-02-{:02}T00:00:00Z", idx + 1);
            let preview = format!("item-{idx}");
            let path = format!("/tmp/item-{idx}.jsonl");
            items.push(make_row(&path, &ts, &preview));
        }

        state.reset_pagination();
        state.ingest_page(page(
            items, /*next_cursor*/ None, /*num_scanned_files*/ 3,
            /*reached_scan_cap*/ false,
        ));
        state.update_viewport(/*rows*/ 5, /*width*/ 80);

        state
            .handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(state.scroll_top, 1);

        state
            .handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(state.selected, 2);
        assert_eq!(state.scroll_top, 2);
    }

    #[tokio::test]
    async fn up_from_bottom_keeps_viewport_stable_when_card_remains_visible() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );

        let mut items = Vec::new();
        for idx in 0..10 {
            let ts = format!("2025-02-{:02}T00:00:00Z", idx + 1);
            let preview = format!("item-{idx}");
            let path = format!("/tmp/item-{idx}.jsonl");
            items.push(make_row(&path, &ts, &preview));
        }

        state.reset_pagination();
        state.ingest_page(page(
            items, /*next_cursor*/ None, /*num_scanned_files*/ 10,
            /*reached_scan_cap*/ false,
        ));
        state.update_viewport(/*rows*/ 5, /*width*/ 80);

        state.selected = state.filtered_rows.len().saturating_sub(1);
        state.ensure_selected_visible();

        let initial_top = state.scroll_top;
        assert_eq!(initial_top, state.filtered_rows.len().saturating_sub(1));

        state
            .handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(state.scroll_top, initial_top.saturating_sub(1));
        assert_eq!(state.selected, state.filtered_rows.len().saturating_sub(2));
    }

    #[tokio::test]
    async fn up_scrolls_only_after_crossing_top_edge() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );

        let mut items = Vec::new();
        for idx in 0..10 {
            let ts = format!("2025-02-{:02}T00:00:00Z", idx + 1);
            let preview = format!("item-{idx}");
            let path = format!("/tmp/item-{idx}.jsonl");
            items.push(make_row(&path, &ts, &preview));
        }

        state.reset_pagination();
        state.ingest_page(page(
            items, /*next_cursor*/ None, /*num_scanned_files*/ 10,
            /*reached_scan_cap*/ false,
        ));
        state.update_viewport(/*rows*/ 5, /*width*/ 80);
        state.selected = 8;
        state.scroll_top = 8;

        state
            .handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
            .await
            .unwrap();

        assert_eq!(state.selected, 7);
        assert_eq!(state.scroll_top, 7);
    }

    #[test]
    fn list_reports_more_rows_above_and_below() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );

        let mut items = Vec::new();
        for idx in 0..5 {
            let ts = format!("2025-02-{:02}T00:00:00Z", idx + 1);
            let preview = format!("item-{idx}");
            let path = format!("/tmp/item-{idx}.jsonl");
            items.push(make_row(&path, &ts, &preview));
        }

        state.reset_pagination();
        state.ingest_page(page(
            items, /*next_cursor*/ None, /*num_scanned_files*/ 5,
            /*reached_scan_cap*/ false,
        ));
        state.update_viewport(/*rows*/ 5, /*width*/ 80);

        assert!(!state.has_more_above());
        assert!(state.has_more_below(/*viewport_height*/ 5));

        state.scroll_top = 2;

        assert!(state.has_more_above());
        assert!(state.has_more_below(/*viewport_height*/ 5));
    }

    #[tokio::test]
    async fn set_query_loads_until_match_and_respects_scan_cap() {
        let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let request_sink = recorded_requests.clone();
        let loader = page_only_loader(move |req: PageLoadRequest| {
            request_sink.lock().unwrap().push(req);
        });

        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.reset_pagination();
        state.ingest_page(page(
            vec![make_row(
                "/tmp/start.jsonl",
                "2025-01-01T00:00:00Z",
                "alpha",
            )],
            Some("2025-01-02T00:00:00Z"),
            /*num_scanned_files*/ 1,
            /*reached_scan_cap*/ false,
        ));
        recorded_requests.lock().unwrap().clear();

        state.set_query("target".to_string());
        let first_request = {
            let guard = recorded_requests.lock().unwrap();
            assert_eq!(guard.len(), 1);
            guard[0].clone()
        };

        state
            .handle_background_event(BackgroundEvent::Page {
                request_token: first_request.request_token,
                search_token: first_request.search_token,
                page: Ok(page(
                    vec![make_row("/tmp/beta.jsonl", "2025-01-02T00:00:00Z", "beta")],
                    Some("2025-01-03T00:00:00Z"),
                    /*num_scanned_files*/ 5,
                    /*reached_scan_cap*/ false,
                )),
            })
            .await
            .unwrap();

        let second_request = {
            let guard = recorded_requests.lock().unwrap();
            assert_eq!(guard.len(), 2);
            guard[1].clone()
        };
        assert!(state.search_state.is_active());
        assert!(state.filtered_rows.is_empty());

        state
            .handle_background_event(BackgroundEvent::Page {
                request_token: second_request.request_token,
                search_token: second_request.search_token,
                page: Ok(page(
                    vec![make_row(
                        "/tmp/match.jsonl",
                        "2025-01-03T00:00:00Z",
                        "target log",
                    )],
                    Some("2025-01-04T00:00:00Z"),
                    /*num_scanned_files*/ 7,
                    /*reached_scan_cap*/ false,
                )),
            })
            .await
            .unwrap();

        assert!(!state.filtered_rows.is_empty());
        assert!(!state.search_state.is_active());

        recorded_requests.lock().unwrap().clear();
        state.set_query("missing".to_string());
        let active_request = {
            let guard = recorded_requests.lock().unwrap();
            assert_eq!(guard.len(), 1);
            guard[0].clone()
        };

        state
            .handle_background_event(BackgroundEvent::Page {
                request_token: second_request.request_token,
                search_token: second_request.search_token,
                page: Ok(page(
                    Vec::new(),
                    /*next_cursor*/ None,
                    /*num_scanned_files*/ 0,
                    /*reached_scan_cap*/ false,
                )),
            })
            .await
            .unwrap();
        assert_eq!(recorded_requests.lock().unwrap().len(), 1);

        state
            .handle_background_event(BackgroundEvent::Page {
                request_token: active_request.request_token,
                search_token: active_request.search_token,
                page: Ok(page(
                    Vec::new(),
                    /*next_cursor*/ None,
                    /*num_scanned_files*/ 3,
                    /*reached_scan_cap*/ true,
                )),
            })
            .await
            .unwrap();

        assert!(state.filtered_rows.is_empty());
        assert!(!state.search_state.is_active());
        assert!(state.pagination.reached_scan_cap);
    }

    #[tokio::test]
    async fn paste_appends_to_existing_query() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.query = String::from("resize");

        state.handle_paste(String::from("results"));

        assert_eq!(state.query, "resize results");
    }

    #[tokio::test]
    async fn whitespace_only_paste_is_ignored() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.query = String::from("resize");

        state.handle_paste(String::from("  \n\t  "));

        assert_eq!(state.query, "resize");
    }

    #[tokio::test]
    async fn paste_uses_existing_search_loading_path() {
        let recorded_requests: Arc<Mutex<Vec<PageLoadRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let request_sink = recorded_requests.clone();
        let loader = page_only_loader(move |req: PageLoadRequest| {
            request_sink.lock().unwrap().push(req);
        });

        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.reset_pagination();
        state.ingest_page(page(
            vec![make_row(
                "/tmp/start.jsonl",
                "2025-01-01T00:00:00Z",
                "alpha",
            )],
            Some("2025-01-02T00:00:00Z"),
            /*num_scanned_files*/ 1,
            /*reached_scan_cap*/ false,
        ));
        recorded_requests.lock().unwrap().clear();

        state.handle_paste(String::from("target"));

        let guard = recorded_requests.lock().unwrap();
        assert_eq!(state.query, "target");
        assert_eq!(guard.len(), 1);
        assert!(guard[0].search_token.is_some());
    }

    #[tokio::test]
    async fn esc_with_empty_query_starts_fresh() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );

        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .expect("handle key");

        assert!(matches!(selection, Some(SessionSelection::StartFresh)));
    }

    #[tokio::test]
    async fn esc_with_query_clears_search_and_preserves_selected_result() {
        let loader = page_only_loader(|_| {});
        let mut state = PickerState::new(
            FrameRequester::test_dummy(),
            loader,
            ProviderFilter::MatchDefault(String::from("openai")),
            /*show_all*/ true,
            /*filter_cwd*/ None,
            SessionPickerAction::Resume,
        );
        state.reset_pagination();
        state.ingest_page(page(
            vec![
                make_row("/tmp/alpha.jsonl", "2025-01-03T00:00:00Z", "alpha"),
                make_row("/tmp/beta.jsonl", "2025-01-02T00:00:00Z", "beta"),
            ],
            /*next_cursor*/ None,
            /*num_scanned_files*/ 2,
            /*reached_scan_cap*/ false,
        ));
        state.set_query(String::from("beta"));

        let selection = state
            .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .expect("handle key");

        assert!(selection.is_none());
        assert!(state.query.is_empty());
        assert_eq!(state.filtered_rows.len(), 2);
        assert_eq!(
            state.filtered_rows[state.selected].path.as_deref(),
            Some(Path::new("/tmp/beta.jsonl"))
        );
    }
}
