use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{self, Stdout};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{ExecutableCommand, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Sparkline, Table,
    TableState, Wrap,
};
use ratatui::Terminal;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
use tasktui_core::{
    API_VERSION, AdminCommand, AdminResult, ApiRequest, ProcessPriority, ServiceRow, TasktuiError,
};
use tasktui_platform_windows::{
    force_kill_process, get_process_priority, list_tcp_port_owners, list_windows_services,
    hide_console_title_bar, list_visible_top_level_window_pids, open_path_in_explorer,
    request_close_process, restart_process, resume_process, send_request, set_process_priority,
    suspend_process,
};

const REFRESH_EVERY: Duration = Duration::from_secs(1);
const SERVICE_REFRESH_EVERY: Duration = Duration::from_secs(5);
const CPU_WINDOW: usize = 3;
const PERFORMANCE_HISTORY: usize = 60;

fn main() -> Result<()> {
    run_app()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    Processes,
    Performance,
    Services,
    Network,
}

impl ActiveTab {
    fn titles() -> [Line<'static>; 4] {
        [
            Line::from("Processes"),
            Line::from("Performance"),
            Line::from("Services"),
            Line::from("Network"),
        ]
    }

    fn index(self) -> usize {
        match self {
            Self::Processes => 0,
            Self::Performance => 1,
            Self::Services => 2,
            Self::Network => 3,
        }
    }

    fn from_index(index: usize) -> Self {
        match index {
            0 => Self::Processes,
            1 => Self::Performance,
            2 => Self::Services,
            _ => Self::Network,
        }
    }

    fn next(self) -> Self {
        Self::from_index((self.index() + 1) % 4)
    }

    fn previous(self) -> Self {
        Self::from_index((self.index() + 3) % 4)
    }

