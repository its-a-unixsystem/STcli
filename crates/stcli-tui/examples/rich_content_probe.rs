//! Ticket-01 disposable rich-content terminal experiment.
//! This example is not a supported STcli API.

#[path = "../src/terminal.rs"]
mod terminal;
use anyhow::{Context, Result, anyhow, bail};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use serde::{Deserialize, Serialize};
use std::{
    env,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use terminal::TerminalSession;

const HELPER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/rich_content_probe/renderer.py"
);
const CARD_SOURCE: &str = include_str!("rich_content_probe/fixtures/card.html");
const COLUMNS_SOURCE: &str = include_str!("rich_content_probe/fixtures/columns.html");
const LITERAL_SOURCE: &str = include_str!("rich_content_probe/fixtures/literal.md");
const MAX_RESPONSE: u64 = 48 * 1024 * 1024;
const MAX_PNG_BASE64: usize = 32 * 1024 * 1024;
const IMAGE_ID: u32 = 0x5354_0101;
const PLACEMENT_ID: u32 = 0x5354_0102;

#[derive(Clone, Copy, PartialEq, Eq)]
enum GraphicsMode {
    Auto,
    Off,
}

struct Options {
    graphics: GraphicsMode,
    renderer: PathBuf,
    evidence: PathBuf,
}

#[derive(Debug, Deserialize)]
struct ProbeReply {
    supported: bool,
    cell_width: u32,
    cell_height: u32,
    reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fixture {
    Card,
    Columns,
    Literal,
}

impl Fixture {
    fn worker_name(self) -> Option<&'static str> {
        match self {
            Self::Card => Some("card"),
            Self::Columns => Some("columns"),
            Self::Literal => None,
        }
    }

    fn source(self) -> &'static str {
        match self {
            Self::Card => CARD_SOURCE,
            Self::Columns => COLUMNS_SOURCE,
            Self::Literal => LITERAL_SOURCE,
        }
    }

    fn alternative(self) -> &'static str {
        match self {
            Self::Card => {
                "Harbor Watch\nNight signal report\nThe lighthouse keeper records a calm sea, a steady western wind, and three vessels safely inside the breakwater. Static HTML and CSS remain readable without scripts or remote resources."
            }
            Self::Columns => {
                "Harbor Operations Board\nNorth Pier — cargo manifests, mooring lines, and navigation lamps.\nInner Harbor — ferries rotate through the sheltered basin.\nBreakwater — signal flags, wave height, and beacon power."
            }
            Self::Literal => LITERAL_SOURCE,
        }
    }
}

#[derive(Clone, Serialize)]
struct RenderRequest {
    id: u64,
    fixture: &'static str,
    width: u32,
}

#[derive(Debug, Deserialize)]
struct WorkerReply {
    id: u64,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    png_base64: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

struct RenderedImage {
    id: u64,
    width: u32,
    height: u32,
    png_base64: String,
    transmitted: bool,
}

enum WorkerEvent {
    Rendered(RenderedImage),
    Failed { id: u64, reason: String },
    IdleStopped,
}

struct RequestQueue {
    pending: Option<RenderRequest>,
    stopped: bool,
}

struct WorkerHandle {
    queue: Arc<(Mutex<RequestQueue>, Condvar)>,
    events: Receiver<WorkerEvent>,
    join: Option<thread::JoinHandle<()>>,
}

impl WorkerHandle {
    fn start(renderer: PathBuf) -> Self {
        let queue = Arc::new((
            Mutex::new(RequestQueue {
                pending: None,
                stopped: false,
            }),
            Condvar::new(),
        ));
        let (event_tx, event_rx) = mpsc::channel();
        let worker_queue = Arc::clone(&queue);
        let join = thread::spawn(move || worker_thread(worker_queue, event_tx, renderer));
        Self {
            queue,
            events: event_rx,
            join: Some(join),
        }
    }

