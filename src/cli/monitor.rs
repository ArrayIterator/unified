// Live monitor dashboard (ratatui TUI, like htop / Ubuntu Monitor).
//
// Launched by `unified monitor` and `unified --monitor`. The dashboard
// runs as its own process: system data comes from sysinfo, and
// application data comes from the daemon status file
// (`unified.status.json`, see `crate::cli::daemon`). When the daemon is
// not running, the application panels say so instead of failing.
//
// Panels (keys 1-6, or Left/Right arrows):
//
//   1. Active Connections - HTTP vs WebSocket counts, requests per second.
//   2. Server Info       - version, uptime, mode, workers, listeners, memory.
//   3. Network Monitor   - interfaces, bytes in/out per second and totals.
//   4. Process Monitor   - top processes by CPU / memory.
//   5. Disk Monitor      - disk usage per mounted disk.
//   6. App Usage         - per-module request counts, hook events, Geo lookups.
//
// Sampling is tiered: CPU, memory, processes and networks refresh every
// second, while disks (mount list, usage, I/O rates) refresh every 30
// seconds and static CPU details (model, frequency, core counts) are read
// once at startup. The screen refreshes every second. `q`, `Q`, Esc, or
// Ctrl-C quits; `c` / `m` on the Processes panel switch the sort order.

use crate::cli::daemon::{read_status, DaemonStatus};
use crate::modules::service_monitor::{disk_kind_label, process_status_label};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Cell, Gauge, Paragraph, Row, Table, Tabs, Wrap},
    Frame, Terminal,
};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::{self, Stdout};
use std::time::{Duration, Instant};
use sysinfo::{Disks, Networks, System};

/// Refresh interval of the dashboard.
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
/// Disks change rarely, so the mount list and usage are refreshed on this
/// slower cadence instead of every tick.
const DISK_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
/// How many processes the Process Monitor panel shows.
const TOP_PROCESSES: usize = 10;

/// Sort order for the Process Monitor panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSort {
    Cpu,
    Memory,
}

/// One dashboard panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Panel {
    Connections,
    Server,
    Network,
    Processes,
    Disks,
    AppUsage,
}

impl Panel {
    const ALL: [Panel; 6] = [
        Panel::Connections,
        Panel::Server,
        Panel::Network,
        Panel::Processes,
        Panel::Disks,
        Panel::AppUsage,
    ];

    fn index(self) -> usize {
        Self::ALL.iter().position(|panel| *panel == self).unwrap_or(0)
    }

    fn title(self) -> &'static str {
        match self {
            Panel::Connections => "Connections",
            Panel::Server => "Server",
            Panel::Network => "Network",
            Panel::Processes => "Processes",
            Panel::Disks => "Disks",
            Panel::AppUsage => "App Usage",
        }
    }

    fn from_index(index: usize) -> Panel {
        Self::ALL[index % Self::ALL.len()]
    }

    fn next(self) -> Panel {
        Self::from_index(self.index() + 1)
    }

    fn prev(self) -> Panel {
        Self::from_index(self.index() + Self::ALL.len() - 1)
    }
}

/// One process row for the Process Monitor panel.
#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    /// One-letter state code: R running, S sleeping, Z zombie, and so on.
    pub status: String,
    /// Numeric user id of the process owner, when known.
    pub user_id: Option<String>,
    pub cpu: f32,
    pub memory: u64,
    pub virtual_memory: u64,
    pub disk_read_bytes: u64,
    pub disk_written_bytes: u64,
    pub run_time_seconds: u64,
}

/// One disk row for the Disk Monitor panel.
#[derive(Debug, Clone)]
pub struct DiskInfo {
    pub name: String,
    pub mount: String,
    pub file_system: String,
    pub kind: String,
    pub total: u64,
    pub available: u64,
    pub read_bytes_per_sec: u64,
    pub written_bytes_per_sec: u64,
}

/// One interface row for the Network Monitor panel.
#[derive(Debug, Clone)]
pub struct NetworkInfo {
    pub name: String,
    pub rx_per_sec: u64,
    pub tx_per_sec: u64,
    pub rx_total: u64,
    pub tx_total: u64,
    pub packets_rx: u64,
    pub packets_tx: u64,
    pub errors_rx: u64,
    pub errors_tx: u64,
}