    fn title(self) -> &'static str {
        match self {
            Self::Processes => "Processes",
            Self::Performance => "Performance",
            Self::Services => "Services",
            Self::Network => "Network",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortMode {
    CpuDesc,
    MemoryDesc,
    PidAsc,
    NameAsc,
}

impl SortMode {
    fn label(self) -> &'static str {
        match self {
            Self::CpuDesc => "CPU",
            Self::MemoryDesc => "Memory",
            Self::PidAsc => "PID",
            Self::NameAsc => "Name",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetworkStateFilter {
    All,
    Listening,
    Established,
}

impl NetworkStateFilter {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Listening,
            Self::Listening => Self::Established,
            Self::Established => Self::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Listening => "Listening",
            Self::Established => "Established",
        }
    }

    fn matches(self, state: &str) -> bool {
        match self {
            Self::All => true,
            Self::Listening => state.eq_ignore_ascii_case("listen"),
            Self::Established => state.eq_ignore_ascii_case("established"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ProcessRow {
    pid: u32,
    parent_pid: Option<u32>,
    depth: usize,
    category: ProcessCategory,
    name: String,
    exe_path: Option<String>,
    priority: String,
    cpu_percent: f32,
    memory_bytes: u64,
    runtime_secs: u64,
}

#[derive(Debug, Clone, PartialEq)]
struct ProcessDetailView {
    pid: u32,
    parent_pid: Option<u32>,
    name: String,
    exe_path: String,
    priority: String,
    cpu_percent: f32,
    memory_bytes: u64,
    runtime_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ProcessCategory {
    App,
    Background,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProcessListEntry {
    Row(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NetworkRow {
    pid: u32,
    process_name: String,
    local_endpoint: String,
    remote_endpoint: String,
    state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Overlay {
    None,
    Search,
    ConfirmForceKill,
    ConfirmServiceAction(ServiceActionIntent),
    ConfirmPriorityChange(ProcessPriority),
    ContextMenu(ContextMenuState),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceActionIntent {
    Start,
    Stop,
    Restart,
}

impl ServiceActionIntent {
    fn title(self) -> &'static str {
        match self {
            Self::Start => "Start Service",
            Self::Stop => "Stop Service",
            Self::Restart => "Restart Service",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContextMenuKind {
    Process,
    Service,
    Network,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneFocus {
    List,
    Detail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ContextMenuTarget {
    ProcessPid(u32),
    ServiceName(String),
    NetworkPid(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContextMenuState {
    kind: ContextMenuKind,
    selected_index: usize,
    target: ContextMenuTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContextAction {
    OpenDetails,
    Close,
    ForceKill,
    Restart,
    Suspend,
    Resume,
    OpenFolder,
    SetPriority(ProcessPriority),
    StartService,
    StopService,
    RestartService,
}

impl ContextAction {
    fn label(self) -> &'static str {
        match self {
            Self::OpenDetails => "Open details",
            Self::Close => "Close",
            Self::ForceKill => "Force kill",
            Self::Restart => "Restart",
            Self::Suspend => "Suspend",
            Self::Resume => "Resume",
            Self::OpenFolder => "Open folder",
            Self::SetPriority(ProcessPriority::Idle) => "Set priority: idle",
            Self::SetPriority(ProcessPriority::BelowNormal) => "Set priority: below_normal",
            Self::SetPriority(ProcessPriority::Normal) => "Set priority: normal",
            Self::SetPriority(ProcessPriority::AboveNormal) => "Set priority: above_normal",
            Self::SetPriority(ProcessPriority::High) => "Set priority: high",
            Self::StartService => "Start service",
            Self::StopService => "Stop service",
            Self::RestartService => "Restart service",
        }
    }
}

const PROCESS_CONTEXT_ACTIONS: [ContextAction; 12] = [
    ContextAction::OpenDetails,
    ContextAction::Close,
    ContextAction::ForceKill,
    ContextAction::Restart,
    ContextAction::Suspend,
    ContextAction::Resume,
    ContextAction::OpenFolder,
    ContextAction::SetPriority(ProcessPriority::Idle),
    ContextAction::SetPriority(ProcessPriority::BelowNormal),
    ContextAction::SetPriority(ProcessPriority::Normal),
    ContextAction::SetPriority(ProcessPriority::AboveNormal),
    ContextAction::SetPriority(ProcessPriority::High),
];

const SERVICE_CONTEXT_ACTIONS: [ContextAction; 4] = [
    ContextAction::OpenDetails,
    ContextAction::StartService,
    ContextAction::StopService,
    ContextAction::RestartService,
];

const NETWORK_CONTEXT_ACTIONS: [ContextAction; 12] = [
    ContextAction::OpenDetails,
    ContextAction::Close,
    ContextAction::ForceKill,
    ContextAction::Restart,
    ContextAction::Suspend,
    ContextAction::Resume,
    ContextAction::OpenFolder,
    ContextAction::SetPriority(ProcessPriority::Idle),
    ContextAction::SetPriority(ProcessPriority::BelowNormal),
    ContextAction::SetPriority(ProcessPriority::Normal),
    ContextAction::SetPriority(ProcessPriority::AboveNormal),
    ContextAction::SetPriority(ProcessPriority::High),
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ViewState {
    selected: usize,
    offset: usize,
    visible_rows: usize,
}

impl ViewState {
    fn clamp(&mut self, len: usize) {
        self.selected = self.selected.min(len.saturating_sub(1));
        self.offset = adjusted_offset(len, self.selected, self.offset, self.visible_rows.max(1));
    }

    fn select_next(&mut self, len: usize) {
        if len == 0 {
            self.selected = 0;
            self.offset = 0;
            return;
        }
        self.selected = (self.selected + 1).min(len - 1);
        self.offset = adjusted_offset(len, self.selected, self.offset, self.visible_rows.max(1));
    }

    fn select_prev(&mut self, len: usize) {
        if len == 0 {
            self.selected = 0;
            self.offset = 0;
            return;
        }
        self.selected = self.selected.saturating_sub(1);
        self.offset = adjusted_offset(len, self.selected, self.offset, self.visible_rows.max(1));
    }
}

#[derive(Debug, Clone, Default)]
struct SystemSummary {
    cpu_percent: f32,
    used_memory: u64,
    total_memory: u64,
    process_count: usize,
    tcp_endpoint_count: usize,
}

#[derive(Debug, Clone, Default)]
struct GroupedProcessViewState {
    apps_offset: usize,
    background_offset: usize,
    apps_visible_rows: usize,
    background_visible_rows: usize,
}

struct AppState {
    system: System,
    process_rows: Vec<ProcessRow>,
    filtered_rows: Vec<ProcessRow>,
    service_rows: Vec<ServiceRow>,
    filtered_services: Vec<ServiceRow>,
    network_rows: Vec<NetworkRow>,
    filtered_network: Vec<NetworkRow>,
    cpu_history: HashMap<u32, VecDeque<f32>>,
    system_cpu_history: VecDeque<u64>,
    system_memory_history: VecDeque<u64>,
    summary: SystemSummary,
    active_tab: ActiveTab,
    sort_mode: SortMode,
    process_sort_enabled: bool,
    query: String,
    show_tree: bool,
    pane_focus: PaneFocus,
    network_filter: NetworkStateFilter,
    overlay: Overlay,
    process_view: ViewState,
    grouped_process_view: GroupedProcessViewState,
    service_view: ViewState,
    network_view: ViewState,
    priority_cache: HashMap<u32, String>,
    process_stable_order: HashMap<u32, usize>,
    next_process_order: usize,
    last_refresh: Instant,
    last_service_refresh: Instant,
    last_priority_refresh: Instant,
    process_detail_scroll: u16,
    service_detail_scroll: u16,
    network_detail_scroll: u16,
    feedback: String,
}

impl AppState {
    fn new() -> Self {
        Self {
            system: System::new_with_specifics(
                RefreshKind::nothing().with_processes(ProcessRefreshKind::everything()),
            ),
            process_rows: Vec::new(),
            filtered_rows: Vec::new(),
            service_rows: Vec::new(),
            filtered_services: Vec::new(),
            network_rows: Vec::new(),
            filtered_network: Vec::new(),
            cpu_history: HashMap::new(),
            system_cpu_history: VecDeque::new(),
            system_memory_history: VecDeque::new(),
            summary: SystemSummary::default(),
            active_tab: ActiveTab::Processes,
            sort_mode: SortMode::CpuDesc,
            process_sort_enabled: false,
            query: String::new(),
            show_tree: false,
            pane_focus: PaneFocus::List,
            network_filter: NetworkStateFilter::All,
            overlay: Overlay::None,
            process_view: ViewState {
                visible_rows: 1,
                ..ViewState::default()
            },
            grouped_process_view: GroupedProcessViewState {
                apps_visible_rows: 1,
                background_visible_rows: 1,
                ..GroupedProcessViewState::default()
            },
            service_view: ViewState {
                visible_rows: 1,
                ..ViewState::default()
            },
            network_view: ViewState {
                visible_rows: 1,
                ..ViewState::default()
            },
            priority_cache: HashMap::new(),
            process_stable_order: HashMap::new(),
            next_process_order: 0,
            last_refresh: Instant::now() - REFRESH_EVERY,
            last_service_refresh: Instant::now() - SERVICE_REFRESH_EVERY,
            last_priority_refresh: Instant::now() - Duration::from_secs(5),
            process_detail_scroll: 0,
            service_detail_scroll: 0,
            network_detail_scroll: 0,
            feedback: "Tab/Shift+Tab to switch pages. / to search.".into(),
        }
    }

    fn refresh_processes(&mut self) {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::everything(),
        );
        let app_pids: std::collections::HashSet<u32> = list_visible_top_level_window_pids()
            .unwrap_or_default()
            .into_iter()
            .collect();

        let mut live_pids = Vec::new();
        let rows: Vec<ProcessRow> = self
            .system
            .processes()
            .values()
            .map(|process| {
                let pid = process.pid().as_u32();
                live_pids.push(pid);
                let history = self.cpu_history.entry(pid).or_default();
                history.push_back(process.cpu_usage());
                while history.len() > CPU_WINDOW {
                    history.pop_front();
                }
                let average_cpu = history.iter().copied().sum::<f32>() / history.len() as f32;
                ProcessRow {
                    pid,
                    parent_pid: process.parent().map(Pid::as_u32),
                    depth: 0,
                    category: if app_pids.contains(&pid) {
                        ProcessCategory::App
                    } else {
                        ProcessCategory::Background
                    },
                    name: process.name().to_string_lossy().to_string(),
                    exe_path: process.exe().map(|path| path.display().to_string()),
                    priority: self
                        .priority_cache
                        .get(&pid)
                        .cloned()
                        .unwrap_or_else(|| "unknown".into()),
                    cpu_percent: average_cpu,
                    memory_bytes: process.memory(),
                    runtime_secs: process.run_time(),
                }
            })
            .collect();
        self.cpu_history.retain(|pid, _| live_pids.contains(pid));
        self.priority_cache.retain(|pid, _| live_pids.contains(pid));
        self.process_stable_order.retain(|pid, _| live_pids.contains(pid));
        for pid in &live_pids {
            self.process_stable_order.entry(*pid).or_insert_with(|| {
                let order = self.next_process_order;
                self.next_process_order += 1;
                order
            });
        }
        self.process_rows = rows;
        self.rebuild_process_view();
        self.refresh_network_rows();
        self.summary = SystemSummary {
            cpu_percent: self.system.global_cpu_usage(),
            used_memory: self.system.used_memory(),
            total_memory: self.system.total_memory(),
            process_count: self.process_rows.len(),
            tcp_endpoint_count: self.network_rows.len(),
        };
        push_sample(
            &mut self.system_cpu_history,
            self.summary.cpu_percent.round().clamp(0.0, 100.0) as u64,
            PERFORMANCE_HISTORY,
        );
        let memory_percent = if self.summary.total_memory == 0 {
            0
        } else {
            ((self.summary.used_memory as f64 / self.summary.total_memory as f64) * 100.0)
                .round()
                .clamp(0.0, 100.0) as u64
        };
        push_sample(
            &mut self.system_memory_history,
            memory_percent,
            PERFORMANCE_HISTORY,
        );
        self.refresh_visible_priorities(false);
        self.last_refresh = Instant::now();
    }

    fn refresh_services(&mut self, force: bool) {
        if !force && self.last_service_refresh.elapsed() < SERVICE_REFRESH_EVERY {
            return;
        }
        match list_windows_services() {
            Ok(rows) => {
                self.service_rows = rows;
                self.rebuild_service_view();
            }
            Err(error) => {
                self.feedback = format!("Failed to enumerate services: {error}");
            }
        }
        self.last_service_refresh = Instant::now();
    }

    fn refresh_network_rows(&mut self) {
        let process_names: HashMap<u32, String> = self
            .process_rows
            .iter()
            .map(|row| (row.pid, row.name.clone()))
            .collect();
        self.network_rows = list_tcp_port_owners()
            .unwrap_or_default()
            .into_iter()
            .map(|row| NetworkRow {
                pid: row.pid,
                process_name: process_names
                    .get(&row.pid)
                    .cloned()
                    .unwrap_or_else(|| "<unknown>".into()),
                local_endpoint: format!("{}:{}", row.local_addr, row.local_port),
                remote_endpoint: format!("{}:{}", row.remote_addr, row.remote_port),
                state: row.state,
            })
            .collect();
        self.rebuild_network_view();
    }

    fn rebuild_process_view(&mut self) {
        let selected_pid = self.filtered_rows.get(self.process_view.selected).map(|row| row.pid);
        let mut rows = self.process_rows.clone();
        if self.process_sort_enabled {
            self.apply_process_sort(&mut rows);
        } else {
            rows.sort_by(|left, right| {
                left.category
                    .cmp(&right.category)
                    .then_with(|| {
                        self.process_stable_order
                            .get(&left.pid)
                            .copied()
                            .unwrap_or(usize::MAX)
                            .cmp(
                                &self
                                    .process_stable_order
                                    .get(&right.pid)
                                    .copied()
                                    .unwrap_or(usize::MAX),
                            )
                    })
                    .then_with(|| left.pid.cmp(&right.pid))
            });
        }
        if self.show_tree {
            rows = flatten_process_tree(&rows);
        }
        self.filtered_rows = self.apply_process_filter(rows);
        if let Some(pid) = selected_pid
            && let Some(index) = self.filtered_rows.iter().position(|row| row.pid == pid)
        {
            self.process_view.selected = index;
        } else {
            self.process_view.selected = self
                .process_view
                .selected
                .min(self.filtered_rows.len().saturating_sub(1));
        }
        self.sync_process_view_state();
    }

    fn rebuild_service_view(&mut self) {
        self.filtered_services = self.apply_service_filter(self.service_rows.clone());
        self.service_view.clamp(self.filtered_services.len());
    }

    fn rebuild_network_view(&mut self) {
        self.filtered_network = self.apply_network_filter(self.network_rows.clone());
        self.network_view.clamp(self.filtered_network.len());
    }

    fn apply_process_filter(&self, rows: Vec<ProcessRow>) -> Vec<ProcessRow> {
        let query = self.query.trim().to_ascii_lowercase();
        if query.is_empty() {
            return rows;
        }
        rows.into_iter()
            .filter(|row| {
                row.name.to_ascii_lowercase().contains(&query)
                    || row.pid.to_string().contains(&query)
                    || row.priority.to_ascii_lowercase().contains(&query)
                    || row
                        .exe_path
                        .as_deref()
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                        .contains(&query)
            })
            .collect()
    }

    fn apply_service_filter(&self, rows: Vec<ServiceRow>) -> Vec<ServiceRow> {
        let query = self.query.trim().to_ascii_lowercase();
        if query.is_empty() {
            return rows;
        }
        rows.into_iter()
            .filter(|row| {
                row.display_name.to_ascii_lowercase().contains(&query)
                    || row.service_name.to_ascii_lowercase().contains(&query)
                    || row.status.to_ascii_lowercase().contains(&query)
                    || row.start_type.to_ascii_lowercase().contains(&query)
            })
            .collect()
    }

    fn apply_network_filter(&self, rows: Vec<NetworkRow>) -> Vec<NetworkRow> {
        let query = self.query.trim().to_ascii_lowercase();
        rows.into_iter()
            .filter(|row| self.network_filter.matches(&row.state))
            .filter(|row| {
                if query.is_empty() {
                    true
                } else {
                    row.pid.to_string().contains(&query)
                        || row.process_name.to_ascii_lowercase().contains(&query)
                        || row.local_endpoint.to_ascii_lowercase().contains(&query)
                        || row.remote_endpoint.to_ascii_lowercase().contains(&query)
                        || row.state.to_ascii_lowercase().contains(&query)
                }
            })
            .collect()
    }

    fn apply_process_sort(&self, rows: &mut [ProcessRow]) {
        rows.sort_by(|left, right| match self.sort_mode {
            SortMode::CpuDesc => right
                .cpu_percent
                .partial_cmp(&left.cpu_percent)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.pid.cmp(&right.pid)),
            SortMode::MemoryDesc => right
                .memory_bytes
                .cmp(&left.memory_bytes)
                .then_with(|| left.pid.cmp(&right.pid)),
            SortMode::PidAsc => left.pid.cmp(&right.pid),
            SortMode::NameAsc => left
                .name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
                .then_with(|| left.pid.cmp(&right.pid)),
        });
    }

    fn select_next(&mut self) {
        match self.active_tab {
            ActiveTab::Processes => {
                let entries = self.process_entries();
                self.process_view.selected =
                    next_process_index(&entries, self.process_view.selected).unwrap_or(self.process_view.selected);
                self.sync_process_view_state();
            }
            ActiveTab::Services => self.service_view.select_next(self.filtered_services.len()),
            ActiveTab::Network => self.network_view.select_next(self.filtered_network.len()),
            ActiveTab::Performance => {}
        }
        if matches!(self.active_tab, ActiveTab::Processes) {
            self.refresh_visible_priorities(true);
        }
    }

    fn select_prev(&mut self) {
        match self.active_tab {
            ActiveTab::Processes => {
                let entries = self.process_entries();
                self.process_view.selected =
                    prev_process_index(&entries, self.process_view.selected).unwrap_or(self.process_view.selected);
                self.sync_process_view_state();
            }
            ActiveTab::Services => self.service_view.select_prev(self.filtered_services.len()),
            ActiveTab::Network => self.network_view.select_prev(self.filtered_network.len()),
            ActiveTab::Performance => {}
        }
        if matches!(self.active_tab, ActiveTab::Processes) {
            self.refresh_visible_priorities(true);
        }
    }

    fn selected_process_detail(&self) -> Option<ProcessDetailView> {
        self.filtered_rows
            .get(self.process_view.selected)
            .map(|row| ProcessDetailView {
                pid: row.pid,
                parent_pid: row.parent_pid,
                name: row.name.clone(),
                exe_path: row.exe_path.clone().unwrap_or_else(|| "-".into()),
                priority: row.priority.clone(),
                cpu_percent: row.cpu_percent,
                memory_bytes: row.memory_bytes,
                runtime_secs: row.runtime_secs,
            })
    }

    fn selected_service(&self) -> Option<&ServiceRow> {
        self.filtered_services.get(self.service_view.selected)
    }

    fn process_entries(&self) -> Vec<ProcessListEntry> {
        if self.process_sort_enabled {
            self.filtered_rows
                .iter()
                .enumerate()
                .map(|(index, _)| ProcessListEntry::Row(index))
                .collect()
        } else {
            let (apps, background) = grouped_process_indexes(&self.filtered_rows);
            apps.into_iter()
                .chain(background)
                .map(ProcessListEntry::Row)
                .collect()
        }
    }

    fn grouped_process_indexes(&self) -> (Vec<usize>, Vec<usize>) {
        grouped_process_indexes(&self.filtered_rows)
    }

    fn sync_process_view_state(&mut self) {
        if self.process_sort_enabled {
            let entries = self.process_entries();
            self.process_view.offset = adjusted_process_offset(
                &entries,
                self.process_view.selected,
                self.process_view.offset,
                self.process_view.visible_rows.max(1),
            );
            return;
        }

        let (apps, background) = self.grouped_process_indexes();
        self.grouped_process_view.apps_offset = self
            .grouped_process_view
            .apps_offset
            .min(apps.len().saturating_sub(self.grouped_process_view.apps_visible_rows.max(1)));
        self.grouped_process_view.background_offset = self
            .grouped_process_view
            .background_offset
            .min(background.len().saturating_sub(self.grouped_process_view.background_visible_rows.max(1)));

        if let Some(row) = self.filtered_rows.get(self.process_view.selected) {
            match row.category {
                ProcessCategory::App => {
                    if let Some(index) = apps.iter().position(|entry| *entry == self.process_view.selected) {
                        self.grouped_process_view.apps_offset = adjusted_offset(
                            apps.len(),
                            index,
                            self.grouped_process_view.apps_offset,
                            self.grouped_process_view.apps_visible_rows.max(1),
                        );
                    }
                }
                ProcessCategory::Background => {
                    if let Some(index) = background.iter().position(|entry| *entry == self.process_view.selected) {
                        self.grouped_process_view.background_offset = adjusted_offset(
                            background.len(),
                            index,
                            self.grouped_process_view.background_offset,
                            self.grouped_process_view.background_visible_rows.max(1),
                        );
                    }
                }
            }
        }
    }

    fn selected_network(&self) -> Option<&NetworkRow> {
        self.filtered_network.get(self.network_view.selected)
    }

    fn selected_target_pid(&self) -> Option<u32> {
        match self.active_tab {
            ActiveTab::Processes => self.filtered_rows.get(self.process_view.selected).map(|row| row.pid),
            ActiveTab::Network => self.selected_network().map(|row| row.pid),
            ActiveTab::Performance | ActiveTab::Services => None,
        }
    }

    fn selected_service_name(&self) -> Option<String> {
        self.selected_service().map(|row| row.service_name.clone())
    }

    fn set_active_tab(&mut self, tab: ActiveTab) {
        self.active_tab = tab;
        self.overlay = Overlay::None;
        self.pane_focus = PaneFocus::List;
        if matches!(tab, ActiveTab::Services) {
            self.refresh_services(false);
        }
    }

    fn cycle_network_filter(&mut self) {
        self.network_filter = self.network_filter.next();
        self.rebuild_network_view();
    }

    fn update_view_metrics(&mut self, area: Rect) {
        let layout = root_sections(area);
        let body = layout[2];
        let body_sections = body_sections(body);
        self.process_view.visible_rows = if self.show_tree || !self.process_sort_enabled {
            body_sections[0].height.saturating_sub(2) as usize
        } else {
            body_sections[0].height.saturating_sub(3) as usize
        }
        .max(1);
        if !self.process_sort_enabled {
            let (_, apps_list_area, _, background_list_area) = grouped_process_sections(body_sections[0]);
            self.grouped_process_view.apps_visible_rows = apps_list_area.height as usize;
            self.grouped_process_view.background_visible_rows = background_list_area.height as usize;
        }
        self.service_view.visible_rows = (body_sections[0].height.saturating_sub(3) as usize).max(1);
        let network_sections = network_list_sections(body_sections[0]);
        self.network_view.visible_rows = (network_sections[1].height.saturating_sub(3) as usize).max(1);
        self.process_view.selected = self
            .process_view
            .selected
            .min(self.filtered_rows.len().saturating_sub(1));
        self.sync_process_view_state();
        self.service_view.clamp(self.filtered_services.len());
        self.network_view.clamp(self.filtered_network.len());
    }

    fn current_context_actions(&self) -> &'static [ContextAction] {
        match self.overlay {
            Overlay::ContextMenu(ContextMenuState {
                kind: ContextMenuKind::Process,
                ..
            }) => &PROCESS_CONTEXT_ACTIONS,
            Overlay::ContextMenu(ContextMenuState {
                kind: ContextMenuKind::Service,
                ..
            }) => &SERVICE_CONTEXT_ACTIONS,
            Overlay::ContextMenu(ContextMenuState {
                kind: ContextMenuKind::Network,
                ..
            }) => &NETWORK_CONTEXT_ACTIONS,
            _ => &[],
        }
    }

    fn visible_process_pids(&self) -> Vec<u32> {
        if !self.process_sort_enabled {
            let (apps, background) = self.grouped_process_indexes();
            return apps
                .iter()
                .skip(self.grouped_process_view.apps_offset)
                .take(self.grouped_process_view.apps_visible_rows.max(1))
                .chain(
                    background
                        .iter()
                        .skip(self.grouped_process_view.background_offset)
                        .take(self.grouped_process_view.background_visible_rows.max(1)),
                )
                .filter_map(|index| self.filtered_rows.get(*index).map(|row| row.pid))
                .collect();
        }
        let entries = self.process_entries();
        let selected_display = selected_display_index(&entries, self.process_view.selected).unwrap_or(0);
        let (offset, _) = visible_window(
            entries.len(),
            selected_display,
            self.process_view.offset,
            self.process_view.visible_rows.max(1),
        );
        entries
            .iter()
            .skip(offset)
            .take(self.process_view.visible_rows.max(1))
            .filter_map(|entry| match entry {
                ProcessListEntry::Row(index) => self.filtered_rows.get(*index).map(|row| row.pid),
            })
            .collect()
    }

    fn refresh_visible_priorities(&mut self, force_selected: bool) {
        if !force_selected && self.last_priority_refresh.elapsed() < Duration::from_secs(5) {
            return;
        }

        let mut targets = self.visible_process_pids();
        if let Some(detail) = self.selected_process_detail()
            && !targets.contains(&detail.pid)
        {
            targets.push(detail.pid);
        }
        if targets.is_empty() {
            return;
        }

        let mut changed = false;
        for pid in targets {
            if let Ok(priority) = get_process_priority(pid) {
                let priority = priority.to_string();
                let entry = self.priority_cache.entry(pid).or_default();
                if *entry != priority {
                    *entry = priority;
                    changed = true;
                }
            }
        }

        if changed {
            for row in &mut self.process_rows {
                row.priority = self
                    .priority_cache
                    .get(&row.pid)
                    .cloned()
                    .unwrap_or_else(|| "unknown".into());
            }
            self.rebuild_process_view();
        }

        self.last_priority_refresh = Instant::now();
    }

    fn cycle_process_sort_mode(&mut self) {
        if !self.process_sort_enabled {
            self.process_sort_enabled = true;
            self.sort_mode = SortMode::CpuDesc;
        } else {
            match self.sort_mode {
                SortMode::CpuDesc => self.sort_mode = SortMode::MemoryDesc,
                SortMode::MemoryDesc => self.sort_mode = SortMode::PidAsc,
                SortMode::PidAsc => self.sort_mode = SortMode::NameAsc,
                SortMode::NameAsc => self.process_sort_enabled = false,
            }
        }
        self.rebuild_process_view();
    }
}

pub fn run_app() -> Result<()> {
    let _ = hide_console_title_bar();
    enable_raw_mode().context("enable raw mode")?;
    io::stdout().execute(terminal::EnterAlternateScreen)?;
    io::stdout().execute(EnableMouseCapture)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    let result = run_event_loop(&mut terminal);
    disable_raw_mode().ok();
    io::stdout().execute(DisableMouseCapture).ok();
    io::stdout().execute(terminal::LeaveAlternateScreen).ok();
    result
}

fn run_event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    let mut app = AppState::new();
    app.refresh_processes();
    app.refresh_services(true);
    app.feedback =
        send_admin_command(AdminCommand::Ping).unwrap_or_else(|error| format!("Service unavailable: {error}"));

    loop {
        if app.last_refresh.elapsed() >= REFRESH_EVERY {
            app.refresh_processes();
            if matches!(app.active_tab, ActiveTab::Services) {
                app.refresh_services(false);
            }
        }

        let size = terminal.size()?;
        app.update_view_metrics(Rect::new(0, 0, size.width, size.height));
        terminal.draw(|frame| render_root(frame, &app))?;

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if handle_key_event(&mut app, key)? {
                        break;
                    }
                }
                Event::Mouse(mouse) => {
                    handle_mouse_event(&mut app, mouse, Rect::new(0, 0, size.width, size.height))?
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn handle_key_event(app: &mut AppState, key: KeyEvent) -> Result<bool> {
    match app.overlay {
        Overlay::Search => return handle_search_key(app, key),
        Overlay::ConfirmForceKill => return handle_force_kill_key(app, key),
        Overlay::ConfirmServiceAction(intent) => {
            return handle_service_confirmation_key(app, key, intent)
        }
        Overlay::ConfirmPriorityChange(priority) => {
            return handle_priority_confirmation_key(app, key, priority)
        }
        Overlay::ContextMenu(_) => return handle_context_menu_key(app, key),
        Overlay::None => {}
    }

    match key.code {
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Tab | KeyCode::Right => app.set_active_tab(app.active_tab.next()),
        KeyCode::BackTab | KeyCode::Left => app.set_active_tab(app.active_tab.previous()),
        KeyCode::F(10) if key.modifiers.contains(KeyModifiers::SHIFT) => {
            open_context_menu_for_selection(app);
        }
        KeyCode::Char('/') => app.overlay = Overlay::Search,
        KeyCode::Down => match app.pane_focus {
            PaneFocus::List => app.select_next(),
            PaneFocus::Detail => increment_detail_scroll(app),
        },
        KeyCode::Up => match app.pane_focus {
            PaneFocus::List => app.select_prev(),
            PaneFocus::Detail => decrement_detail_scroll(app),
        },
        KeyCode::Enter => {
            if !matches!(app.active_tab, ActiveTab::Performance) {
                app.pane_focus = match app.pane_focus {
                    PaneFocus::List => PaneFocus::Detail,
                    PaneFocus::Detail => PaneFocus::List,
                };
            }
            app.feedback = if matches!(app.pane_focus, PaneFocus::Detail) {
                format!("{} detail pane focused.", app.active_tab.title())
            } else {
                format!("{} list focused.", app.active_tab.title())
            };
        }
        KeyCode::Char('s') if matches!(app.active_tab, ActiveTab::Processes) => {
            app.cycle_process_sort_mode();
        }
        KeyCode::Char('t') if matches!(app.active_tab, ActiveTab::Processes) => {
            app.show_tree = !app.show_tree;
            app.rebuild_process_view();
        }
        KeyCode::Char('f') if matches!(app.active_tab, ActiveTab::Network) => {
            app.cycle_network_filter();
        }
        KeyCode::Char('o') => {
            app.feedback = match app.active_tab {
                ActiveTab::Processes => open_selected_process_path(app),
                ActiveTab::Network => open_selected_network_path(app),
                _ => "Open folder is available on Processes and Network.".into(),
            };
        }
        KeyCode::Char('k') => {
            if let Some(pid) = app.selected_target_pid() {
                app.feedback = send_admin_command(AdminCommand::RequestCloseProcess { pid })?;
            }
        }
        KeyCode::Char('r') => {
            if let Some(pid) = app.selected_target_pid() {
                app.feedback = send_admin_command(AdminCommand::RestartProcess { pid })?;
                app.refresh_processes();
            }
        }
        KeyCode::Char('K') => {
            if app.selected_target_pid().is_some() {
                app.overlay = Overlay::ConfirmForceKill;
            }
        }
        KeyCode::Char('z') => {
            if let Some(pid) = app.selected_target_pid() {
                app.feedback = send_admin_command(AdminCommand::SuspendProcess { pid })?;
                app.refresh_processes();
            }
        }
        KeyCode::Char('x') => {
            if let Some(pid) = app.selected_target_pid() {
                app.feedback = send_admin_command(AdminCommand::ResumeProcess { pid })?;
                app.refresh_processes();
            }
        }
        KeyCode::Char('4') => app.overlay = Overlay::ConfirmPriorityChange(ProcessPriority::Idle),
        KeyCode::Char('5') => app.overlay = Overlay::ConfirmPriorityChange(ProcessPriority::BelowNormal),
        KeyCode::Char('6') => app.overlay = Overlay::ConfirmPriorityChange(ProcessPriority::Normal),
        KeyCode::Char('7') => app.overlay = Overlay::ConfirmPriorityChange(ProcessPriority::AboveNormal),
        KeyCode::Char('8') => app.overlay = Overlay::ConfirmPriorityChange(ProcessPriority::High),
        KeyCode::Char('1') if matches!(app.active_tab, ActiveTab::Services) && app.selected_service().is_some() => {
            app.overlay = Overlay::ConfirmServiceAction(ServiceActionIntent::Start);
        }
        KeyCode::Char('2') if matches!(app.active_tab, ActiveTab::Services) && app.selected_service().is_some() => {
            app.overlay = Overlay::ConfirmServiceAction(ServiceActionIntent::Stop);
        }
        KeyCode::Char('3') if matches!(app.active_tab, ActiveTab::Services) && app.selected_service().is_some() => {
            app.overlay = Overlay::ConfirmServiceAction(ServiceActionIntent::Restart);
        }
        _ => {}
    }
    Ok(false)
}

fn open_context_menu_for_selection(app: &mut AppState) {
    app.pane_focus = PaneFocus::List;
    app.overlay = match app.active_tab {
        ActiveTab::Processes => app.selected_target_pid().map(|pid| {
            Overlay::ContextMenu(ContextMenuState {
                kind: ContextMenuKind::Process,
                selected_index: 0,
                target: ContextMenuTarget::ProcessPid(pid),
            })
        }),
        ActiveTab::Services => app.selected_service_name().map(|service_name| {
            Overlay::ContextMenu(ContextMenuState {
                kind: ContextMenuKind::Service,
                selected_index: 0,
                target: ContextMenuTarget::ServiceName(service_name),
            })
        }),
        ActiveTab::Network => app.selected_target_pid().map(|pid| {
            Overlay::ContextMenu(ContextMenuState {
                kind: ContextMenuKind::Network,
                selected_index: 0,
                target: ContextMenuTarget::NetworkPid(pid),
            })
        }),
        ActiveTab::Performance => None,
    }
    .unwrap_or(Overlay::None);
}

fn handle_search_key(app: &mut AppState, key: KeyEvent) -> Result<bool> {
    match key.code {
        KeyCode::Esc => app.overlay = Overlay::None,
        KeyCode::Enter => app.overlay = Overlay::None,
        KeyCode::Backspace => {
            app.query.pop();
            rebuild_active_tab(app);
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.query.push(c);
            rebuild_active_tab(app);
        }
        _ => {}
    }
    Ok(false)
}

fn handle_force_kill_key(app: &mut AppState, key: KeyEvent) -> Result<bool> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => app.overlay = Overlay::None,
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
            if let Some(pid) = app.selected_target_pid() {
                app.feedback = send_admin_command(AdminCommand::ForceKillProcess { pid })?;
                app.refresh_processes();
            }
            app.overlay = Overlay::None;
        }
        _ => {}
    }
    Ok(false)
}

fn handle_service_confirmation_key(
    app: &mut AppState,
    key: KeyEvent,
    intent: ServiceActionIntent,
) -> Result<bool> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => app.overlay = Overlay::None,
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
            if let Some(service_name) = app.selected_service_name() {
                let command = match intent {
                    ServiceActionIntent::Start => AdminCommand::StartService { service_name },
                    ServiceActionIntent::Stop => AdminCommand::StopService { service_name },
                    ServiceActionIntent::Restart => AdminCommand::RestartService {
                        service_name,
                        timeout_ms: 30_000,
                    },
                };
                app.feedback = send_admin_command(command)?;
                app.refresh_services(true);
            }
            app.overlay = Overlay::None;
        }
        _ => {}
    }
    Ok(false)
}

fn handle_priority_confirmation_key(
    app: &mut AppState,
    key: KeyEvent,
    priority: ProcessPriority,
) -> Result<bool> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => app.overlay = Overlay::None,
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
            app.feedback = set_selected_priority(app, priority)?;
            app.overlay = Overlay::None;
            app.refresh_processes();
        }
        _ => {}
    }
    Ok(false)
}

fn handle_context_menu_key(app: &mut AppState, key: KeyEvent) -> Result<bool> {
    let actions = app.current_context_actions();
    if actions.is_empty() {
        app.overlay = Overlay::None;
        return Ok(false);
    }

    let mut selected_index = match &app.overlay {
        Overlay::ContextMenu(state) => state.selected_index,
        _ => 0,
    };

    match key.code {
        KeyCode::Esc => app.overlay = Overlay::None,
        KeyCode::Down => {
            selected_index = (selected_index + 1).min(actions.len() - 1);
            if let Overlay::ContextMenu(state) = &mut app.overlay {
                state.selected_index = selected_index;
            }
        }
        KeyCode::Up => {
            selected_index = selected_index.saturating_sub(1);
            if let Overlay::ContextMenu(state) = &mut app.overlay {
                state.selected_index = selected_index;
            }
        }
        KeyCode::Enter => {
            let action = actions[selected_index];
            let overlay = app.overlay.clone();
            app.overlay = Overlay::None;
            if let Overlay::ContextMenu(state) = overlay {
                perform_context_action(app, &state, action)?;
            }
        }
        _ => {}
    }
    Ok(false)
}

fn handle_mouse_event(app: &mut AppState, mouse: MouseEvent, frame_area: Rect) -> Result<()> {
    let layout = root_sections(frame_area);
    let summary_area = layout[0];
    let tabs_area = layout[1];
    let body = layout[2];
    let footer_area = layout[3];
    let body_sections = body_sections(body);

    if let Overlay::ContextMenu(state) = app.overlay.clone() {
        let menu_area = context_menu_area(frame_area, app.current_context_actions().len());
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            if contains_point(menu_area, mouse.column, mouse.row) {
                if let Some(index) = list_index_at_row(menu_area, mouse.row, false) {
                    let actions = app.current_context_actions();
                    if index < actions.len() {
                        let action = actions[index];
                        app.overlay = Overlay::None;
                        perform_context_action(app, &state, action)?;
                    }
                }
            }
            else {
                app.overlay = Overlay::None;
            }
        }
        return Ok(());
    }

    if !matches!(app.overlay, Overlay::None) {
        return Ok(());
    }

    if contains_point(tabs_area, mouse.column, mouse.row)
        && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        && let Some(tab) = tab_at_position(tabs_area, mouse.column)
    {
        app.set_active_tab(tab);
        return Ok(());
    }

    if contains_point(summary_area, mouse.column, mouse.row)
        || contains_point(footer_area, mouse.column, mouse.row)
    {
        return Ok(());
    }

    let list_area = body_sections[0];
    let detail_area = body_sections[1];
    if matches!(app.active_tab, ActiveTab::Network) {
        let network_sections = network_list_sections(list_area);
        if contains_point(network_sections[0], mouse.column, mouse.row)
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some(filter) = network_filter_at_position(network_sections[0], mouse.column)
        {
            app.network_filter = filter;
            app.rebuild_network_view();
            return Ok(());
        }
    }

    if contains_point(list_area, mouse.column, mouse.row) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                select_row_at(app, list_area, mouse.row);
                app.pane_focus = PaneFocus::List;
            }
            MouseEventKind::Down(MouseButton::Right) => {
                select_row_at(app, list_area, mouse.row);
                open_context_menu_for_selection(app);
            }
            MouseEventKind::ScrollDown => app.select_next(),
            MouseEventKind::ScrollUp => app.select_prev(),
            _ => {}
        }
    } else if contains_point(detail_area, mouse.column, mouse.row)
        && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
    {
        app.pane_focus = PaneFocus::Detail;
        app.feedback = format!("{} detail pane focused.", app.active_tab.title());
    }

    Ok(())
}

fn render_root(frame: &mut ratatui::Frame<'_>, app: &AppState) {
    let layout = root_sections(frame.area());
    render_summary_bar(frame, layout[0], app);
    render_tabs(frame, layout[1], app);

    let body_sections = body_sections(layout[2]);
    match app.active_tab {
        ActiveTab::Processes => {
            if app.show_tree {
                render_process_tree(frame, body_sections[0], app);
            } else {
                render_process_table(frame, body_sections[0], app);
            }
            render_process_detail_pane(frame, body_sections[1], app);
        }
        ActiveTab::Performance => render_performance_view(frame, layout[2], app),
        ActiveTab::Services => {
            render_service_table(frame, body_sections[0], app);
            render_service_detail_pane(frame, body_sections[1], app);
        }
        ActiveTab::Network => {
            let network_sections = network_list_sections(body_sections[0]);
            render_network_filter_bar(frame, network_sections[0], app);
            render_network_table(frame, network_sections[1], app);
            render_network_detail_pane(frame, body_sections[1], app);
        }
    }

    render_footer(frame, layout[3], app);

    match app.overlay {
        Overlay::Search => render_input_modal(frame, centered_rect(frame.area(), 55, 20), "Search", &app.query),
        Overlay::ConfirmForceKill => render_force_kill_modal(frame, app),
        Overlay::ConfirmServiceAction(intent) => render_service_confirmation_modal(frame, app, intent),
        Overlay::ConfirmPriorityChange(priority) => render_priority_confirmation_modal(frame, app, priority),
        Overlay::ContextMenu(_) => render_context_menu(frame, app),
        Overlay::None => {}
    }
}

fn render_summary_bar(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState) {
    let sections = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(area);

    let cards = [
        (
            "CPU",
            format!("{:.1}%", app.summary.cpu_percent),
            cpu_style(app.summary.cpu_percent),
        ),
        (
            "Memory",
            format!(
                "{} / {}",
                format_memory(app.summary.used_memory),
                format_memory(app.summary.total_memory)
            ),
            memory_style(app.summary.used_memory),
        ),
        (
            "Processes",
            app.summary.process_count.to_string(),
            Style::default().fg(Color::Cyan),
        ),
        (
            "TCP Endpoints",
            app.summary.tcp_endpoint_count.to_string(),
            Style::default().fg(Color::Green),
        ),
    ];

    for (index, (title, value, style)) in cards.into_iter().enumerate() {
        let widget = Paragraph::new(value)
            .style(style.add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL).title(title));
        frame.render_widget(widget, sections[index]);
    }
}

fn render_tabs(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState) {
    let block = Block::default().borders(Borders::ALL).title("Views");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let tab_areas = tabs_hit_areas(area);
    for (index, tab_area) in tab_areas.iter().enumerate() {
        let tab = ActiveTab::from_index(index);
        let is_active = tab == app.active_tab;
        let text = format!(" {} ", tab.title());
        let paragraph = Paragraph::new(text)
            .style(if is_active {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            })
            .alignment(ratatui::layout::Alignment::Center);
        frame.render_widget(paragraph, *tab_area);
    }

    if inner.width > 0 {
        let used_width = tab_areas
            .last()
            .map(|last| last.x.saturating_add(last.width).saturating_sub(inner.x))
            .unwrap_or(0);
        if inner.width > used_width {
            let filler = Rect::new(
                inner.x.saturating_add(used_width),
                inner.y,
                inner.width.saturating_sub(used_width),
                inner.height,
            );
            frame.render_widget(Paragraph::new(""), filler);
        }
    }
}

fn render_process_table(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState) {
    if !app.process_sort_enabled {
        render_grouped_process_list(frame, area, app);
        return;
    }
    let entries = app.process_entries();
    let selected_display = selected_display_index(&entries, app.process_view.selected).unwrap_or(0);
    let visible_rows = area.height.saturating_sub(3) as usize;
    let (offset, selected_in_view) = visible_window(
        entries.len(),
        selected_display,
        app.process_view.offset,
        visible_rows.max(1),
    );
    let rows = entries
        .iter()
        .skip(offset)
        .take(visible_rows.max(1))
        .map(|entry| match entry {
            ProcessListEntry::Row(index) => {
                let row = &app.filtered_rows[*index];
                Row::new(vec![
                    Cell::from(row.pid.to_string()),
                    Cell::from(format!("{:.1}", row.cpu_percent)).style(cpu_style(row.cpu_percent)),
                    Cell::from(format_memory(row.memory_bytes)).style(memory_style(row.memory_bytes)),
                    Cell::from(format_runtime(row.runtime_secs)),
                    Cell::from(row.priority.clone()),
                    Cell::from(row.name.clone()),
                ])
                .style(process_row_style(row))
            }
        });
    let title = format!(
        "Processes | {} | tree={} | / search | right-click actions",
        if app.process_sort_enabled {
            format!("sort={}", app.sort_mode.label())
        } else {
            "sort=off".into()
        },
        if app.show_tree { "on" } else { "off" }
    );
    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(14),
            Constraint::Min(18),
        ],
    )
    .header(
        Row::new(vec!["PID", "CPU%", "Memory", "Runtime", "Priority", "Name"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(Style::default().bg(Color::Blue))
    .block(Block::default().borders(Borders::ALL).title(title));
    let mut state = TableState::default().with_selected(selected_in_view);
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_grouped_process_list(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState) {
    render_grouped_process_sections(frame, area, app, false);
}

fn render_process_tree(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState) {
    if !app.process_sort_enabled {
        render_grouped_process_sections(frame, area, app, true);
        return;
    }
    let entries = app.process_entries();
    let selected_display = selected_display_index(&entries, app.process_view.selected).unwrap_or(0);
    let visible_rows = area.height.saturating_sub(2) as usize;
    let (offset, selected_in_view) = visible_window(
        entries.len(),
        selected_display,
        app.process_view.offset,
        visible_rows.max(1),
    );
    let items: Vec<ListItem<'_>> = entries
        .iter()
        .skip(offset)
        .take(visible_rows.max(1))
        .map(|entry| match entry {
            ProcessListEntry::Row(index) => {
                let row = &app.filtered_rows[*index];
                let line = Line::from(vec![
                    Span::styled(
                        format!("{}{}", "  ".repeat(row.depth), row.name),
                        process_row_style(row),
                    ),
                    Span::styled(
                        format!(
                            " [{}] {:.1}% {} {}",
                            row.pid,
                            row.cpu_percent,
                            format_memory(row.memory_bytes),
                            row.priority
                        ),
                        process_row_style(row).fg(Color::DarkGray),
                    ),
                ]);
                ListItem::new(line)
            }
        })
        .collect();
    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::Blue))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Process Tree | Apps and background grouped | right-click actions"),
        );
    let mut state = ListState::default();
    state.select(selected_in_view);
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_grouped_process_sections(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &AppState,
    tree_mode: bool,
) {
    let outer = Block::default()
        .borders(Borders::ALL)
        .title(if tree_mode {
            "Processes | sort=off | grouped tree"
        } else {
            "Processes | sort=off | grouped"
        });
    frame.render_widget(outer, area);

    let (apps, background) = app.grouped_process_indexes();
    let (apps_header, apps_list_area, background_header, background_list_area) = grouped_process_sections(area);

    render_process_group_header(frame, apps_header, "Apps", apps.len(), Color::Black, Color::Cyan);
    render_process_group_header(
        frame,
        background_header,
        "Background",
        background.len(),
        Color::Black,
        Color::Yellow,
    );
    render_process_group_list(
        frame,
        apps_list_area,
        &apps,
        app,
        app.grouped_process_view.apps_offset,
        tree_mode,
    );
    render_process_group_list(
        frame,
        background_list_area,
        &background,
        app,
        app.grouped_process_view.background_offset,
        tree_mode,
    );

}

fn render_process_group_header(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    label: &str,
    count: usize,
    foreground: Color,
    background: Color,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let text = format!(" {} ({count}) ", label.to_ascii_uppercase());
    let mut band = text;
    let width = area.width as usize;
    if band.len() < width {
        band.push_str(&" ".repeat(width - band.len()));
    } else {
        band.truncate(width);
    }
    let header = Paragraph::new(band)
        .style(
            Style::default()
                .fg(foreground)
                .bg(background)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(header, area);
}

fn render_process_group_list(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    indexes: &[usize],
    app: &AppState,
    offset: usize,
    tree_mode: bool,
) {
    let selected_in_group = indexes
        .iter()
        .position(|index| *index == app.process_view.selected);
    let (_, selected_in_view) = visible_window(
        indexes.len(),
        selected_in_group.unwrap_or(0),
        offset,
        area.height.max(1) as usize,
    );
    let items: Vec<ListItem<'_>> = indexes
        .iter()
        .skip(offset)
        .take(area.height.max(1) as usize)
        .map(|index| {
            let row = &app.filtered_rows[*index];
            if tree_mode {
                let line = Line::from(vec![
                    Span::styled(
                        format!("{}{}", "  ".repeat(row.depth), row.name),
                        process_row_style(row),
                    ),
                    Span::styled(
                        format!(
                            " [{}] {:.1}% {} {}",
                            row.pid,
                            row.cpu_percent,
                            format_memory(row.memory_bytes),
                            row.priority
                        ),
                        process_row_style(row).fg(Color::DarkGray),
                    ),
                ]);
                ListItem::new(line)
            } else {
                let line = format!(
                    "{:<7} {:>5.1} {:>10} {:>8} {:<13} {}",
                    row.pid,
                    row.cpu_percent,
                    format_memory(row.memory_bytes),
                    format_runtime(row.runtime_secs),
                    row.priority,
                    row.name
                );
                ListItem::new(Line::from(Span::styled(line, process_row_style(row))))
            }
        })
        .collect();

    let list = List::new(items).highlight_style(
        Style::default()
            .fg(Color::Black)
            .bg(Color::LightCyan)
            .add_modifier(Modifier::BOLD),
    );
    let mut state = ListState::default();
    state.select(selected_in_view.filter(|_| selected_in_group.is_some()));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_service_table(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState) {
    let visible_rows = area.height.saturating_sub(3) as usize;
    let (offset, selected_in_view) = visible_window(
        app.filtered_services.len(),
        app.service_view.selected,
        app.service_view.offset,
        visible_rows.max(1),
    );
    let rows = app
        .filtered_services
        .iter()
        .skip(offset)
        .take(visible_rows.max(1))
        .map(|row| {
            Row::new(vec![
                Cell::from(row.display_name.clone()),
                Cell::from(row.service_name.clone()),
                Cell::from(row.status.clone()).style(service_status_style(&row.status)),
                Cell::from(row.start_type.clone()),
            ])
        });
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(34),
            Constraint::Percentage(30),
            Constraint::Length(14),
            Constraint::Length(12),
        ],
    )
    .header(
        Row::new(vec!["Display Name", "Service Name", "Status", "Start Type"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(Style::default().bg(Color::Blue))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Services | 1 start | 2 stop | 3 restart | right-click actions"),
    );
    let mut state = TableState::default().with_selected(selected_in_view);
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_network_filter_bar(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState) {
    let labels = vec![
        filter_chip(NetworkStateFilter::All, app.network_filter),
        filter_chip(NetworkStateFilter::Listening, app.network_filter),
        filter_chip(NetworkStateFilter::Established, app.network_filter),
    ];
    let paragraph = Paragraph::new(Line::from(labels))
        .block(Block::default().borders(Borders::ALL).title("Network Filter | f to cycle"));
    frame.render_widget(paragraph, area);
}

fn render_network_table(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState) {
    let visible_rows = area.height.saturating_sub(3) as usize;
    let (offset, selected_in_view) = visible_window(
        app.filtered_network.len(),
        app.network_view.selected,
        app.network_view.offset,
        visible_rows.max(1),
    );
    let rows = app
        .filtered_network
        .iter()
        .skip(offset)
        .take(visible_rows.max(1))
        .map(|row| {
            Row::new(vec![
                Cell::from(row.pid.to_string()),
                Cell::from(row.process_name.clone()),
                Cell::from(row.local_endpoint.clone()),
                Cell::from(row.remote_endpoint.clone()),
                Cell::from(row.state.clone()).style(network_state_style(&row.state)),
            ])
        });
    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(22),
            Constraint::Length(22),
            Constraint::Length(22),
            Constraint::Length(14),
        ],
    )
    .header(
        Row::new(vec!["PID", "Process", "Local", "Remote", "State"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(Style::default().bg(Color::Blue))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Network | right-click actions | o open folder"),
    );
    let mut state = TableState::default().with_selected(selected_in_view);
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_performance_view(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState) {
    let sections = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(area);
    let charts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(sections[0]);

    let cpu_history_raw: Vec<u64> = app.system_cpu_history.iter().copied().collect();
    let cpu_history = fit_history_to_width(&cpu_history_raw, charts[0].width.saturating_sub(2) as usize);
    let cpu = Sparkline::default()
        .block(Block::default().borders(Borders::ALL).title("CPU History (60s)"))
        .style(Style::default().fg(Color::LightRed))
        .max(100)
        .absent_value_style(Style::default().fg(Color::DarkGray))
        .data(&cpu_history);
    frame.render_widget(cpu, charts[0]);

    let memory_history_raw: Vec<u64> = app.system_memory_history.iter().copied().collect();
    let memory_history =
        fit_history_to_width(&memory_history_raw, charts[1].width.saturating_sub(2) as usize);
    let memory = Sparkline::default()
        .block(Block::default().borders(Borders::ALL).title("Memory History (%)"))
        .style(Style::default().fg(Color::LightBlue))
        .max(100)
        .absent_value_style(Style::default().fg(Color::DarkGray))
        .data(&memory_history);
    frame.render_widget(memory, charts[1]);

    let lines = vec![
        Line::from(format!("CPU usage: {:.1}%", app.summary.cpu_percent)),
        Line::from(format!(
            "Memory: {} / {}",
            format_memory(app.summary.used_memory),
            format_memory(app.summary.total_memory)
        )),
        Line::from(format!("Processes: {}", app.summary.process_count)),
        Line::from(format!("TCP endpoints: {}", app.summary.tcp_endpoint_count)),
        Line::from(""),
        Line::from("This page tracks whole-system trends."),
        Line::from("Disk, GPU, and per-core graphs are reserved for a later pass."),
    ];
    let summary = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("Performance Summary"))
        .wrap(Wrap { trim: false });
    frame.render_widget(summary, sections[1]);
}

fn fit_history_to_width(samples: &[u64], width: usize) -> Vec<Option<u64>> {
    let width = width.max(1);
    if samples.is_empty() {
        return vec![None; width];
    }
    if width == 1 {
        return vec![samples.last().copied()];
    }
    if samples.len() <= width {
        let mut aligned = vec![None; width - samples.len()];
        aligned.extend(samples.iter().copied().map(Some));
        return aligned;
    }
    let last_index = samples.len().saturating_sub(1);
    (0..width)
        .map(|slot| {
            let source_index = slot.saturating_mul(last_index) / (width - 1);
            Some(samples[source_index])
        })
        .collect()
}

fn render_process_detail_pane(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState) {
    let border_style = if matches!(app.pane_focus, PaneFocus::Detail) {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    let lines = if let Some(detail) = app.selected_process_detail() {
        vec![
            Line::from(format!("Name: {}", detail.name)),
            Line::from(format!("PID: {}", detail.pid)),
            Line::from(format!(
                "Parent PID: {}",
                detail
                    .parent_pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "-".into())
            )),
            Line::from(format!("CPU: {:.1}%", detail.cpu_percent)),
            Line::from(format!("Memory: {}", format_memory(detail.memory_bytes))),
            Line::from(format!("Runtime: {}", format_runtime(detail.runtime_secs))),
            Line::from(format!("Priority: {}", detail.priority)),
            Line::from(""),
            Line::from(format!("Exe: {}", detail.exe_path)),
            Line::from(""),
            Line::from("Actions: k close | K kill | r restart | z/x suspend-resume | 4-8 priority | o open folder"),
        ]
    } else {
        vec![Line::from("No process selected.")]
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Process Detail")
                    .border_style(border_style),
            )
            .scroll((app.process_detail_scroll, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_service_detail_pane(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState) {
    let border_style = if matches!(app.pane_focus, PaneFocus::Detail) {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    let lines = if let Some(service) = app.selected_service() {
        vec![
            Line::from(format!("Display name: {}", service.display_name)),
            Line::from(format!("Service name: {}", service.service_name)),
            Line::from(format!("Status: {}", service.status)),
            Line::from(format!("Start type: {}", service.start_type)),
            Line::from(""),
            Line::from("Actions: 1 start | 2 stop | 3 restart | right-click actions"),
        ]
    } else {
        vec![Line::from("No service selected.")]
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Service Detail")
                    .border_style(border_style),
            )
            .scroll((app.service_detail_scroll, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_network_detail_pane(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState) {
    let border_style = if matches!(app.pane_focus, PaneFocus::Detail) {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    let lines = if let Some(row) = app.selected_network() {
        vec![
            Line::from(format!("PID: {}", row.pid)),
            Line::from(format!("Process: {}", row.process_name)),
            Line::from(format!("Local: {}", row.local_endpoint)),
            Line::from(format!("Remote: {}", row.remote_endpoint)),
            Line::from(format!("State: {}", row.state)),
            Line::from(""),
            Line::from("Actions: k close | K kill | r restart | z/x suspend-resume | 4-8 priority | o open folder"),
        ]
    } else {
        vec![Line::from("No endpoint selected.")]
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Network Detail")
                    .border_style(border_style),
            )
            .scroll((app.network_detail_scroll, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_footer(frame: &mut ratatui::Frame<'_>, area: Rect, app: &AppState) {
    let lines = match app.active_tab {
        ActiveTab::Processes => vec![
            Line::from("Tab/Shift+Tab switch pages | / search | s sort/off | t tree | Enter toggle pane focus | Shift+F10 menu"),
            Line::from("k close | K kill | r restart | z/x suspend-resume | 4 idle | 5 below | 6 normal | 7 above | 8 high"),
            Line::from(app.feedback.clone()),
        ],
        ActiveTab::Performance => vec![
            Line::from("Tab/Shift+Tab switch pages | / search leaves query active for other tabs"),
            Line::from("Performance keeps the last 60 samples of CPU and memory."),
            Line::from(app.feedback.clone()),
        ],
        ActiveTab::Services => vec![
            Line::from("Tab/Shift+Tab switch pages | / search | 1 start | 2 stop | 3 restart | Shift+F10 menu"),
            Line::from("Use right-click for a service context menu."),
            Line::from(app.feedback.clone()),
        ],
        ActiveTab::Network => vec![
            Line::from("Tab/Shift+Tab switch pages | / search | f state filter | Enter toggle pane focus | Shift+F10 menu"),
            Line::from("k close | K kill | r restart | z/x suspend-resume | 4 idle | 5 below | 6 normal | 7 above | 8 high"),
            Line::from(app.feedback.clone()),
        ],
    };
    let header_style = if app.feedback.contains("Service unavailable") {
        Style::default().fg(Color::LightRed)
    } else if app.feedback.contains("failed") || app.feedback.contains("Failed") {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Cyan)
    };
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(header_style)
            .block(Block::default().borders(Borders::ALL).title("Help"))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_input_modal(frame: &mut ratatui::Frame<'_>, area: Rect, title: &str, value: &str) {
    frame.render_widget(Clear, area);
    let widget = Paragraph::new(value.to_string())
        .block(Block::default().borders(Borders::ALL).title(title))
        .style(Style::default().fg(Color::Yellow));
    frame.render_widget(widget, area);
}

fn render_force_kill_modal(frame: &mut ratatui::Frame<'_>, app: &AppState) {
    let area = centered_rect(frame.area(), 60, 30);
    frame.render_widget(Clear, area);
    let lines = match app.active_tab {
        ActiveTab::Processes => app
            .selected_process_detail()
            .map(|detail| {
                vec![
                    Line::from("Force kill confirmation"),
                    Line::from(""),
                    Line::from(format!("Name: {}", detail.name)),
                    Line::from(format!("PID: {}", detail.pid)),
                    Line::from(""),
                    Line::from("This uses TerminateProcess and can leave app state inconsistent."),
                    Line::from(""),
                    Line::from("Press Y or Enter to continue. Press N or Esc to cancel."),
                ]
            })
            .unwrap_or_else(|| vec![Line::from("No process selected.")]),
        ActiveTab::Network => app
            .selected_network()
            .map(|row| {
                vec![
                    Line::from("Force kill confirmation"),
                    Line::from(""),
                    Line::from(format!("Process: {}", row.process_name)),
                    Line::from(format!("PID: {}", row.pid)),
                    Line::from(""),
                    Line::from("This uses TerminateProcess and can leave app state inconsistent."),
                    Line::from(""),
                    Line::from("Press Y or Enter to continue. Press N or Esc to cancel."),
                ]
            })
            .unwrap_or_else(|| vec![Line::from("No endpoint selected.")]),
        _ => vec![Line::from("Force kill is only available on Processes and Network.")],
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Confirm Force Kill"))
            .wrap(Wrap { trim: false })
            .style(Style::default().bg(Color::Black).fg(Color::Yellow)),
        area,
    );
}

fn render_service_confirmation_modal(
    frame: &mut ratatui::Frame<'_>,
    app: &AppState,
    intent: ServiceActionIntent,
) {
    let area = centered_rect(frame.area(), 60, 30);
    frame.render_widget(Clear, area);
    let lines = if let Some(service) = app.selected_service() {
        vec![
            Line::from("Service action confirmation"),
            Line::from(""),
            Line::from(format!("Action: {}", intent.label())),
            Line::from(format!("Display: {}", service.display_name)),
            Line::from(format!("Service: {}", service.service_name)),
            Line::from(""),
            Line::from("Press Y or Enter to continue."),
            Line::from("Press N or Esc to cancel."),
        ]
    } else {
        vec![Line::from("No service selected.")]
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(intent.title()))
            .wrap(Wrap { trim: false })
            .style(Style::default().bg(Color::Black).fg(Color::Yellow)),
        area,
    );
}

fn render_priority_confirmation_modal(
    frame: &mut ratatui::Frame<'_>,
    app: &AppState,
    priority: ProcessPriority,
) {
    let area = centered_rect(frame.area(), 60, 30);
    frame.render_widget(Clear, area);
    let lines = match app.active_tab {
        ActiveTab::Processes => app
            .selected_process_detail()
            .map(|detail| {
                vec![
                    Line::from("Priority change confirmation"),
                    Line::from(""),
                    Line::from(format!("Name: {}", detail.name)),
                    Line::from(format!("PID: {}", detail.pid)),
                    Line::from(format!("Current: {}", detail.priority)),
                    Line::from(format!("New: {priority}")),
                    Line::from(""),
                    Line::from("Press Y or Enter to continue. Press N or Esc to cancel."),
                ]
            })
            .unwrap_or_else(|| vec![Line::from("No process selected.")]),
        ActiveTab::Network => app
            .selected_network()
            .map(|row| {
                vec![
                    Line::from("Priority change confirmation"),
                    Line::from(""),
                    Line::from(format!("Process: {}", row.process_name)),
                    Line::from(format!("PID: {}", row.pid)),
                    Line::from(format!("New: {priority}")),
                    Line::from(""),
                    Line::from("Press Y or Enter to continue. Press N or Esc to cancel."),
                ]
            })
            .unwrap_or_else(|| vec![Line::from("No endpoint selected.")]),
        _ => vec![Line::from("Priority changes are only available on Processes and Network.")],
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Confirm Priority Change"))
            .wrap(Wrap { trim: false })
            .style(Style::default().bg(Color::Black).fg(Color::Yellow)),
        area,
    );
}

fn render_context_menu(frame: &mut ratatui::Frame<'_>, app: &AppState) {
    let actions = app.current_context_actions();
    if actions.is_empty() {
        return;
    }
    let area = context_menu_area(frame.area(), actions.len());
    frame.render_widget(Clear, area);
    let items: Vec<ListItem<'_>> = actions
        .iter()
        .map(|action| ListItem::new(Line::from(action.label())))
        .collect();
    let title = match &app.overlay {
        Overlay::ContextMenu(ContextMenuState {
            kind: ContextMenuKind::Process,
            ..
        }) => "Process Actions",
        Overlay::ContextMenu(ContextMenuState {
            kind: ContextMenuKind::Service,
            ..
        }) => "Service Actions",
        Overlay::ContextMenu(ContextMenuState {
            kind: ContextMenuKind::Network,
            ..
        }) => "Network Actions",
        _ => "Actions",
    };
    let list = List::new(items)
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Yellow))
        .block(Block::default().borders(Borders::ALL).title(title));
    let mut state = ListState::default();
    let selected = match &app.overlay {
        Overlay::ContextMenu(state) => state.selected_index,
        _ => 0,
    };
    state.select(Some(selected.min(actions.len().saturating_sub(1))));
    frame.render_stateful_widget(list, area, &mut state);
}

fn perform_context_action(
    app: &mut AppState,
    state: &ContextMenuState,
    action: ContextAction,
) -> Result<()> {
    match action {
        ContextAction::OpenDetails => {
            app.pane_focus = PaneFocus::Detail;
            app.feedback = match state.kind {
                ContextMenuKind::Process => "Process detail pane focused.".into(),
                ContextMenuKind::Service => "Service detail pane focused.".into(),
                ContextMenuKind::Network => "Network detail pane focused.".into(),
            };
        }
        ContextAction::Close => {
            if let Some(pid) = context_target_pid(&state.target) {
                app.feedback = send_admin_command(AdminCommand::RequestCloseProcess { pid })?;
            }
        }
        ContextAction::Restart => {
            if let Some(pid) = context_target_pid(&state.target) {
                app.feedback = send_admin_command(AdminCommand::RestartProcess { pid })?;
                app.refresh_processes();
            }
        }
        ContextAction::ForceKill => {
            app.overlay = Overlay::ConfirmForceKill;
        }
        ContextAction::Suspend => {
            if let Some(pid) = context_target_pid(&state.target) {
                app.feedback = send_admin_command(AdminCommand::SuspendProcess { pid })?;
                app.refresh_processes();
            }
        }
        ContextAction::Resume => {
            if let Some(pid) = context_target_pid(&state.target) {
                app.feedback = send_admin_command(AdminCommand::ResumeProcess { pid })?;
                app.refresh_processes();
            }
        }
        ContextAction::OpenFolder => {
            app.feedback = match state.kind {
                ContextMenuKind::Process => open_process_path_by_pid(context_target_pid(&state.target)),
                ContextMenuKind::Network => open_network_path_by_pid(context_target_pid(&state.target)),
                ContextMenuKind::Service => "Services do not have an executable folder here.".into(),
            };
        }
        ContextAction::SetPriority(priority) => {
            app.overlay = Overlay::ConfirmPriorityChange(priority);
        }
        ContextAction::StartService => {
            app.overlay = Overlay::ConfirmServiceAction(ServiceActionIntent::Start);
        }
        ContextAction::StopService => {
            app.overlay = Overlay::ConfirmServiceAction(ServiceActionIntent::Stop);
        }
        ContextAction::RestartService => {
            app.overlay = Overlay::ConfirmServiceAction(ServiceActionIntent::Restart);
        }
    }
    Ok(())
}

fn rebuild_active_tab(app: &mut AppState) {
    match app.active_tab {
        ActiveTab::Processes => app.rebuild_process_view(),
        ActiveTab::Performance => {}
        ActiveTab::Services => app.rebuild_service_view(),
        ActiveTab::Network => app.rebuild_network_view(),
    }
}

fn send_admin_command(command: AdminCommand) -> Result<String> {
    let request = ApiRequest {
        request_id: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string(),
        version: API_VERSION.into(),
        command: command.clone(),
    };
    let response = match send_request(&request) {
        Ok(response) => response,
        Err(error) => return Ok(handle_local_fallback(command, error.to_string())),
    };
    if !response.ok {
        let error = response.error.unwrap_or(TasktuiError::ServiceUnavailable);
        return Ok(format!("Admin operation failed: {error}"));
    }
    Ok(match response.result {
        Some(AdminResult::Pong) => "Service reachable.".into(),
        Some(AdminResult::ProcessClosed { pid, forced }) => {
            if forced {
                format!("Process {pid} terminated.")
            } else {
                format!("Close requested for process {pid}.")
            }
        }
        Some(AdminResult::ProcessRestarted { pid }) => {
            format!("Process {pid} restarted.")
        }
        Some(AdminResult::ProcessStateChanged { pid, action }) => {
            format!("Process {pid} {action}.")
        }
        Some(AdminResult::ProcessPriorityChanged { pid, priority }) => {
            format!("Process {pid} priority set to {priority}.")
        }
        Some(AdminResult::ServiceStateChanged {
            service_name,
            action,
        }) => {
            format!("Service {service_name} {action}.")
        }
        None => "Service returned no result.".into(),
    })
}

fn handle_local_fallback(command: AdminCommand, transport_error: String) -> String {
    if !transport_error.contains("service not reachable") {
        return format!("Admin operation failed: {transport_error}");
    }

    let local_result = match command {
        AdminCommand::RequestCloseProcess { pid } => request_close_process(pid)
            .map(|_| format!("Close requested for process {pid}. (local fallback)")),
        AdminCommand::RestartProcess { pid } => restart_process(pid)
            .map(|_| format!("Process {pid} restarted. (local fallback)")),
        AdminCommand::ForceKillProcess { pid } => force_kill_process(pid)
            .map(|_| format!("Process {pid} terminated. (local fallback)")),
        AdminCommand::SuspendProcess { pid } => suspend_process(pid)
            .map(|_| format!("Process {pid} suspended. (local fallback)")),
        AdminCommand::ResumeProcess { pid } => resume_process(pid)
            .map(|_| format!("Process {pid} resumed. (local fallback)")),
        AdminCommand::SetPriority { pid, priority } => set_process_priority(pid, priority)
            .map(|_| format!("Process {pid} priority set to {priority}. (local fallback)")),
        AdminCommand::Ping
        | AdminCommand::StartService { .. }
        | AdminCommand::StopService { .. }
        | AdminCommand::RestartService { .. } => {
            return format!("Admin operation failed: {transport_error}");
        }
    };

    match local_result {
        Ok(message) => message,
        Err(error) => format!(
            "Admin operation failed: service not reachable; local fallback also failed: {error}"
        ),
    }
}

fn set_selected_priority(app: &AppState, priority: ProcessPriority) -> Result<String> {
    match app.selected_target_pid() {
        Some(pid) => send_admin_command(AdminCommand::SetPriority { pid, priority }),
        None => Ok("No process selected.".into()),
    }
}

fn increment_detail_scroll(app: &mut AppState) {
    match app.active_tab {
        ActiveTab::Processes => {
            app.process_detail_scroll = app.process_detail_scroll.saturating_add(1);
        }
        ActiveTab::Services => {
            app.service_detail_scroll = app.service_detail_scroll.saturating_add(1);
        }
        ActiveTab::Network => {
            app.network_detail_scroll = app.network_detail_scroll.saturating_add(1);
        }
        ActiveTab::Performance => {}
    }
}

fn decrement_detail_scroll(app: &mut AppState) {
    match app.active_tab {
        ActiveTab::Processes => {
            app.process_detail_scroll = app.process_detail_scroll.saturating_sub(1);
        }
        ActiveTab::Services => {
            app.service_detail_scroll = app.service_detail_scroll.saturating_sub(1);
        }
        ActiveTab::Network => {
            app.network_detail_scroll = app.network_detail_scroll.saturating_sub(1);
        }
        ActiveTab::Performance => {}
    }
}

fn context_target_pid(target: &ContextMenuTarget) -> Option<u32> {
    match target {
        ContextMenuTarget::ProcessPid(pid) | ContextMenuTarget::NetworkPid(pid) => Some(*pid),
        ContextMenuTarget::ServiceName(_) => None,
    }
}

fn open_selected_process_path(app: &AppState) -> String {
    match app
        .filtered_rows
        .get(app.process_view.selected)
        .and_then(|row| row.exe_path.as_deref())
    {
        Some(path) => match open_path_in_explorer(Path::new(path)) {
            Ok(()) => format!("Opened executable folder for {path}."),
            Err(error) => format!("Failed to open path: {error}"),
        },
        None => "Selected process has no executable path.".into(),
    }
}

fn open_process_path_by_pid(pid: Option<u32>) -> String {
    match pid.and_then(|pid| {
        resolve_process_path(pid)
            .ok()
            .map(|path| (pid, path))
    }) {
        Some((pid, path)) => match open_path_in_explorer(Path::new(&path)) {
            Ok(()) => format!("Opened executable folder for PID {pid}."),
            Err(error) => format!("Failed to open path: {error}"),
        },
        None => "Selected process has no executable path.".into(),
    }
}

fn open_selected_network_path(app: &AppState) -> String {
    match app.selected_network() {
        Some(row) => match resolve_process_path(row.pid) {
            Ok(path) => match open_path_in_explorer(Path::new(&path)) {
                Ok(()) => format!("Opened executable folder for PID {}.", row.pid),
                Err(error) => format!("Failed to open path: {error}"),
            },
            Err(error) => format!("Failed to resolve path: {error}"),
        },
        None => "No endpoint selected.".into(),
    }
}

fn open_network_path_by_pid(pid: Option<u32>) -> String {
    match pid {
        Some(pid) => match resolve_process_path(pid) {
            Ok(path) => match open_path_in_explorer(Path::new(&path)) {
                Ok(()) => format!("Opened executable folder for PID {pid}."),
                Err(error) => format!("Failed to open path: {error}"),
            },
            Err(error) => format!("Failed to resolve path: {error}"),
        },
        None => "No endpoint selected.".into(),
    }
}

fn resolve_process_path(pid: u32) -> Result<String> {
    let mut system =
        System::new_with_specifics(RefreshKind::nothing().with_processes(ProcessRefreshKind::everything()));
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
        true,
        ProcessRefreshKind::everything(),
    );
    let process = system
        .process(Pid::from_u32(pid))
        .ok_or_else(|| anyhow!("process not found: {pid}"))?;
    let path = process
        .exe()
        .ok_or_else(|| anyhow!("process has no executable path: {pid}"))?;
    Ok(path.display().to_string())
}

fn format_memory(memory_bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    format!("{:.1} MiB", memory_bytes as f64 / MIB)
}

fn format_runtime(runtime_secs: u64) -> String {
    let hours = runtime_secs / 3600;
    let minutes = (runtime_secs % 3600) / 60;
    let seconds = runtime_secs % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn root_sections(area: Rect) -> [Rect; 4] {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(5),
        ])
        .split(area);
    [layout[0], layout[1], layout[2], layout[3]]
}

fn body_sections(area: Rect) -> [Rect; 2] {
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(64), Constraint::Percentage(36)])
        .split(area);
    [layout[0], layout[1]]
}

fn network_list_sections(area: Rect) -> [Rect; 2] {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(5)])
        .split(area);
    [layout[0], layout[1]]
}

fn centered_rect(area: Rect, width_percent: u16, height_percent: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_percent) / 2),
            Constraint::Percentage(height_percent),
            Constraint::Percentage((100 - height_percent) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1])[1]
}

fn context_menu_area(area: Rect, items: usize) -> Rect {
    let height = (items as u16).saturating_add(2).clamp(6, 16);
    let width = 34;
    let y = area
        .y
        .saturating_add(area.height.saturating_sub(height) / 2);
    let x = area
        .x
        .saturating_add(area.width.saturating_sub(width) / 2);
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

fn visible_window(total: usize, selected: usize, offset: usize, visible: usize) -> (usize, Option<usize>) {
    if total == 0 || visible == 0 {
        return (0, None);
    }
    let selected = selected.min(total.saturating_sub(1));
    let max_offset = total.saturating_sub(visible);
    let offset = offset.min(max_offset);
    let offset = if selected < offset {
        selected
    } else if selected >= offset.saturating_add(visible) {
        selected.saturating_add(1).saturating_sub(visible)
    } else {
        offset
    }
    .min(max_offset);
    (offset, Some(selected.saturating_sub(offset)))
}

fn adjusted_offset(total: usize, selected: usize, offset: usize, visible: usize) -> usize {
    visible_window(total, selected, offset, visible).0
}

fn contains_point(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x
        && x < area.x.saturating_add(area.width)
        && y >= area.y
        && y < area.y.saturating_add(area.height)
}

fn list_index_at_row(area: Rect, row: u16, has_header: bool) -> Option<usize> {
    let content_top = area.y.saturating_add(if has_header { 2 } else { 1 });
    let content_bottom = area.y.saturating_add(area.height.saturating_sub(1));
    if row < content_top || row >= content_bottom {
        None
    } else {
        Some((row - content_top) as usize)
    }
}

fn plain_list_index_at_row(area: Rect, row: u16) -> Option<usize> {
    let content_bottom = area.y.saturating_add(area.height);
    if row < area.y || row >= content_bottom {
        None
    } else {
        Some((row - area.y) as usize)
    }
}

fn tab_at_position(area: Rect, x: u16) -> Option<ActiveTab> {
    for (index, tab_area) in tabs_hit_areas(area).iter().enumerate() {
        if x >= tab_area.x && x < tab_area.x.saturating_add(tab_area.width) {
            return Some(ActiveTab::from_index(index));
        }
    }
    None
}

fn tabs_hit_areas(area: Rect) -> Vec<Rect> {
    let inner = Block::default().borders(Borders::ALL).inner(area);
    if inner.width == 0 || inner.height == 0 {
        return Vec::new();
    }

    let titles = ActiveTab::titles();
    let mut areas = Vec::with_capacity(titles.len());
    let mut cursor_x = inner.x;
    for title in titles {
        let width = (title.width() as u16).saturating_add(2);
        if cursor_x >= inner.x.saturating_add(inner.width) {
            break;
        }
        let remaining = inner
            .x
            .saturating_add(inner.width)
            .saturating_sub(cursor_x);
        let actual_width = width.min(remaining);
        areas.push(Rect::new(cursor_x, inner.y, actual_width, inner.height));
        cursor_x = cursor_x.saturating_add(actual_width);
    }
    areas
}

fn grouped_process_indexes(rows: &[ProcessRow]) -> (Vec<usize>, Vec<usize>) {
    let apps = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| matches!(row.category, ProcessCategory::App).then_some(index))
        .collect();
    let background = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| matches!(row.category, ProcessCategory::Background).then_some(index))
        .collect();
    (apps, background)
}

fn selected_display_index(entries: &[ProcessListEntry], selected_process_index: usize) -> Option<usize> {
    entries.iter().position(|entry| matches!(entry, ProcessListEntry::Row(index) if *index == selected_process_index))
}

fn adjusted_process_offset(
    entries: &[ProcessListEntry],
    selected_process_index: usize,
    offset: usize,
    visible_rows: usize,
) -> usize {
    let selected_display = selected_display_index(entries, selected_process_index).unwrap_or(0);
    adjusted_offset(entries.len(), selected_display, offset, visible_rows.max(1))
}

fn display_to_process_index(entries: &[ProcessListEntry], display_index: usize) -> Option<usize> {
    entries.get(display_index).map(|ProcessListEntry::Row(index)| *index)
}

fn next_process_index(entries: &[ProcessListEntry], current_process_index: usize) -> Option<usize> {
    let current_display = selected_display_index(entries, current_process_index)?;
    entries
        .iter()
        .skip(current_display.saturating_add(1))
        .map(|entry| match entry {
            ProcessListEntry::Row(index) => *index,
        })
        .next()
        .or(Some(current_process_index))
}

fn prev_process_index(entries: &[ProcessListEntry], current_process_index: usize) -> Option<usize> {
    let current_display = selected_display_index(entries, current_process_index)?;
    entries
        .iter()
        .take(current_display)
        .rev()
        .map(|entry| match entry {
            ProcessListEntry::Row(index) => *index,
        })
        .next()
        .or(Some(current_process_index))
}

fn grouped_process_sections(area: Rect) -> (Rect, Rect, Rect, Rect) {
    let inner = Block::default().borders(Borders::ALL).inner(area);
    if inner.height < 4 {
        return (
            Rect::new(inner.x, inner.y, inner.width, inner.height.min(1)),
            Rect::new(inner.x, inner.y.saturating_add(1), inner.width, 0),
            Rect::new(inner.x, inner.y.saturating_add(1), inner.width, 0),
            Rect::new(inner.x, inner.y.saturating_add(1), inner.width, 0),
        );
    }

    let remaining = inner.height.saturating_sub(2);
    let apps_list_height = (remaining / 2).max(1);
    let background_list_height = remaining.saturating_sub(apps_list_height).max(1);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(apps_list_height),
            Constraint::Length(1),
            Constraint::Length(background_list_height),
        ])
        .split(inner);
    (layout[0], layout[1], layout[2], layout[3])
}

fn network_filter_at_position(area: Rect, x: u16) -> Option<NetworkStateFilter> {
    if area.width <= 2 || x < area.x.saturating_add(1) || x >= area.x.saturating_add(area.width.saturating_sub(1)) {
        return None;
    }
    let inner = area.width.saturating_sub(2).max(1);
    let relative = x.saturating_sub(area.x.saturating_add(1));
    let bucket = ((relative as usize) * 3) / inner as usize;
    Some(match bucket.min(2) {
        0 => NetworkStateFilter::All,
        1 => NetworkStateFilter::Listening,
        _ => NetworkStateFilter::Established,
    })
}

fn select_row_at(app: &mut AppState, list_area: Rect, row: u16) {
    match app.active_tab {
        ActiveTab::Processes => {
            if !app.process_sort_enabled {
                let (_, apps_list_area, _, background_list_area) = grouped_process_sections(list_area);
                let (apps, background) = app.grouped_process_indexes();
                if let Some(index) = plain_list_index_at_row(apps_list_area, row) {
                    let visible_index = index.saturating_add(app.grouped_process_view.apps_offset);
                    if let Some(process_index) = apps.get(visible_index) {
                        app.process_view.selected = *process_index;
                        app.sync_process_view_state();
                    }
                } else if let Some(index) = plain_list_index_at_row(background_list_area, row) {
                    let visible_index = index.saturating_add(app.grouped_process_view.background_offset);
                    if let Some(process_index) = background.get(visible_index) {
                        app.process_view.selected = *process_index;
                        app.sync_process_view_state();
                    }
                }
                return;
            }
            let has_header = app.process_sort_enabled && !app.show_tree;
            if let Some(index) = list_index_at_row(list_area, row, has_header) {
                let entries = app.process_entries();
                let visible_index = index.saturating_add(app.process_view.offset);
                if let Some(process_index) = display_to_process_index(&entries, visible_index) {
                    app.process_view.selected = process_index.min(app.filtered_rows.len().saturating_sub(1));
                }
                app.sync_process_view_state();
            }
        }
        ActiveTab::Services => {
            if let Some(index) = list_index_at_row(list_area, row, true) {
                app.service_view.selected = index.min(app.filtered_services.len().saturating_sub(1));
                app.service_view.offset = adjusted_offset(
                    app.filtered_services.len(),
                    app.service_view.selected,
                    app.service_view.offset,
                    app.service_view.visible_rows.max(1),
                );
            }
        }
        ActiveTab::Network => {
            let sections = network_list_sections(list_area);
            if let Some(index) = list_index_at_row(sections[1], row, true) {
                app.network_view.selected = index.min(app.filtered_network.len().saturating_sub(1));
                app.network_view.offset = adjusted_offset(
                    app.filtered_network.len(),
                    app.network_view.selected,
                    app.network_view.offset,
                    app.network_view.visible_rows.max(1),
                );
            }
        }
        ActiveTab::Performance => {}
    }
}

fn process_row_style(row: &ProcessRow) -> Style {
    if row.cpu_percent >= 80.0 {
        Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD)
    } else if row.memory_bytes >= 1024 * 1024 * 1024 {
        Style::default().fg(Color::Yellow)
    } else if row.cpu_percent >= 40.0 {
        Style::default().fg(Color::LightYellow)
    } else {
        Style::default()
    }
}

fn cpu_style(cpu_percent: f32) -> Style {
    if cpu_percent >= 80.0 {
        Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD)
    } else if cpu_percent >= 40.0 {
        Style::default().fg(Color::LightYellow)
    } else {
        Style::default()
    }
}