    fn request(&self, request: RenderRequest) -> Result<()> {
        let (lock, ready) = &*self.queue;
        let mut queue = lock
            .lock()
            .map_err(|_| anyhow!("renderer request queue was poisoned"))?;
        if queue.stopped {
            bail!("renderer worker thread has stopped");
        }
        queue.pending = Some(request);
        ready.notify_one();
        Ok(())
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        let (lock, ready) = &*self.queue;
        if let Ok(mut queue) = lock.lock() {
            queue.stopped = true;
            queue.pending = None;
            ready.notify_one();
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct ScopedWorker {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    unit: String,
    renders: u32,
}

impl ScopedWorker {
    fn launch(renderer: &Path) -> Result<Self> {
        if !renderer.is_file() {
            bail!("renderer executable is missing: {}", renderer.display());
        }
        if !Path::new(HELPER).is_file() {
            bail!("renderer helper is missing: {HELPER}");
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let unit = format!("stcli-rich-probe-{}-{nonce}", std::process::id());
        let mut child = Command::new("systemd-run")
            .args([
                "--user",
                "--scope",
                "--quiet",
                &format!("--unit={unit}"),
                "-p",
                "MemoryMax=1G",
                "-p",
                "MemorySwapMax=0",
                "-p",
                "TasksMax=256",
                "-p",
                "CPUQuota=200%",
                "python3",
                HELPER,
                "--worker",
                "--renderer",
            ])
            .arg(renderer)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to launch renderer in transient user scope")?;
        let input = child.stdin.take().context("renderer stdin was not piped")?;
        let output = BufReader::new(
            child
                .stdout
                .take()
                .context("renderer stdout was not piped")?,
        );
        Ok(Self {
            child,
            input: Some(input),
            output,
            unit,
            renders: 0,
        })
    }

    fn render(&mut self, request: &RenderRequest) -> Result<RenderedImage> {
        let input = self
            .input
            .as_mut()
            .context("renderer input pipe is closed")?;
        serde_json::to_writer(&mut *input, request).context("failed to encode render request")?;
        input.write_all(b"\n")?;
        input.flush()?;

        let completed = Arc::new(AtomicBool::new(false));
        let watchdog_completed = Arc::clone(&completed);
        let watchdog_unit = self.unit.clone();
        let timeout = if self.renders == 0 {
            Duration::from_secs(10)
        } else {
            Duration::from_secs(3)
        };
        thread::spawn(move || {
            thread::sleep(timeout);
            if !watchdog_completed.load(Ordering::Acquire) {
                let _ = Command::new("systemctl")
                    .args([
                        "--user",
                        "kill",
                        "--kill-whom=all",
                        "--signal=KILL",
                        &watchdog_unit,
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        });

        let mut bytes = Vec::new();
        let mut limited = self.output.by_ref().take(MAX_RESPONSE + 1);
        let count = limited.read_until(b'\n', &mut bytes)?;
        if count == 0 {
            bail!("renderer closed its response pipe");
        }
        if count as u64 > MAX_RESPONSE || !bytes.ends_with(b"\n") {
            bail!("renderer response exceeded the 48 MiB framing limit");
        }
        let reply: WorkerReply =
            serde_json::from_slice(&bytes).context("renderer returned invalid JSON")?;
        if reply.id != request.id {
            bail!("renderer response ID did not match its request");
        }
        if let Some(error) = reply.error {
            bail!("{}", filter_diagnostic(&error));
        }
        let width = reply.width.context("renderer response omitted width")?;
        completed.store(true, Ordering::Release);
        self.renders = self.renders.saturating_add(1);
        let height = reply.height.context("renderer response omitted height")?;
        let png_base64 = reply.png_base64.context("renderer response omitted PNG")?;
        if width != request.width || width == 0 || width > 1600 || height == 0 || height > 4096 {
            bail!("renderer returned invalid image dimensions {width}x{height}");
        }
        if png_base64.len() > MAX_PNG_BASE64 || !png_base64.bytes().all(is_base64_byte) {
            bail!("renderer returned an invalid or oversized PNG payload");
        }
        Ok(RenderedImage {
            id: request.id,
            width,
            height,
            png_base64,
            transmitted: false,
        })
    }

    fn stop(&mut self) {
        self.input.take();
        let deadline = Instant::now() + Duration::from_millis(750);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => thread::sleep(Duration::from_millis(25)),
                Err(_) => break,
            }
        }
        let _ = Command::new("systemctl")
            .args([
                "--user",
                "kill",
                "--kill-whom=all",
                "--signal=KILL",
                &self.unit,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ScopedWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn worker_thread(
    queue: Arc<(Mutex<RequestQueue>, Condvar)>,
    events: Sender<WorkerEvent>,
    renderer: PathBuf,
) {
    let mut worker: Option<ScopedWorker> = None;
    loop {
        let request = {
            let (lock, ready) = &*queue;
            let mut state = match lock.lock() {
                Ok(state) => state,
                Err(_) => return,
            };
            while state.pending.is_none() && !state.stopped {
                let waited = ready.wait_timeout(state, Duration::from_secs(30));
                let Ok((next, timeout)) = waited else { return };
                state = next;
                if timeout.timed_out() && state.pending.is_none() {
                    drop(state);
                    if worker.take().is_some() {
                        let _ = events.send(WorkerEvent::IdleStopped);
                    }
                    state = match lock.lock() {
                        Ok(state) => state,
                        Err(_) => return,
                    };
                }
            }
            if state.stopped {
                return;
            }
            state.pending.take().expect("pending request was checked")
        };

        if worker.is_none() {
            match ScopedWorker::launch(&renderer) {
                Ok(started) => worker = Some(started),
                Err(error) => {
                    let _ = events.send(WorkerEvent::Failed {
                        id: request.id,
                        reason: filter_diagnostic(&format!("{error:#}")),
                    });
                    return;
                }
            }
        }
        match worker
            .as_mut()
            .expect("worker was initialized")
            .render(&request)
        {
            Ok(image) => {
                if events.send(WorkerEvent::Rendered(image)).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = events.send(WorkerEvent::Failed {
                    id: request.id,
                    reason: filter_diagnostic(&format!("{error:#}")),
                });
                return;
            }
        }
    }
}

struct App {
    mode: GraphicsMode,
    capability: ProbeReply,
    fixture: Fixture,
    removed: bool,
    popup: bool,
    source: bool,
    scroll_rows: u32,
    next_id: u64,
    current_id: Option<u64>,
    requested_width: Option<u32>,
    image: Option<RenderedImage>,
    fallback_reason: String,
    placement_visible: bool,
    renderer_failed: bool,
    worker: Option<WorkerHandle>,
    pane: Rect,
    renderer: PathBuf,
    evidence: PathBuf,
    evidence_written: bool,
}

impl App {
    fn new(options: Options, capability: ProbeReply) -> Self {
        let reason = if options.graphics == GraphicsMode::Off {
            "graphics disabled by --graphics off".to_owned()
        } else {
            filter_diagnostic(&capability.reason)
        };
        Self {
            mode: options.graphics,
            capability,
            fixture: Fixture::Card,
            removed: false,
            popup: false,
            source: false,
            scroll_rows: 0,
            next_id: 1,
            current_id: None,
            requested_width: None,
            image: None,
            fallback_reason: reason,
            placement_visible: false,
            renderer_failed: false,
            worker: None,
            pane: Rect::default(),
            renderer: options.renderer,
            evidence: options.evidence,
            evidence_written: false,
        }
    }

    fn graphics_available(&self) -> bool {
        self.mode == GraphicsMode::Auto
            && self.capability.supported
            && self.capability.cell_width > 0
            && self.capability.cell_height > 0
    }

    fn invalidate(&mut self, delete_image: bool) {
        self.current_id = None;
        self.requested_width = None;
        if delete_image {
            self.delete_owned_image();
            self.image = None;
        } else {
            self.hide_placement();
        }
    }

    fn select(&mut self, fixture: Fixture) {
        self.invalidate(true);
        self.fixture = fixture;
        self.popup = false;
        self.source = false;
        self.scroll_rows = 0;
        self.fallback_reason = if self.graphics_available() {
            "waiting for isolated renderer".to_owned()
        } else if self.mode == GraphicsMode::Off {
            "graphics disabled by --graphics off".to_owned()
        } else {
            filter_diagnostic(&self.capability.reason)
        };
    }

    fn request_for_pane(&mut self) {
        if !self.graphics_available()
            || self.fixture.worker_name().is_none()
            || self.removed
            || self.popup
            || self.source
            || self.renderer_failed
            || self.pane.width == 0
        {
            return;
        }
        let width = u32::from(self.pane.width)
            .saturating_mul(self.capability.cell_width)
            .clamp(1, 1600);
        if self.requested_width == Some(width)
            && (self.current_id.is_some() || self.image.is_some())
        {
            return;
        }
        self.delete_owned_image();
        self.image = None;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let id = self.next_id;
        self.current_id = Some(id);
        self.requested_width = Some(width);
        let request = RenderRequest {
            id,
            fixture: self
                .fixture
                .worker_name()
                .expect("checked graphical fixture"),
            width,
        };
        if self.worker.is_none() {
            self.worker = Some(WorkerHandle::start(self.renderer.clone()));
        }
        if let Err(error) = self
            .worker
            .as_ref()
            .expect("worker exists")
            .request(request)
        {
            self.current_id = None;
            self.fallback_reason = filter_diagnostic(&error.to_string());
            self.worker = None;
        } else {
            self.fallback_reason = "rendering…".to_owned();
        }
    }

    fn drain_worker(&mut self) {
        let mut retire = false;
        let Some(worker) = self.worker.as_ref() else {
            return;
        };
        while let Ok(event) = worker.events.try_recv() {
            match event {
                WorkerEvent::Rendered(image) if Some(image.id) == self.current_id => {
                    #[allow(clippy::collapsible_if)]
                    if !self.evidence_written {
                        if let Ok(bytes) = decode_base64(&image.png_base64) {
                            let _ = std::fs::create_dir_all(&self.evidence);
                            let _ = std::fs::write(self.evidence.join("rendered-card.png"), bytes);
                            self.evidence_written = true;
                        }
                    }
                    self.fallback_reason.clear();
                    self.current_id = None;
                    self.image = Some(image);
                }
                WorkerEvent::Rendered(_) => {}
                WorkerEvent::Failed { id, reason } if Some(id) == self.current_id => {
                    self.current_id = None;
                    self.image = None;
                    self.fallback_reason = reason;
                    self.renderer_failed = true;
                    retire = true;
                }
                WorkerEvent::Failed { .. } => {
                    self.renderer_failed = true;
                    retire = true;
                }
                WorkerEvent::IdleStopped => {}
            }
        }
        if retire {
            self.worker = None;
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return true;
        }
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('1') => self.select(Fixture::Card),
            KeyCode::Char('2') => self.select(Fixture::Columns),
            KeyCode::Char('3') => self.select(Fixture::Literal),
            KeyCode::Up | KeyCode::PageUp => {
                let amount = if key.code == KeyCode::PageUp {
                    u32::from(self.pane.height.max(1))
                } else {
                    1
                };
                self.scroll_rows = self.scroll_rows.saturating_sub(amount);
                self.hide_placement();
            }
            KeyCode::Down | KeyCode::PageDown => {
                let amount = if key.code == KeyCode::PageDown {
                    u32::from(self.pane.height.max(1))
                } else {
                    1
                };
                let limit = if self.image.is_some() {
                    self.max_scroll()
                } else {
                    4096
                };
                self.scroll_rows = self.scroll_rows.saturating_add(amount).min(limit);
                self.hide_placement();
            }
            KeyCode::Char('p') => {
                self.popup = !self.popup;
                self.hide_placement();
            }
            KeyCode::Char('s') => {
                self.source = !self.source;
                self.hide_placement();
            }
            KeyCode::Char('d') => {
                self.removed = true;
                self.invalidate(true);
            }
            KeyCode::Char('r') => {
                self.removed = false;
                self.popup = false;
                self.source = false;
                self.scroll_rows = 0;
                self.invalidate(true);
            }
            _ => {}
        }
        false
    }

    fn max_scroll(&self) -> u32 {
        let Some(image) = self.image.as_ref() else {
            return 0;
        };
        let visible = u32::from(self.pane.height).saturating_mul(self.capability.cell_height);
        image
            .height
            .saturating_sub(visible)
            .div_ceil(self.capability.cell_height.max(1))
    }

    fn hide_placement(&mut self) {
        if self.placement_visible {
            kitty_command(&format!("a=d,d=p,i={IMAGE_ID},p={PLACEMENT_ID},q=2"), None);
            self.placement_visible = false;
        }
    }

    fn delete_owned_image(&mut self) {
        if self.image.is_some() || self.placement_visible {
            kitty_command(&format!("a=d,d=I,i={IMAGE_ID},q=2"), None);
        }
        self.placement_visible = false;
    }

    fn place_image(&mut self) {
        if self.popup || self.source || self.removed || self.fixture == Fixture::Literal {
            self.hide_placement();
            return;
        }
        let max_scroll = self.max_scroll();
        self.scroll_rows = self.scroll_rows.min(max_scroll);
        let Some(image) = self.image.as_mut() else {
            return;
        };
        if !image.transmitted {
            transmit_png(IMAGE_ID, &image.png_base64);
            image.transmitted = true;
        }
        let crop_y = self.scroll_rows.saturating_mul(self.capability.cell_height);
        let pane_pixel_height =
            u32::from(self.pane.height).saturating_mul(self.capability.cell_height);
        let crop_h = pane_pixel_height.min(image.height.saturating_sub(crop_y));
        let crop_w = u32::from(self.pane.width)
            .saturating_mul(self.capability.cell_width)
            .min(image.width);
        if crop_w == 0 || crop_h == 0 {
            self.hide_placement();
            return;
        }
        move_cursor(self.pane.x, self.pane.y);
        kitty_command(
            &format!(
                "a=p,i={IMAGE_ID},p={PLACEMENT_ID},q=2,C=1,z=-1,c={},x=0,y={crop_y},w={crop_w},h={crop_h}",
                self.pane.width
            ),
            None,
        );
        self.placement_visible = true;
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.delete_owned_image();
        self.current_id = None;
        self.worker = None;
    }
}

fn main() -> Result<()> {
    let options = parse_options()?;
    let capability = if options.graphics == GraphicsMode::Off {
        ProbeReply {
            supported: false,
            cell_width: 0,
            cell_height: 0,
            reason: "graphics disabled by --graphics off".to_owned(),
        }
    } else {
        run_capability_probe().unwrap_or_else(|error| ProbeReply {
            supported: false,
            cell_width: 0,
            cell_height: 0,
            reason: filter_diagnostic(&format!("terminal capability probe failed: {error:#}")),
        })
    };
    let mut app = App::new(options, capability);
    let mut terminal = TerminalSession::enter()?;
    let result = run_loop(terminal.terminal(), &mut app);
    app.delete_owned_image();
    result
}

fn run_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        app.drain_worker();
        let mut pane = Rect::default();
        terminal.draw(|frame| pane = draw(frame, app))?;
        if pane != app.pane {
            if app.pane.width != pane.width {
                app.requested_width = None;
                app.current_id = None;
                app.image = None;
                app.delete_owned_image();
            }
            app.pane = pane;
        }
        app.request_for_pane();
        app.place_image();

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press && app.handle_key(key) => {
                return Ok(());
            }
            Event::Resize(_, _) => {
                app.invalidate(true);
            }
            Event::Key(_)
            | Event::Mouse(_)
            | Event::FocusGained
            | Event::FocusLost
            | Event::Paste(_) => {}
        }
    }
}

fn draw(frame: &mut Frame<'_>, app: &App) -> Rect {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(2),
        ])
        .split(frame.area());
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "Rich-content renderer probe",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  · native Ratatui controls"),
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("ticket 01 experiment"),
        ),
        chunks[0],
    );
    let pane = chunks[1].inner(ratatui::layout::Margin {
        horizontal: 1,
        vertical: 1,
    });
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(match app.fixture {
                Fixture::Card => "1 Card",
                Fixture::Columns => "2 Responsive columns",
                Fixture::Literal => "3 Literal Markdown",
            }),
        chunks[1],
    );