/// One snapshot of everything the dashboard shows.
#[derive(Debug, Clone)]
pub struct MonitorData {
    pub collected_at: chrono::DateTime<chrono::Local>,
    pub cpu_usage: f32,
    pub cpu_per_core: Vec<f32>,
    pub memory_used: u64,
    pub memory_total: u64,
    pub memory_available: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    /// Load average over 1, 5 and 15 minutes.
    pub load_average: (f64, f64, f64),
    pub cpu_brand: Option<String>,
    pub cpu_frequency_mhz: u64,
    pub physical_cores: Option<usize>,
    pub host_uptime_seconds: u64,
    pub processes: Vec<ProcessInfo>,
    pub disks: Vec<DiskInfo>,
    pub networks: Vec<NetworkInfo>,
    /// Application data from the daemon status file, or `None` when the
    /// daemon is not running.
    pub daemon: Option<DaemonStatus>,
}

impl MonitorData {
    /// Collect a one-shot snapshot. Network rates are zero in a one-shot
    /// sample because there is no previous sample to compare against; use
    /// `MonitorCollector` for continuous sampling.
    pub fn collect() -> Self {
        MonitorCollector::new().collect()
    }
}

/// Stateful collector: keeps the sysinfo handles and the previous network
/// and disk totals so per-second rates can be computed between samples.
/// Disks refresh on a slower cadence than the 1-second tick.
pub struct MonitorCollector {
    system: System,
    disks: Disks,
    networks: Networks,
    prev_networks: HashMap<String, (u64, u64)>,
    prev_instant: Instant,
    cached_disks: Vec<DiskInfo>,
    last_disk_refresh: Option<Instant>,
    process_sort: ProcessSort,
    cpu_brand: Option<String>,
    cpu_frequency_mhz: u64,
    physical_cores: Option<usize>,
}

impl MonitorCollector {
    pub fn new() -> Self {
        let mut system = System::new_all();
        system.refresh_all();
        let first_cpu = system.cpus().first();
        Self {
            cpu_brand: first_cpu.map(|cpu| cpu.brand().to_string()),
            cpu_frequency_mhz: first_cpu.map(|cpu| cpu.frequency()).unwrap_or(0),
            physical_cores: system.physical_core_count(),
            system,
            disks: Disks::new_with_refreshed_list(),
            networks: Networks::new_with_refreshed_list(),
            prev_networks: HashMap::new(),
            prev_instant: Instant::now(),
            cached_disks: Vec::new(),
            last_disk_refresh: None,
            process_sort: ProcessSort::Cpu,
        }
    }

    /// Change the sort order used by the Process Monitor panel.
    pub fn set_process_sort(&mut self, sort: ProcessSort) {
        self.process_sort = sort;
    }

    /// Refresh all sources and return a new snapshot.
    pub fn collect(&mut self) -> MonitorData {
        self.system.refresh_all();
        self.networks.refresh();

        let now = Instant::now();
        let elapsed = now.duration_since(self.prev_instant).as_secs_f64().max(0.001);
        self.prev_instant = now;

        let mut networks = Vec::new();
        for (name, data) in self.networks.list() {
            let rx_total = data.total_received();
            let tx_total = data.total_transmitted();
            let (rx_per_sec, tx_per_sec) = match self.prev_networks.get(name) {
                Some((prev_rx, prev_tx)) => (
                    ((rx_total.saturating_sub(*prev_rx)) as f64 / elapsed) as u64,
                    ((tx_total.saturating_sub(*prev_tx)) as f64 / elapsed) as u64,
                ),
                None => (0, 0),
            };
            self.prev_networks
                .insert(name.clone(), (rx_total, tx_total));
            networks.push(NetworkInfo {
                name: name.clone(),
                rx_per_sec,
                tx_per_sec,
                rx_total,
                tx_total,
                packets_rx: data.packets_received(),
                packets_tx: data.packets_transmitted(),
                errors_rx: data.errors_on_received(),
                errors_tx: data.errors_on_transmitted(),
            });
        }
        networks.sort_by(|a, b| a.name.cmp(&b.name));

        let mut processes: Vec<ProcessInfo> = self
            .system
            .processes()
            .values()
            .map(|process| {
                let disk_usage = process.disk_usage();
                ProcessInfo {
                    pid: process.pid().as_u32(),
                    name: process.name().to_string_lossy().into_owned(),
                    status: process_status_label(process.status()).to_string(),
                    user_id: process.user_id().map(|uid| uid.to_string()),
                    cpu: process.cpu_usage(),
                    memory: process.memory(),
                    virtual_memory: process.virtual_memory(),
                    disk_read_bytes: disk_usage.total_read_bytes,
                    disk_written_bytes: disk_usage.total_written_bytes,
                    run_time_seconds: process.run_time(),
                }
            })
            .collect();
        match self.process_sort {
            ProcessSort::Cpu => processes.sort_by(|a, b| {
                b.cpu
                    .partial_cmp(&a.cpu)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| b.memory.cmp(&a.memory))
            }),
            ProcessSort::Memory => processes.sort_by(|a, b| {
                b.memory.cmp(&a.memory).then_with(|| {
                    b.cpu.partial_cmp(&a.cpu).unwrap_or(Ordering::Equal)
                })
            }),
        }
        processes.truncate(TOP_PROCESSES);