fn memory_style(memory_bytes: u64) -> Style {
    if memory_bytes >= 2 * 1024 * 1024 * 1024 {
        Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD)
    } else if memory_bytes >= 1024 * 1024 * 1024 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    }
}

fn service_status_style(status: &str) -> Style {
    if status.eq_ignore_ascii_case("running") {
        Style::default().fg(Color::LightGreen).add_modifier(Modifier::BOLD)
    } else if status.eq_ignore_ascii_case("stopped") {
        Style::default().fg(Color::Gray)
    } else {
        Style::default().fg(Color::Yellow)
    }
}

fn network_state_style(state: &str) -> Style {
    if state.eq_ignore_ascii_case("listen") {
        Style::default().fg(Color::LightGreen)
    } else if state.eq_ignore_ascii_case("established") {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Gray)
    }
}

fn filter_chip(filter: NetworkStateFilter, active: NetworkStateFilter) -> Span<'static> {
    let style = if filter == active {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    Span::styled(format!(" {} ", filter.label()), style)
}

fn push_sample(samples: &mut VecDeque<u64>, value: u64, max_len: usize) {
    samples.push_back(value);
    while samples.len() > max_len {
        samples.pop_front();
    }
}

fn flatten_process_tree(rows: &[ProcessRow]) -> Vec<ProcessRow> {
    let mut by_parent: BTreeMap<Option<u32>, Vec<ProcessRow>> = BTreeMap::new();
    for row in rows {
        by_parent.entry(row.parent_pid).or_default().push(row.clone());
    }

    fn visit(
        parent: Option<u32>,
        depth: usize,
        by_parent: &mut BTreeMap<Option<u32>, Vec<ProcessRow>>,
        output: &mut Vec<ProcessRow>,
    ) {
        if let Some(mut children) = by_parent.remove(&parent) {
            children.sort_by(|left, right| left.pid.cmp(&right.pid));
            for mut child in children {
                child.depth = depth;
                let pid = child.pid;
                output.push(child);
                visit(Some(pid), depth + 1, by_parent, output);
            }
        }
    }

    let mut output = Vec::new();
    let mut map = by_parent;
    visit(None, 0, &mut map, &mut output);
    for (_, mut orphans) in map {
        orphans.sort_by(|left, right| left.pid.cmp(&right.pid));
        for mut orphan in orphans {
            orphan.depth = 0;
            output.push(orphan);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rows() -> Vec<ProcessRow> {
        vec![
            ProcessRow {
                pid: 10,
                parent_pid: None,
                depth: 0,
                category: ProcessCategory::App,
                name: "alpha".into(),
                exe_path: None,
                priority: "normal".into(),
                cpu_percent: 1.0,
                memory_bytes: 100,
                runtime_secs: 10,
            },
            ProcessRow {
                pid: 11,
                parent_pid: Some(10),
                depth: 0,
                category: ProcessCategory::Background,
                name: "beta".into(),
                exe_path: None,
                priority: "normal".into(),
                cpu_percent: 3.0,
                memory_bytes: 300,
                runtime_secs: 20,
            },
            ProcessRow {
                pid: 12,
                parent_pid: Some(10),
                depth: 0,
                category: ProcessCategory::Background,
                name: "gamma".into(),
                exe_path: None,
                priority: "normal".into(),
                cpu_percent: 2.0,
                memory_bytes: 200,
                runtime_secs: 30,
            },
        ]
    }

    #[test]
    fn sorts_by_cpu_desc() {
        let mut rows = sample_rows();
        let app = AppState::new();
        app.apply_process_sort(&mut rows);
        assert_eq!(rows.iter().map(|row| row.pid).collect::<Vec<_>>(), vec![11, 12, 10]);
    }

    #[test]
    fn process_sort_is_off_by_default() {
        let app = AppState::new();
        assert!(!app.process_sort_enabled);
    }

    #[test]
    fn filters_services_by_status() {
        let app = AppState {
            query: "run".into(),
            ..AppState::new()
        };
        let rows = app.apply_service_filter(vec![
            ServiceRow {
                display_name: "Print Spooler".into(),
                service_name: "Spooler".into(),
                status: "running".into(),
                start_type: "auto".into(),
            },
            ServiceRow {
                display_name: "Updater".into(),
                service_name: "UpdaterSvc".into(),
                status: "stopped".into(),
                start_type: "manual".into(),
            },
        ]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].service_name, "Spooler");
    }

    #[test]
    fn network_filter_matches_expected_states() {
        assert!(NetworkStateFilter::All.matches("listen"));
        assert!(NetworkStateFilter::Listening.matches("listen"));
        assert!(!NetworkStateFilter::Listening.matches("established"));
        assert!(NetworkStateFilter::Established.matches("established"));
    }

    #[test]
    fn builds_tree_depths() {
        let rows = flatten_process_tree(&sample_rows());
        assert_eq!(rows[0].pid, 10);
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[2].depth, 1);
    }

    #[test]
    fn visible_window_keeps_selection_in_view() {
        let (offset, selected) = visible_window(100, 99, 0, 8);
        assert_eq!(offset, 92);
        assert_eq!(selected, Some(7));

        let (offset, selected) = visible_window(100, 95, 92, 8);
        assert_eq!(offset, 92);
        assert_eq!(selected, Some(3));
    }

    #[test]
    fn history_buffer_rotates() {
        let mut samples = VecDeque::new();
        for value in 0..70 {
            push_sample(&mut samples, value, 60);
        }
        assert_eq!(samples.len(), 60);
        assert_eq!(samples.front().copied(), Some(10));
        assert_eq!(samples.back().copied(), Some(69));
    }

    #[test]
    fn history_resamples_to_requested_width() {
        let samples = vec![10, 20, 30];
        assert_eq!(
            fit_history_to_width(&samples, 5),
            vec![None, None, Some(10), Some(20), Some(30)]
        );
        assert_eq!(fit_history_to_width(&samples, 2), vec![Some(10), Some(30)]);
    }

    #[test]
    fn context_menu_contains_above_normal_priority() {
        assert!(PROCESS_CONTEXT_ACTIONS.contains(&ContextAction::SetPriority(
            ProcessPriority::AboveNormal,
        )));
        assert!(NETWORK_CONTEXT_ACTIONS.contains(&ContextAction::SetPriority(
            ProcessPriority::AboveNormal,
        )));
    }

    #[test]
    fn process_priority_refresh_targets_visible_rows() {
        let mut app = AppState::new();
        app.filtered_rows = (0..5)
            .map(|pid| ProcessRow {
                pid,
                parent_pid: None,
                depth: 0,
                category: ProcessCategory::Background,
                name: format!("p{pid}"),
                exe_path: None,
                priority: "unknown".into(),
                cpu_percent: 0.0,
                memory_bytes: 0,
                runtime_secs: 0,
            })
            .collect();
        app.process_view.visible_rows = 2;
        app.process_view.selected = 3;
        app.process_view.offset = 2;
        app.process_sort_enabled = true;
        assert_eq!(app.visible_process_pids(), vec![2, 3]);
    }

    #[test]
    fn detail_focus_scrolls_detail_instead_of_selection() {
        let mut app = AppState::new();
        app.pane_focus = PaneFocus::Detail;
        app.process_detail_scroll = 0;
        app.process_view.selected = 4;
        increment_detail_scroll(&mut app);
        assert_eq!(app.process_detail_scroll, 1);
        assert_eq!(app.process_view.selected, 4);
    }

    #[test]
    fn tab_hit_testing_uses_rendered_tab_widths() {
        let area = Rect::new(0, 0, 80, 3);
        let areas = tabs_hit_areas(area);
        assert_eq!(areas.len(), 4);
        assert_eq!(tab_at_position(area, areas[0].x), Some(ActiveTab::Processes));
        assert_eq!(tab_at_position(area, areas[1].x), Some(ActiveTab::Performance));
        assert_eq!(tab_at_position(area, areas[2].x), Some(ActiveTab::Services));
        assert_eq!(tab_at_position(area, areas[3].x), Some(ActiveTab::Network));
    }

    #[test]
    fn grouped_process_indexes_keep_apps_before_background() {
        let rows = vec![
            ProcessRow {
                pid: 1,
                parent_pid: None,
                depth: 0,
                category: ProcessCategory::App,
                name: "app".into(),
                exe_path: None,
                priority: "normal".into(),
                cpu_percent: 0.0,
                memory_bytes: 0,
                runtime_secs: 0,
            },
            ProcessRow {
                pid: 2,
                parent_pid: None,
                depth: 0,
                category: ProcessCategory::Background,
                name: "bg".into(),
                exe_path: None,
                priority: "normal".into(),
                cpu_percent: 0.0,
                memory_bytes: 0,
                runtime_secs: 0,
            },
            ProcessRow {
                pid: 3,
                parent_pid: None,
                depth: 0,
                category: ProcessCategory::App,
                name: "app2".into(),
                exe_path: None,
                priority: "normal".into(),
                cpu_percent: 0.0,
                memory_bytes: 0,
                runtime_secs: 0,
            },
        ];
        let (apps, background) = grouped_process_indexes(&rows);
        assert_eq!(apps, vec![0, 2]);
        assert_eq!(background, vec![1]);
    }

    #[test]
    fn process_entries_are_grouped_without_header_rows_when_sort_is_off() {
        let rows = vec![ProcessRow {
            pid: 1,
            parent_pid: None,
            depth: 0,
            category: ProcessCategory::App,
            name: "app".into(),
            exe_path: None,
            priority: "normal".into(),
            cpu_percent: 0.0,
            memory_bytes: 0,
            runtime_secs: 0,
        },
        ProcessRow {
            pid: 2,
            parent_pid: None,
            depth: 0,
            category: ProcessCategory::Background,
            name: "bg".into(),
            exe_path: None,
            priority: "normal".into(),
            cpu_percent: 0.0,
            memory_bytes: 0,
            runtime_secs: 0,
        }];
        let mut app = AppState::new();
        app.filtered_rows = rows;
        let entries = app.process_entries();
        assert_eq!(entries, vec![ProcessListEntry::Row(0), ProcessListEntry::Row(1)]);
    }

    #[test]
    fn grouped_process_sections_reserve_header_bands() {
        let area = Rect::new(0, 0, 80, 20);
        let (apps_header, apps_list, background_header, background_list) = grouped_process_sections(area);
        assert_eq!(apps_header.height, 1);
        assert_eq!(background_header.height, 1);
        assert!(apps_list.height > 0);
        assert!(background_list.height > 0);
    }

    #[test]
    fn plain_list_index_matches_first_visual_row() {
        let area = Rect::new(10, 5, 40, 4);
        assert_eq!(plain_list_index_at_row(area, 5), Some(0));
        assert_eq!(plain_list_index_at_row(area, 6), Some(1));
        assert_eq!(plain_list_index_at_row(area, 9), None);
    }

    #[test]
    fn shift_f10_opens_process_context_menu_for_selected_row() {
        let mut app = AppState::new();
        app.filtered_rows = sample_rows();
        app.process_view.selected = 1;
        open_context_menu_for_selection(&mut app);
        assert!(matches!(
            app.overlay,
            Overlay::ContextMenu(ContextMenuState {
                kind: ContextMenuKind::Process,
                target: ContextMenuTarget::ProcessPid(11),
                ..
            })
        ));
    }

    #[test]
    fn process_entries_are_flat_when_sort_enabled() {
        let mut app = AppState::new();
        app.filtered_rows = sample_rows();
        app.process_sort_enabled = true;
        let entries = app.process_entries();
        assert_eq!(
            entries,
            vec![
                ProcessListEntry::Row(0),
                ProcessListEntry::Row(1),
                ProcessListEntry::Row(2),
            ]
        );
    }

    #[test]
    fn stable_order_is_preserved_for_existing_pids() {
        let mut app = AppState::new();
        app.process_stable_order.insert(10, 0);
        app.process_stable_order.insert(12, 1);
        app.next_process_order = 2;
        app.process_rows = vec![
            ProcessRow {
                pid: 12,
                parent_pid: None,
                depth: 0,
                category: ProcessCategory::Background,
                name: "later".into(),
                exe_path: None,
                priority: "normal".into(),
                cpu_percent: 0.0,
                memory_bytes: 0,
                runtime_secs: 0,
            },
            ProcessRow {
                pid: 10,
                parent_pid: None,
                depth: 0,
                category: ProcessCategory::Background,
                name: "first".into(),
                exe_path: None,
                priority: "normal".into(),
                cpu_percent: 0.0,
                memory_bytes: 0,
                runtime_secs: 0,
            },
        ];
        app.rebuild_process_view();
        assert_eq!(app.filtered_rows[0].pid, 10);
        assert_eq!(app.filtered_rows[1].pid, 12);
    }

    #[test]
    fn display_index_maps_to_process_row() {
        let entries = vec![ProcessListEntry::Row(0), ProcessListEntry::Row(1)];
        assert_eq!(display_to_process_index(&entries, 0), Some(0));
        assert_eq!(display_to_process_index(&entries, 1), Some(1));
    }

    #[test]
    fn process_navigation_follows_grouped_display_order() {
        let entries = vec![
            ProcessListEntry::Row(2),
            ProcessListEntry::Row(4),
            ProcessListEntry::Row(1),
        ];
        assert_eq!(next_process_index(&entries, 2), Some(4));
        assert_eq!(next_process_index(&entries, 4), Some(1));
        assert_eq!(prev_process_index(&entries, 1), Some(4));
    }
}