    if app.removed {
        frame.render_widget(Paragraph::new("Content removed. Press r to restore."), pane);
    } else if app.source {
        let source = filter_terminal_text(app.fixture.source());
        frame.render_widget(
            Paragraph::new(source)
                .wrap(Wrap { trim: false })
                .scroll((app.scroll_rows.min(u32::from(u16::MAX)) as u16, 0)),
            pane,
        );
    } else if app.fixture == Fixture::Literal || app.image.is_none() {
        let mut text = Text::from(filter_terminal_text(app.fixture.alternative()));
        if app.fixture != Fixture::Literal && !app.fallback_reason.is_empty() {
            text.lines.push(Line::default());
            text.lines.push(Line::styled(
                format!("Graphical view unavailable: {}", app.fallback_reason),
                Style::default().fg(Color::Yellow),
            ));
        }
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .scroll((app.scroll_rows.min(u32::from(u16::MAX)) as u16, 0)),
            pane,
        );
    }

    frame.render_widget(
        Paragraph::new("1/2/3 fixture  ↑/↓ PgUp/PgDn scroll  p popup  s source  d remove  r restore  q/Ctrl-C quit")
            .alignment(Alignment::Center),
        chunks[2],
    );

    if app.popup {
        let area = centered(frame.area(), 54, 7);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new("Ratatui popup\n\nThe graphical placement is removed while this overlay is open.\nPress p to close.")
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true })
                .block(Block::default().borders(Borders::ALL).title("Native overlay")),
            area,
        );
    }
    pane
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn parse_options() -> Result<Options> {
    let mut graphics = GraphicsMode::Auto;
    let mut renderer = PathBuf::from("/usr/lib/chromium/chromium");
    let mut evidence = env::temp_dir().join("stcli-rich-content-probe");
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--graphics" => match args.next().as_deref() {
                Some("auto") => graphics = GraphicsMode::Auto,
                Some("off") => graphics = GraphicsMode::Off,
                Some(value) => bail!("invalid --graphics value {value:?}; expected auto or off"),
                None => bail!("--graphics requires auto or off"),
            },
            "--renderer" => {
                renderer = PathBuf::from(args.next().context("--renderer requires a path")?)
            }
            "--evidence" => {
                evidence = PathBuf::from(args.next().context("--evidence requires a directory")?)
            }
            "-h" | "--help" => {
                println!(
                    "Usage: rich_content_probe [--graphics auto|off] [--renderer PATH] [--evidence DIR]"
                );
                std::process::exit(0);
            }
            _ => bail!("unknown argument {arg:?}"),
        }
    }
    Ok(Options {
        graphics,
        renderer,
        evidence,
    })
}