        // Disks change rarely: refresh the mount list and usage
        // at most every 30 seconds and reuse the cached rows in between.
        let disks_due = self
            .last_disk_refresh
            .map(|last| last.elapsed() >= DISK_REFRESH_INTERVAL)
            .unwrap_or(true);
        if disks_due {
            self.disks.refresh();
            self.cached_disks = self
                .disks
                .list()
                .iter()
                .map(|disk| {
                    // sysinfo 0.32 removed per-disk I/O counters
                    // (`Disk::usage`), so read/write rates are zero.
                    let (read_bytes_per_sec, written_bytes_per_sec) = (0, 0);
                    DiskInfo {
                        name: disk.name().to_string_lossy().into_owned(),
                        mount: disk.mount_point().to_string_lossy().into_owned(),
                        file_system: disk.file_system().to_string_lossy().into_owned(),
                        kind: disk_kind_label(disk.kind()).to_string(),
                        total: disk.total_space(),
                        available: disk.available_space(),
                        read_bytes_per_sec,
                        written_bytes_per_sec,
                    }
                })
                .collect();
            self.last_disk_refresh = Some(now);
        }

        let load = System::load_average();
        MonitorData {
            collected_at: chrono::Local::now(),
            cpu_usage: self.system.global_cpu_usage(),
            cpu_per_core: self
                .system
                .cpus()
                .iter()
                .map(|cpu| cpu.cpu_usage())
                .collect(),
            memory_used: self.system.used_memory(),
            memory_total: self.system.total_memory(),
            memory_available: self.system.available_memory(),
            swap_used: self.system.used_swap(),
            swap_total: self.system.total_swap(),
            load_average: (load.one, load.five, load.fifteen),
            cpu_brand: self.cpu_brand.clone(),
            cpu_frequency_mhz: self.cpu_frequency_mhz,
            physical_cores: self.physical_cores,
            host_uptime_seconds: System::uptime(),
            processes,
            disks: self.cached_disks.clone(),
            networks,
            daemon: read_status(),
        }
    }
}

/// Run the monitor dashboard. Returns the process exit code.
pub fn run_monitor() -> i32 {
    if let Err(error) = enable_raw_mode() {
        eprintln!("Cannot enable terminal raw mode: {error}");
        return 2;
    }
    // Restore the terminal even if the event loop panics or errors.
    let _guard = TerminalGuard;
    let mut stdout = io::stdout();
    if let Err(error) = execute!(stdout, EnterAlternateScreen) {
        eprintln!("Cannot enter the alternate screen: {error}");
        return 2;
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("Cannot start the terminal: {error}");
            return 2;
        }
    };
    match run_event_loop(&mut terminal) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Monitor error: {error}");
            2
        }
    }
}

/// Restores raw mode and the normal screen when dropped.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

fn run_event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    let mut collector = MonitorCollector::new();
    let mut panel = Panel::Connections;
    let mut data = collector.collect();

    loop {
        terminal.draw(|frame| draw_ui(frame, &data, panel))?;

        // Handle input until the next refresh is due.
        let deadline = Instant::now() + REFRESH_INTERVAL;
        loop {
            let timeout = deadline.saturating_duration_since(Instant::now());
            if timeout.is_zero() {
                break;
            }
            if event::poll(timeout)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                            return Ok(());
                        }
                        KeyCode::Char('c')
                            if key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            return Ok(());
                        }
                        KeyCode::Char(c @ '1'..='6') => {
                            panel = Panel::from_index(c as usize - '1' as usize);
                        }
                        KeyCode::Char('c') => collector.set_process_sort(ProcessSort::Cpu),
                        KeyCode::Char('m') => {
                            collector.set_process_sort(ProcessSort::Memory)
                        }
                        KeyCode::Left => panel = panel.prev(),
                        KeyCode::Right => panel = panel.next(),
                        _ => {}
                    }
                }
            } else {
                break;
            }
        }

        data = collector.collect();
    }
}

fn draw_ui(frame: &mut Frame, data: &MonitorData, panel: Panel) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    let titles: Vec<Line> = Panel::ALL
        .iter()
        .enumerate()
        .map(|(index, panel)| Line::from(format!("{} {}", index + 1, panel.title())))
        .collect();
    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(
                    " Unified Monitor - updated {} ",
                    data.collected_at.format("%H:%M:%S")
                )),
        )
        .select(panel.index())
        .style(Style::default().fg(Color::White))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(tabs, chunks[0]);

    match panel {
        Panel::Connections => draw_connections(frame, chunks[1], data),
        Panel::Server => draw_server(frame, chunks[1], data),
        Panel::Network => draw_network(frame, chunks[1], data),
        Panel::Processes => draw_processes(frame, chunks[1], data),
        Panel::Disks => draw_disks(frame, chunks[1], data),
        Panel::AppUsage => draw_app_usage(frame, chunks[1], data),
    }

    let footer = Paragraph::new(
        " 1-6 switch panel - Left/Right switch panel - c/m sort processes - q or Esc quits - refreshes every second ",
    )
    .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(footer, chunks[2]);
}

/// A two-column key/value table.
fn info_table<'a>(title: &'a str, rows: Vec<Row<'a>>) -> Table<'a> {
    Table::new(rows, [Constraint::Length(28), Constraint::Min(10)])
        .block(Block::default().borders(Borders::ALL).title(title))
        .column_spacing(2)
}

fn kv_row<'a>(key: &'a str, value: String) -> Row<'a> {
    Row::new(vec![Cell::from(key), Cell::from(value)])
}

fn not_running_paragraph(area_title: &'static str) -> Paragraph<'static> {
    Paragraph::new(
        "Daemon is not running.\n\nStart it with `unified start` to see live application data.",
    )
    .block(Block::default().borders(Borders::ALL).title(area_title))
    .wrap(Wrap { trim: true })
}

fn daemon_uptime_seconds(status: &DaemonStatus) -> u64 {
    if let Ok(started) = chrono::DateTime::parse_from_rfc3339(&status.started_at) {
        let elapsed = chrono::Utc::now()
            .signed_duration_since(started)
            .num_seconds();
        if elapsed >= 0 {
            return elapsed as u64;
        }
    }
    status.uptime_seconds
}

fn draw_connections(frame: &mut Frame, area: ratatui::layout::Rect, data: &MonitorData) {
    let Some(status) = &data.daemon else {
        frame.render_widget(not_running_paragraph(" Active Connections "), area);
        return;
    };
    let rows = vec![
        kv_row("HTTP connections", status.http_connections.to_string()),
        kv_row(
            "WebSocket connections",
            status.websocket_connections.to_string(),
        ),
        kv_row("Total requests", status.total_requests.to_string()),
        kv_row(
            "Requests per second",
            format!("{:.1}", status.requests_per_second),
        ),
        kv_row("Daemon PID", status.pid.to_string()),
        kv_row("Mode", status.mode.clone()),
        kv_row(
            "Uptime",
            crate::cli::daemon::format_uptime(daemon_uptime_seconds(status)),
        ),
    ];
    frame.render_widget(info_table(" Active Connections ", rows), area);
}

fn draw_server(frame: &mut Frame, area: ratatui::layout::Rect, data: &MonitorData) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(area);

    let average_cpu = if data.cpu_per_core.is_empty() {
        0.0
    } else {
        data.cpu_per_core.iter().sum::<f32>() / data.cpu_per_core.len() as f32
    };
    let mut rows = vec![
        kv_row(
            "Version",
            data.daemon
                .as_ref()
                .map(|status| status.version.clone())
                .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string()),
        ),
        kv_row("CPU usage", format!("{:.1}%", data.cpu_usage)),
        kv_row(
            "CPU cores",
            format!(
                "{} cores, {:.1}% average",
                data.cpu_per_core.len(),
                average_cpu
            ),
        ),
    ];
    if let Some(brand) = &data.cpu_brand {
        rows.push(kv_row("CPU model", brand.clone()));
    }
    if data.cpu_frequency_mhz > 0 {
        rows.push(kv_row(
            "CPU frequency",
            format!("{:.2} GHz", data.cpu_frequency_mhz as f64 / 1000.0),
        ));
    }
    if let Some(physical) = data.physical_cores {
        rows.push(kv_row("Physical cores", physical.to_string()));
    }
    rows.push(kv_row(
        "Load average",
        format!(
            "{:.2} {:.2} {:.2}",
            data.load_average.0, data.load_average.1, data.load_average.2
        ),
    ));
    rows.push(kv_row(
        "Memory used",
        format!(
            "{} / {}",
            human_bytes(data.memory_used),
            human_bytes(data.memory_total)
        ),
    ));
    rows.push(kv_row("Memory available", human_bytes(data.memory_available)));
    rows.push(kv_row(
        "Swap used",
        format!(
            "{} / {}",
            human_bytes(data.swap_used),
            human_bytes(data.swap_total)
        ),
    ));
    rows.push(kv_row(
        "Host uptime",
        crate::cli::daemon::format_uptime(data.host_uptime_seconds),
    ));
    match &data.daemon {
        Some(status) => {
            rows.push(kv_row("Daemon PID", status.pid.to_string()));
            rows.push(kv_row("Mode", status.mode.clone()));
            rows.push(kv_row(
                "Daemon uptime",
                crate::cli::daemon::format_uptime(daemon_uptime_seconds(status)),
            ));
            rows.push(kv_row("Listeners", status.listeners.join(", ")));
            rows.push(kv_row("Workers", status.workers.to_string()));
            rows.push(kv_row(
                "Paused",
                if status.paused { "yes".to_string() } else { "no".to_string() },
            ));
            rows.push(kv_row(
                "Maintenance",
                if status.maintenance {
                    "on".to_string()
                } else {
                    "off".to_string()
                },
            ));
        }
        None => {
            rows.push(kv_row("Daemon", "not running".to_string()));
        }
    }
    frame.render_widget(info_table(" Server Info ", rows), chunks[0]);

    let memory_percent = if data.memory_total > 0 {
        (data.memory_used.saturating_mul(100) / data.memory_total) as u16
    } else {
        0
    };
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(" Memory "))
        .gauge_style(Style::default().fg(Color::Green))
        .percent(memory_percent)
        .label(format!(
            "{} / {}",
            human_bytes(data.memory_used),
            human_bytes(data.memory_total)
        ));
    frame.render_widget(gauge, chunks[1]);
}