fn run_capability_probe() -> Result<ProbeReply> {
    if !Path::new(HELPER).is_file() {
        bail!("renderer helper is missing: {HELPER}");
    }
    let mut child = Command::new("python3")
        .args([HELPER, "--terminal-probe"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to start terminal capability probe")?;
    let stdout = child
        .stdout
        .take()
        .context("capability probe stdout was not piped")?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = BufReader::new(stdout)
            .take(16 * 1024)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = sender.send(result);
    });
    let bytes = match receiver.recv_timeout(Duration::from_millis(750)) {
        Ok(result) => result?,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("terminal capability probe timed out");
        }
    };
    let status = child.wait()?;
    if !status.success() {
        bail!("terminal capability probe exited unsuccessfully");
    }
    let reply: ProbeReply =
        serde_json::from_slice(&bytes).context("terminal probe returned invalid JSON")?;
    if reply.cell_width > 1024 || reply.cell_height > 1024 {
        bail!("terminal probe returned implausible cell dimensions");
    }
    Ok(reply)
}

fn transmit_png(image_id: u32, png_base64: &str) {
    let mut chunks = png_base64.as_bytes().chunks(4096).peekable();
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        let parameters = format!("a=t,t=d,f=100,i={image_id},q=2,m={more}");
        kitty_command(&parameters, Some(chunk));
    }
}

fn kitty_command(parameters: &str, payload: Option<&[u8]>) {
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(b"\x1b_G");
    let _ = stdout.write_all(parameters.as_bytes());
    if let Some(payload) = payload {
        let _ = stdout.write_all(b";");
        let _ = stdout.write_all(payload);
    }
    let _ = stdout.write_all(b"\x1b\\");
    let _ = stdout.flush();
}

fn move_cursor(x: u16, y: u16) {
    let mut stdout = io::stdout().lock();
    let _ = write!(
        stdout,
        "\x1b[{};{}H",
        y.saturating_add(1),
        x.saturating_add(1)
    );
    let _ = stdout.flush();
}

fn filter_terminal_text(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '\n' | '\t' => character,
            character if !character.is_control() => character,
            _ => '�',
        })
        .collect()
}

fn filter_diagnostic(value: &str) -> String {
    filter_terminal_text(value).chars().take(512).collect()
}

fn is_base64_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')
}

fn decode_base64(value: &str) -> Result<Vec<u8>> {
    let mut output = Vec::with_capacity(value.len() * 3 / 4);
    let mut quartet = [0u8; 4];
    let mut count = 0;
    for byte in value.bytes() {
        quartet[count] = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => 64,
            _ => bail!("invalid base64 evidence payload"),
        };
        count += 1;
        if count == 4 {
            output.push((quartet[0] << 2) | (quartet[1] >> 4));
            if quartet[2] != 64 {
                output.push((quartet[1] << 4) | (quartet[2] >> 2));
            }
            if quartet[3] != 64 {
                output.push((quartet[2] << 6) | quartet[3]);
            }
            count = 0;
        }
    }
    if count != 0 {
        bail!("incomplete base64 evidence payload");
    }
    Ok(output)
}