fn draw_network(frame: &mut Frame, area: ratatui::layout::Rect, data: &MonitorData) {
    if data.networks.is_empty() {
        let paragraph = Paragraph::new("No network interfaces found.")
            .block(Block::default().borders(Borders::ALL).title(" Network Monitor "));
        frame.render_widget(paragraph, area);
        return;
    }
    let rows: Vec<Row> = data
        .networks
        .iter()
        .map(|network| {
            Row::new(vec![
                Cell::from(network.name.clone()),
                Cell::from(format!("{}/s", human_bytes(network.rx_per_sec))),
                Cell::from(format!("{}/s", human_bytes(network.tx_per_sec))),
                Cell::from(human_bytes(network.rx_total)),
                Cell::from(human_bytes(network.tx_total)),
                Cell::from(network.packets_rx.to_string()),
                Cell::from(network.packets_tx.to_string()),
                Cell::from((network.errors_rx + network.errors_tx).to_string()),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(14),
            Constraint::Min(10),
            Constraint::Min(10),
            Constraint::Min(10),
            Constraint::Min(10),
            Constraint::Min(9),
            Constraint::Min(9),
            Constraint::Length(7),
        ],
    )
    .header(Row::new(vec![
        Cell::from("Interface"),
        Cell::from("RX / sec"),
        Cell::from("TX / sec"),
        Cell::from("Total RX"),
        Cell::from("Total TX"),
        Cell::from("Pkts RX"),
        Cell::from("Pkts TX"),
        Cell::from("Errors"),
    ]))
    .block(Block::default().borders(Borders::ALL).title(" Network Monitor "));
    frame.render_widget(table, area);
}

fn draw_processes(frame: &mut Frame, area: ratatui::layout::Rect, data: &MonitorData) {
    if data.processes.is_empty() {
        let paragraph = Paragraph::new("No processes found.")
            .block(Block::default().borders(Borders::ALL).title(" Process Monitor "));
        frame.render_widget(paragraph, area);
        return;
    }
    let rows: Vec<Row> = data
        .processes
        .iter()
        .map(|process| {
            Row::new(vec![
                Cell::from(process.pid.to_string()),
                Cell::from(process.name.clone()),
                Cell::from(process.status.clone()),
                Cell::from(process.user_id.clone().unwrap_or_else(|| "-".to_string())),
                Cell::from(format!("{:.1}%", process.cpu)),
                Cell::from(human_bytes(process.memory)),
                Cell::from(human_bytes(process.virtual_memory)),
                Cell::from(human_bytes(process.disk_read_bytes)),
                Cell::from(human_bytes(process.disk_written_bytes)),
                Cell::from(human_duration(process.run_time_seconds)),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Min(16),
            Constraint::Length(4),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(8),
        ],
    )
    .header(Row::new(vec![
        Cell::from("PID"),
        Cell::from("Name"),
        Cell::from("St"),
        Cell::from("User"),
        Cell::from("CPU"),
        Cell::from("Memory"),
        Cell::from("Virt"),
        Cell::from("Disk Read"),
        Cell::from("Disk Write"),
        Cell::from("Time"),
    ]))
    .block(Block::default().borders(Borders::ALL).title(" Process Monitor "));
    frame.render_widget(table, area);
}

fn draw_disks(frame: &mut Frame, area: ratatui::layout::Rect, data: &MonitorData) {
    if data.disks.is_empty() {
        let paragraph = Paragraph::new("No disks found.")
            .block(Block::default().borders(Borders::ALL).title(" Disk Monitor "));
        frame.render_widget(paragraph, area);
        return;
    }
    let rows: Vec<Row> = data
        .disks
        .iter()
        .map(|disk| {
            let used = disk.total.saturating_sub(disk.available);
            let percent = if disk.total > 0 {
                used.saturating_mul(100) / disk.total
            } else {
                0
            };
            Row::new(vec![
                Cell::from(disk.name.clone()),
                Cell::from(disk.mount.clone()),
                Cell::from(disk.file_system.clone()),
                Cell::from(disk.kind.clone()),
                Cell::from(human_bytes(disk.total)),
                Cell::from(human_bytes(used)),
                Cell::from(human_bytes(disk.available)),
                Cell::from(format!("{percent}%")),
                Cell::from(format!("{}/s", human_bytes(disk.read_bytes_per_sec))),
                Cell::from(format!("{}/s", human_bytes(disk.written_bytes_per_sec))),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Min(10),
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Length(10),
        ],
    )
    .header(Row::new(vec![
        Cell::from("Device"),
        Cell::from("Mount"),
        Cell::from("FS"),
        Cell::from("Kind"),
        Cell::from("Total"),
        Cell::from("Used"),
        Cell::from("Free"),
        Cell::from("Use"),
        Cell::from("Read /s"),
        Cell::from("Write /s"),
    ]))
    .block(Block::default().borders(Borders::ALL).title(" Disk Monitor "));
    frame.render_widget(table, area);
}

fn draw_app_usage(frame: &mut Frame, area: ratatui::layout::Rect, data: &MonitorData) {
    let Some(status) = &data.daemon else {
        frame.render_widget(not_running_paragraph(" App Usage "), area);
        return;
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(7), Constraint::Min(0)])
        .split(area);

    let summary = vec![
        kv_row("Hook events", status.hook_events.to_string()),
        kv_row("Geo lookups", status.geo_lookups.to_string()),
        kv_row("Total requests", status.total_requests.to_string()),
        kv_row("Modules reporting", status.modules.len().to_string()),
    ];
    frame.render_widget(info_table(" App Usage ", summary), chunks[0]);

    if status.modules.is_empty() {
        let paragraph = Paragraph::new(
            "No module statistics reported by the daemon yet.\nThe server updates per-module counts in the status file while it runs.",
        )
        .block(Block::default().borders(Borders::ALL).title(" Modules "))
        .wrap(Wrap { trim: true });
        frame.render_widget(paragraph, chunks[1]);
        return;
    }
    let rows: Vec<Row> = status
        .modules
        .iter()
        .map(|module| {
            Row::new(vec![
                Cell::from(module.id.clone()),
                Cell::from(module.requests.to_string()),
            ])
        })
        .collect();
    let table = Table::new(rows, [Constraint::Min(20), Constraint::Length(16)])
        .header(Row::new(vec![
            Cell::from("Module"),
            Cell::from("Requests"),
        ]))
        .block(Block::default().borders(Borders::ALL).title(" Modules "));
    frame.render_widget(table, chunks[1]);
}

/// Format a byte count as a human-readable string, for example `1.5 GB`.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Format a duration in seconds compactly, for example `3d 4h`, `12m 5s`.
fn human_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let secs = seconds % 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {secs}s")
    } else {
        format!("{secs}s")
    }
}
