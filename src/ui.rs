use crate::{
    config::{Agent, Config},
    events::Event,
};
use anyhow::Result;
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event as Input, KeyCode, KeyEvent,
        KeyEventKind, KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph},
};
use std::{
    collections::{HashMap, VecDeque},
    io::{self, Stdout},
    path::Path,
    time::{Duration, Instant},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// Neutral surfaces and thin separators. Operational statuses also have text labels.
const BG: Color = Color::Rgb(18, 19, 22);
const PANEL: Color = Color::Rgb(18, 19, 22);
const FG: Color = Color::Rgb(231, 233, 244);
const MUTED: Color = Color::Rgb(158, 165, 190);
const BORDER: Color = Color::Rgb(66, 70, 78);
const PURPLE: Color = Color::Rgb(218, 220, 226);
const BLUE: Color = Color::Rgb(131, 177, 255);
const GREEN: Color = Color::Rgb(130, 218, 178);
const AMBER: Color = Color::Rgb(244, 202, 128);
const RED: Color = Color::Rgb(255, 146, 161);

pub struct Screen {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}
impl Screen {
    pub fn enter() -> Result<Self> {
        enable_raw_mode()?;
        if let Err(e) = execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste) {
            let _ = disable_raw_mode();
            return Err(e.into());
        }
        match Terminal::new(CrosstermBackend::new(io::stdout())) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(e) => {
                let _ = disable_raw_mode();
                let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
                Err(e.into())
            }
        }
    }
    pub fn draw(&mut self, state: &View, config: &Config, goal: &str, demo: bool) -> Result<()> {
        self.terminal
            .draw(|frame| render(frame, state, config, goal, demo))?;
        Ok(())
    }
    pub fn configure(&mut self, config: &mut Config, path: &Path) -> Result<()> {
        crate::setup::run(&mut self.terminal, config, path)
    }
    pub fn configure_agent(
        &mut self,
        config: &mut Config,
        path: &Path,
        selected: usize,
        live_config: &std::sync::Arc<std::sync::RwLock<Config>>,
    ) -> Result<()> {
        crate::setup::run_at(
            &mut self.terminal,
            config,
            path,
            selected,
            Some(live_config),
        )
    }
    pub fn authorize_full_access(&mut self, project: &Path) -> Result<bool> {
        let mut response = String::new();
        loop {
            self.terminal
                .draw(|f| render_permission_consent(f, project, &response))?;
            if let Input::Key(key) = event::read()?
                && key.kind != KeyEventKind::Release
            {
                match key.code {
                    KeyCode::Esc => return Ok(false),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(false);
                    }
                    KeyCode::Enter if response == "ALLOW" => return Ok(true),
                    KeyCode::Backspace => {
                        response.pop();
                    }
                    KeyCode::Char(c) if c.is_ascii_alphabetic() && response.len() < 5 => {
                        response.push(c.to_ascii_uppercase())
                    }
                    _ => {}
                }
            }
        }
    }
    pub fn onboard(&mut self, project: &Path) -> Result<bool> {
        loop {
            self.terminal.draw(|f| render_onboarding(f, project))?;
            if let Input::Key(key) = event::read()?
                && key.kind != KeyEventKind::Release
            {
                match key.code {
                    KeyCode::Enter => return Ok(true),
                    KeyCode::Esc => return Ok(false),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(false);
                    }
                    _ => {}
                }
            }
        }
    }
    pub fn launch(
        &mut self,
        config: &mut Config,
        project: &Path,
        path: &Path,
    ) -> Result<Option<Launch>> {
        let mut launch = Launch {
            goal: String::new(),
            mode: 2,
            resume: false,
        };
        loop {
            self.terminal
                .draw(|f| render_launcher(f, config, project, &launch))?;
            let input = event::read()?;
            if let Input::Paste(text) = &input {
                launch.goal.push_str(&safe(text).replace('\n', " "));
                continue;
            }
            if let Input::Key(key) = input
                && key.kind != KeyEventKind::Release
            {
                match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(None);
                    }
                    KeyCode::Enter if !launch.goal.trim().is_empty() => return Ok(Some(launch)),
                    KeyCode::F(2) => launch.mode = (launch.mode + 1) % 3,
                    KeyCode::F(4) => {
                        if let Some(saved) =
                            crate::memory::Store::load(project, launch.mode == 0)?.pending(None)
                        {
                            launch.goal = saved.goal;
                            launch.resume = true;
                            return Ok(Some(launch));
                        }
                    }
                    KeyCode::F(5) => {
                        let _lock = crate::engine::lock(project)?;
                        config.permissions =
                            if config.permissions == crate::config::Permissions::FullAccess {
                                crate::config::Permissions::Restricted
                            } else {
                                crate::config::Permissions::FullAccess
                            };
                        if config.permissions == crate::config::Permissions::Restricted {
                            crate::permissions::revoke(project)?;
                        }
                        config.save(path)?;
                    }
                    KeyCode::F(3) => {
                        let _lock = crate::engine::lock(project)?;
                        self.configure(config, path)?;
                    }
                    KeyCode::Backspace => {
                        launch.goal.pop();
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        launch.goal.clear()
                    }
                    KeyCode::Char(c)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                            && !c.is_control() =>
                    {
                        launch.goal.push(c)
                    }
                    _ => {}
                }
            }
        }
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

pub struct Launch {
    pub goal: String,
    pub mode: usize,
    pub resume: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Conversation,
    Tasks,
    Profile,
    Memory,
}

pub enum Action {
    Send(String),
    Configure,
}

pub struct View {
    pub action: Option<Action>,
    pub composing: bool,
    pub draft: crate::setup::Text,
    pub notice: String,
    pub selected: usize,
    pub all: bool,
    pub scroll: u16,
    pub round: String,
    pub round_limit: Option<usize>,
    pub status: String,
    pub states: HashMap<String, String>,
    pub events: VecDeque<Event>,
    pub tab: Tab,
    pub help: bool,
    tasks: HashMap<String, String>,
    reports: HashMap<String, String>,
    report_count: usize,
    message_count: usize,
    error_count: usize,
    first_time: Option<u64>,
    last_time: u64,
    agent_logs: HashMap<String, VecDeque<Event>>,
    memory_maps: HashMap<String, String>,
    shared_memory: String,
    pub motion: bool,
    elapsed: Duration,
    started: Instant,
    log_capacity: usize,
    total_events: usize,
    agent_counts: HashMap<String, usize>,
    activity: HashMap<String, VecDeque<Duration>>,
    working_since: HashMap<String, Duration>,
    frozen_logs: Option<HashMap<String, VecDeque<Event>>>,
}
impl Default for View {
    fn default() -> Self {
        Self {
            action: None,
            composing: false,
            draft: Default::default(),
            notice: String::new(),
            selected: 0,
            all: true,
            scroll: 0,
            round: "0".into(),
            round_limit: None,
            status: "STARTING".into(),
            states: HashMap::new(),
            events: VecDeque::new(),
            tab: Tab::Conversation,
            help: false,
            tasks: HashMap::new(),
            reports: HashMap::new(),
            report_count: 0,
            message_count: 0,
            error_count: 0,
            first_time: None,
            last_time: 0,
            agent_logs: HashMap::new(),
            memory_maps: HashMap::new(),
            shared_memory: String::new(),
            motion: true,
            elapsed: Duration::ZERO,
            started: Instant::now(),
            log_capacity: crate::config::default_log_buffer(),
            total_events: 0,
            agent_counts: HashMap::new(),
            activity: HashMap::new(),
            working_since: HashMap::new(),
            frozen_logs: None,
        }
    }
}
impl View {
    pub fn with_log_capacity(capacity: usize) -> Self {
        Self {
            log_capacity: capacity.clamp(100, 20_000),
            ..Self::default()
        }
    }
    pub fn advance(&mut self) -> bool {
        let before = self.elapsed.as_secs();
        self.elapsed = self.started.elapsed();
        before != self.elapsed.as_secs()
    }
    fn logs(&self) -> &HashMap<String, VecDeque<Event>> {
        self.frozen_logs.as_ref().unwrap_or(&self.agent_logs)
    }
    fn terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "COMPLETE" | "DELIVERED_WITH_LIMITS" | "FAILED" | "CANCELLED"
        )
    }
    fn spinner(&self, name: &str) -> &str {
        if self.terminal() || self.states.get(name).is_none_or(|s| s != "WORKING") {
            return "·";
        }
        if !self.motion {
            return "●";
        }
        ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
            [(self.elapsed.as_millis() / 100) as usize % 10]
    }
    fn sparkline(&self, name: &str) -> String {
        let history = self.activity.get(name);
        (0..12)
            .rev()
            .map(|offset| {
                let second = self.elapsed.as_secs().checked_sub(offset);
                let count = second
                    .map(|s| {
                        history
                            .into_iter()
                            .flatten()
                            .filter(|t| t.as_secs() == s)
                            .count()
                    })
                    .unwrap_or(0);
                ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"][count.min(7)]
            })
            .collect()
    }
    pub fn apply(&mut self, event: Event) {
        if event.kind == "memory" {
            self.memory_maps.insert(event.agent, event.text);
            return;
        }
        if event.kind == "shared_memory" {
            self.shared_memory = event.text;
            return;
        }
        self.advance();
        self.total_events += 1;
        self.first_time.get_or_insert(event.time);
        self.last_time = event.time;
        match event.kind.as_str() {
            "round_budget" => self.round_limit = event.text.parse().ok(),
            "round" => {
                self.round = event.text.clone();
                self.status = "RUNNING".into();
            }
            "working" | "waiting" | "ready" | "error" => {
                self.states
                    .insert(event.agent.clone(), event.kind.to_uppercase());
                if event.kind == "working" {
                    self.working_since.insert(event.agent.clone(), self.elapsed);
                    self.tasks.insert(event.agent.clone(), safe(&event.text));
                }
                if event.kind == "error" {
                    self.error_count += 1;
                }
            }
            "report" => {
                self.reports.insert(event.agent.clone(), safe(&event.text));
                self.report_count += 1;
            }
            "message" => self.message_count += 1,
            "done" => {
                self.states.insert(event.agent.clone(), "APPROVED".into());
                self.status = "COMPLETE".into();
            }
            "done_with_limits" => {
                self.states.insert(event.agent.clone(), "LIMITED".into());
                self.status = "DELIVERED_WITH_LIMITS".into();
            }
            "session_error" => {
                self.status = "FAILED".into();
                self.error_count += 1;
            }
            "cancelled" => self.status = "CANCELLED".into(),
            _ => {}
        }
        let mut recipients = vec![event.agent.clone()];
        if let Some(to) = &event.to
            && to != &event.agent
        {
            recipients.push(to.clone());
        }
        for name in recipients {
            *self.agent_counts.entry(name.clone()).or_default() += 1;
            let history = self.activity.entry(name.clone()).or_default();
            history.push_back(self.elapsed);
            while history
                .front()
                .is_some_and(|t| self.elapsed.saturating_sub(*t) > Duration::from_secs(12))
                || history.len() > 4096
            {
                history.pop_front();
            }
            let log = self.agent_logs.entry(name).or_default();
            if event.kind == "delta"
                && let Some(last) = log.back_mut()
                && last.kind == "delta"
                && last.text.len() + event.text.len() < 16_000
            {
                last.text.push_str(&event.text);
                last.time = event.time;
                continue;
            }
            if !log.back().is_some_and(|e| {
                e.kind == "output" && event.kind == "output" && e.text == event.text
            }) {
                log.push_back(event.clone());
                if log.len() > self.log_capacity {
                    log.pop_front();
                }
            }
        }
        self.events.push_back(event);
        if self.events.len() > 1000 {
            self.events.pop_front();
        }
    }
    /// Return true when the user requests leaving the dashboard.
    pub fn handle_key(&mut self, key: KeyEvent, agents: usize) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
        }
        if self.composing {
            match key.code {
                KeyCode::Esc => self.composing = false,
                KeyCode::Enter
                    if key.modifiers.contains(KeyModifiers::SHIFT)
                        || key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.draft.insert("\n")
                }
                KeyCode::Enter if !self.draft.value.trim().is_empty() => {
                    if self.draft.value.len() > 16 * 1024 {
                        self.notice =
                            "Message is too long (maximum 16 KiB). Shorten it before sending."
                                .into();
                    } else {
                        self.action = Some(Action::Send(self.draft.value.clone()));
                    }
                }
                KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.draft.insert("\n")
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.draft = Default::default()
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && !c.is_control() =>
                {
                    if self.draft.value.len() + c.len_utf8() <= 16 * 1024 {
                        self.draft.insert(&c.to_string());
                    }
                }
                KeyCode::Backspace => self.draft.backspace(),
                KeyCode::Delete => self.draft.delete(),
                KeyCode::Left => self.draft.left(),
                KeyCode::Right => self.draft.right(),
                KeyCode::Up => self.draft.vertical(false),
                KeyCode::Down => self.draft.vertical(true),
                KeyCode::Home => self.draft.home(),
                KeyCode::End => self.draft.end(),
                _ => {}
            }
            return false;
        }
        if self.help {
            self.help = false;
            return false;
        }
        match key.code {
            KeyCode::Char('i') | KeyCode::Char('n') => {
                self.composing = true;
                self.help = false;
                self.notice.clear();
            }
            KeyCode::F(2) | KeyCode::F(3) => self.action = Some(Action::Configure),
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char('?') | KeyCode::F(1) => self.help = true,
            KeyCode::Char('m') => self.motion = !self.motion,
            KeyCode::Char('p') => {
                self.frozen_logs = if self.frozen_logs.is_some() {
                    None
                } else {
                    Some(self.agent_logs.clone())
                };
                self.scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1) % agents.max(1);
                self.scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = (self.selected + agents.saturating_sub(1)) % agents.max(1);
                self.scroll = 0;
            }
            KeyCode::Tab | KeyCode::Right => {
                self.selected = (self.selected + 1) % agents.max(1);
                self.scroll = 0;
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.selected = (self.selected + agents.saturating_sub(1)) % agents.max(1);
                self.scroll = 0;
            }
            KeyCode::Enter => {
                self.all = !self.all;
                self.tab = Tab::Conversation;
                self.scroll = 0;
            }
            KeyCode::Char('1') => {
                self.tab = Tab::Conversation;
                self.scroll = 0;
            }
            KeyCode::Char('2') => {
                self.tab = Tab::Tasks;
                self.scroll = 0;
            }
            KeyCode::Char('3') => {
                self.tab = Tab::Profile;
                self.scroll = 0;
            }
            KeyCode::Char('4') => {
                self.tab = Tab::Memory;
                self.scroll = 0;
            }
            KeyCode::Char('a') => {
                self.all = !self.all;
                self.scroll = 0;
            }
            KeyCode::PageUp => {
                self.scroll = if self.tab == Tab::Conversation {
                    self.scroll.saturating_add(8)
                } else {
                    self.scroll.saturating_sub(8)
                }
            }
            KeyCode::PageDown => {
                self.scroll = if self.tab == Tab::Conversation {
                    self.scroll.saturating_sub(8)
                } else {
                    self.scroll.saturating_add(8)
                }
            }
            KeyCode::Home | KeyCode::End => {
                self.scroll = 0;
                self.frozen_logs = None;
            }
            _ => {}
        }
        false
    }

    pub fn paste(&mut self, text: &str) {
        self.composing = true;
        let text = safe(&text.replace("\r\n", "\n").replace('\r', "\n"));
        if self.draft.value.len() + text.len() <= 16 * 1024 {
            self.draft.insert(&text);
        } else {
            self.notice = "Paste exceeds 16 KiB; draft kept unchanged.".into();
        }
    }
}

fn style(color: Color) -> Style {
    Style::default().fg(color)
}
fn bold(color: Color) -> Style {
    style(color).add_modifier(Modifier::BOLD)
}
fn panel(title: &str, focused: bool) -> Block<'static> {
    Block::default()
        .title(Line::from(Span::styled(
            format!(" {title} "),
            bold(if focused { PURPLE } else { MUTED }),
        )))
        .borders(Borders::TOP)
        .border_style(style(if focused { PURPLE } else { BORDER }))
        .style(style(FG).bg(PANEL))
        .padding(Padding::horizontal(1))
}
fn status_color(status: &str) -> Color {
    match status {
        "WORKING" | "RUNNING" => BLUE,
        "READY" | "APPROVED" | "COMPLETE" => GREEN,
        "ERROR" | "FAILED" => RED,
        "WAITING" | "CANCELLED" | "LIMITED" | "DELIVERED_WITH_LIMITS" => AMBER,
        _ => MUTED,
    }
}
fn status_label(status: &str) -> &str {
    match status {
        "WORKING" => "Working",
        "WAITING" => "Waiting",
        "READY" => "Ready",
        "APPROVED" => "Approved",
        "ERROR" => "Failed",
        "COMPLETE" => "Complete",
        "DELIVERED_WITH_LIMITS" => "Delivered / checks pending",
        "LIMITED" => "Checks pending",
        "RUNNING" => "Running",
        "FAILED" => "Failed",
        "CANCELLED" => "Cancelled",
        "STARTING" => "Starting",
        _ => "Idle",
    }
}
fn provider(a: &Agent) -> &str {
    a.provider.label()
}
fn role(name: &str) -> &str {
    match name {
        "lead" => "Team lead",
        "frontend" => "Frontend engineer",
        "backend" => "Backend engineer",
        "architect" | "devops" => "Project architect",
        "tester" => "Test engineer",
        "reviewer" => "Code reviewer",
        "designer" => "Product designer",
        _ => "Specialist",
    }
}
fn safe(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect()
}
fn fit(text: &str, width: usize) -> String {
    let text = safe(text).replace('\n', " ");
    if text.width() <= width {
        return text;
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w >= width {
            break;
        }
        out.push(c);
        used += w;
    }
    if width > 0 {
        out.push('…');
    }
    out
}
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    for original in safe(text).lines() {
        let mut line = String::new();
        let mut used = 0;
        for word in original.split_whitespace() {
            if !line.is_empty() && used + 1 + word.width() > width {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            if !line.is_empty() {
                line.push(' ');
                used += 1;
            }
            for c in word.chars() {
                let w = c.width().unwrap_or(0);
                if used + w > width {
                    lines.push(std::mem::take(&mut line));
                    used = 0;
                }
                line.push(c);
                used += w;
            }
        }
        lines.push(line);
    }
    lines
}

pub fn render(frame: &mut Frame, view: &View, config: &Config, goal: &str, demo: bool) {
    let area = frame.area();
    frame.render_widget(Block::default().style(style(FG).bg(BG)), area);
    if area.width < 60 || area.height < 20 {
        frame.render_widget(
            Paragraph::new("AI LEAD\nResize to at least 60 × 20 to see all agent logs. q exits."),
            area,
        );
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(3),
    ])
    .margin(1)
    .split(area);
    let active: Vec<_> = if view.terminal() {
        vec![]
    } else {
        config
            .agents
            .iter()
            .filter(|a| view.states.get(&a.name).map(String::as_str) == Some("WORKING"))
            .map(|a| a.name.as_str())
            .collect()
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("AI LEAD", bold(FG)),
                Span::styled(
                    format!(
                        "    {}    {}    round {}/{}",
                        if demo {
                            "demo"
                        } else if config.leader.full_access {
                            "live / FULL ACCESS"
                        } else {
                            "live"
                        },
                        status_label(&view.status),
                        view.round,
                        view.round_limit.unwrap_or(config.max_rounds)
                    ),
                    style(MUTED),
                ),
                Span::styled(
                    format!(
                        "    {} reports / {} messages",
                        view.report_count, view.message_count
                    ),
                    style(MUTED),
                ),
            ]),
            Line::from(Span::styled(
                fit(
                    &format!(
                        "{:02}:{:02} · {} events · {}{}",
                        view.elapsed.as_secs() / 60,
                        view.elapsed.as_secs() % 60,
                        view.total_events,
                        if view.frozen_logs.is_some() {
                            "VIEW PAUSED · "
                        } else {
                            ""
                        },
                        goal
                    ),
                    rows[0].width as usize,
                ),
                style(MUTED),
            )),
            Line::from(Span::styled(
                fit(
                    &format!(
                        "{}/{} specialists active{}",
                        active.len(),
                        config.max_parallel_agents,
                        if active.is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", active.join(", "))
                        }
                    ),
                    rows[0].width as usize,
                ),
                style(if active.is_empty() { MUTED } else { BLUE }),
            )),
        ]),
        rows[0],
    );
    match view.tab {
        Tab::Conversation if view.all => render_grid(frame, rows[1], view, config),
        Tab::Conversation => render_conversation(frame, rows[1], view, config),
        Tab::Tasks => render_tasks(frame, rows[1], view, config),
        Tab::Profile => render_profile(frame, rows[1], view, config),
        Tab::Memory => render_memory(frame, rows[1], view, config),
    }
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                fit(
                    &format!(
                        "i Message to lead  ·  {}",
                        if view.notice.is_empty() {
                            "send a follow-up while agents work or after completion"
                        } else {
                            &view.notice
                        }
                    ),
                    rows[2].width as usize,
                ),
                style(BLUE),
            )),
            Line::from(Span::styled(
                fit(
                    &format!(
                        "F2 Model / provider: {}  ·  Tab / ↑↓ agent  ·  Enter expand",
                        std::iter::once(&config.leader)
                            .chain(&config.agents)
                            .nth(view.selected)
                            .unwrap_or(&config.leader)
                            .name
                    ),
                    rows[2].width as usize,
                ),
                style(FG),
            )),
            Line::from(Span::styled(
                "1 logs  2 tasks  3 profile  4 memory  p pause  ? help  q exit",
                style(MUTED),
            )),
        ]),
        rows[2],
    );
    if view.help {
        render_help(frame, area);
    }
    if view.composing {
        render_composer(frame, area, view);
    }
}

fn render_composer(frame: &mut Frame, area: Rect, view: &View) {
    let popup = centered(area, area.width.saturating_sub(4).min(90), 11);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(" Message to lead ")
        .borders(Borders::ALL)
        .border_style(style(BLUE))
        .style(style(FG).bg(PANEL))
        .padding(Padding::horizontal(1));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let rows = Layout::vertical([Constraint::Min(2), Constraint::Length(3)]).split(inner);
    let width = rows[0].width.max(1) as usize;
    let mut lines = vec![String::new()];
    let (mut row, mut column) = (0usize, 0usize);
    let mut cursor = (0usize, 0usize);
    for (offset, c) in view.draft.value.char_indices() {
        if c != '\n' && column + c.width().unwrap_or(0) > width {
            lines.push(String::new());
            row += 1;
            column = 0;
        }
        if offset == view.draft.cursor {
            cursor = (row, column);
        }
        if c == '\n' {
            lines.push(String::new());
            row += 1;
            column = 0;
        } else {
            lines[row].push(c);
            column += c.width().unwrap_or(0);
        }
    }
    if view.draft.cursor == view.draft.value.len() {
        cursor = (row, column);
    }
    let scroll = cursor
        .0
        .saturating_sub(rows[0].height.saturating_sub(1) as usize);
    frame.render_widget(
        Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>())
            .scroll((scroll as u16, 0)),
        rows[0],
    );
    frame.set_cursor_position((
        rows[0].x + cursor.1.min(width - 1) as u16,
        rows[0].y + cursor.0.saturating_sub(scroll) as u16,
    ));
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Enter send  ·  Ctrl+J new line  ·  Esc keep draft",
                style(BLUE),
            )),
            Line::from(Span::styled(
                "Saved input reaches the lead at the next planning turn.",
                style(MUTED),
            )),
            Line::from(Span::styled(
                fit(&view.notice, rows[1].width as usize),
                style(AMBER),
            )),
        ]),
        rows[1],
    );
}

fn render_grid(frame: &mut Frame, area: Rect, view: &View, config: &Config) {
    let columns = if area.width >= 145 { 3 } else { 2 };
    let count = config.agents.len().div_ceil(columns);
    let lead_height = if area.height >= 26 { 7 } else { 4 };
    let sections = Layout::vertical([Constraint::Length(lead_height), Constraint::Min(3)])
        .spacing(1)
        .split(area);
    render_agent_log(frame, sections[0], view, &config.leader, 0);
    let constraints = vec![Constraint::Ratio(1, count.max(1) as u32); count.max(1)];
    let rows = Layout::vertical(constraints).spacing(1).split(sections[1]);
    for (index, agent) in config.agents.iter().enumerate() {
        let parts = Layout::horizontal(vec![Constraint::Ratio(1, columns as u32); columns])
            .spacing(2)
            .split(rows[index / columns]);
        render_agent_log(frame, parts[index % columns], view, agent, index + 1);
    }
}
fn render_agent_log(frame: &mut Frame, area: Rect, view: &View, agent: &Agent, index: usize) {
    let state = view
        .states
        .get(&agent.name)
        .map(String::as_str)
        .unwrap_or("IDLE");
    let active = state == "WORKING" && !view.terminal();
    let count = view.agent_counts.get(&agent.name).copied().unwrap_or(0);
    let seconds = view
        .elapsed
        .saturating_sub(
            view.working_since
                .get(&agent.name)
                .copied()
                .unwrap_or(view.elapsed),
        )
        .as_secs();
    let label = if active {
        format!(
            "{} {} / {} · {}s · {}",
            view.spinner(&agent.name),
            agent.name,
            status_label(state),
            seconds,
            count
        )
    } else {
        format!("{} / {} · {}", agent.name, status_label(state), count)
    };
    let recent = view
        .activity
        .get(&agent.name)
        .and_then(|h| h.back())
        .is_some_and(|t| view.elapsed.saturating_sub(*t) < Duration::from_millis(800));
    let block = panel(&label, view.selected == index).border_style(style(
        if active || (recent && view.motion) {
            BLUE
        } else if view.selected == index {
            FG
        } else {
            BORDER
        },
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines = Vec::new();
    {
        let own = view.logs().get(&agent.name).into_iter().flatten();
        let system = view.logs().get("system").into_iter().flatten().filter(|e| {
            index == 0
                && agent.name != "system"
                && matches!(e.kind.as_str(), "error" | "session_error" | "cancelled")
        });
        let wanted = inner.height as usize
            + if view.selected == index {
                view.scroll as usize
            } else {
                0
            };
        let tail: Vec<_> = own
            .chain(system)
            .filter(|e| !["round", "ready", "waiting", "session"].contains(&e.kind.as_str()))
            .rev()
            .take(wanted + 1)
            .collect();
        for e in tail.into_iter().rev() {
            if ["round", "ready", "waiting", "session"].contains(&e.kind.as_str()) {
                continue;
            }
            let prefix = if e.agent != agent.name {
                format!("{} → ", e.agent)
            } else if let Some(to) = &e.to {
                format!("→ {to}: ")
            } else if e.kind == "working" {
                "task: ".into()
            } else {
                String::new()
            };
            let color = match e.kind.as_str() {
                "error" | "session_error" => RED,
                "report" | "done" => GREEN,
                "done_with_limits" | "verification_limit" => AMBER,
                "message" => BLUE,
                _ => MUTED,
            };
            for text in wrap(&format!("{prefix}{}", e.text), inner.width as usize) {
                lines.push(Line::from(Span::styled(text, style(color))));
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "Waiting for an assignment.",
            style(MUTED),
        )));
    }
    if inner.height >= 6 {
        let model = agent.model.as_deref().unwrap_or("default");
        lines.push(Line::from(Span::styled(
            format!(
                "{}  {} / {}",
                view.sparkline(&agent.name),
                provider(agent),
                model
            ),
            style(if active { BLUE } else { MUTED }),
        )));
    }
    let offset = if view.selected == index {
        view.scroll as usize
    } else {
        0
    };
    let skip = lines
        .len()
        .saturating_sub(inner.height as usize)
        .saturating_sub(offset);
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .skip(skip)
                .take(inner.height as usize)
                .collect::<Vec<_>>(),
        ),
        inner,
    );
}
fn selected<'a>(view: &View, config: &'a Config) -> &'a Agent {
    if view.selected == 0 {
        &config.leader
    } else {
        config
            .agents
            .get(view.selected - 1)
            .unwrap_or(&config.leader)
    }
}
fn relevant(view: &View, config: &Config, e: &Event) -> bool {
    let name = &selected(view, config).name;
    view.all || &e.agent == name || e.to.as_ref() == Some(name)
}
fn render_conversation(frame: &mut Frame, area: Rect, view: &View, config: &Config) {
    let title = if view.all {
        "Team conversation".into()
    } else {
        format!("Conversation / {}", selected(view, config).name)
    };
    let block = panel(&title, false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines = Vec::new();
    let empty = VecDeque::new();
    let tail: Vec<_> = view
        .logs()
        .get(&selected(view, config).name)
        .unwrap_or(&empty)
        .iter()
        .filter(|e| !["round", "ready", "waiting", "session"].contains(&e.kind.as_str()))
        .rev()
        .take(inner.height as usize + view.scroll as usize + 1)
        .collect();
    for e in tail.into_iter().rev() {
        if !relevant(view, config, e)
            || ["round", "ready", "waiting", "session"].contains(&e.kind.as_str())
        {
            continue;
        }
        let (label, color) = match e.kind.as_str() {
            "message" => ("message", BLUE),
            "report" => ("report", GREEN),
            "working" => ("assignment", PURPLE),
            "error" | "session_error" => ("error", RED),
            "done" => ("approved", GREEN),
            "done_with_limits" | "verification_limit" => ("checks pending", AMBER),
            "cancelled" => ("stopped", AMBER),
            _ => ("activity", MUTED),
        };
        let target =
            e.to.as_ref()
                .map(|to| format!(" → {to}"))
                .unwrap_or_default();
        let elapsed = e.time.saturating_sub(view.first_time.unwrap_or(e.time));
        lines.push(Line::from(vec![
            Span::styled(format!("{}{}", e.agent, target), bold(color)),
            Span::styled(
                format!("  {label}  +{:02}:{:02}", elapsed / 60, elapsed % 60),
                style(MUTED),
            ),
        ]));
        for text in wrap(&e.text, inner.width as usize) {
            lines.push(Line::from(Span::styled(
                text,
                style(if e.kind == "output" { MUTED } else { FG }),
            )));
        }
        lines.push(Line::from(""));
    }
    if lines.is_empty() {
        lines = vec![
            Line::from(""),
            Line::from(Span::styled("Your team is getting ready.", bold(PURPLE))),
            Line::from(""),
            Line::from("Assignments, handoffs and reports appear here."),
        ];
    }
    let skip = lines
        .len()
        .saturating_sub(inner.height as usize)
        .saturating_sub(view.scroll as usize);
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .skip(skip)
                .take(inner.height as usize)
                .collect::<Vec<_>>(),
        ),
        inner,
    );
}
fn render_tasks(frame: &mut Frame, area: Rect, view: &View, config: &Config) {
    let block = panel("Assignments & results", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines = Vec::new();
    for a in std::iter::once(&config.leader).chain(&config.agents) {
        if !view.all && a.name != selected(view, config).name {
            continue;
        }
        let state = view
            .states
            .get(&a.name)
            .map(String::as_str)
            .unwrap_or("IDLE");
        lines.push(Line::from(vec![
            Span::styled(&a.name, bold(PURPLE)),
            Span::styled(
                format!("  {}", status_label(state)),
                style(status_color(state)),
            ),
        ]));
        for l in wrap(
            view.tasks
                .get(&a.name)
                .map(String::as_str)
                .unwrap_or("No assignment yet."),
            inner.width as usize,
        ) {
            lines.push(Line::from(l));
        }
        if let Some(report) = view.reports.get(&a.name) {
            lines.push(Line::from(Span::styled("Latest report", style(GREEN))));
            for l in wrap(report, inner.width as usize) {
                lines.push(Line::from(Span::styled(l, style(MUTED))));
            }
        }
        lines.push(Line::from(""));
    }
    frame.render_widget(Paragraph::new(lines).scroll((view.scroll, 0)), inner);
}
fn render_profile(frame: &mut Frame, area: Rect, view: &View, config: &Config) {
    let a = selected(view, config);
    let mut lines = vec![
        Line::from(Span::styled(role(&a.name), bold(PURPLE))),
        Line::from(""),
        Line::from(format!("Agent      {}", a.name)),
        Line::from(format!("Provider   {}", provider(a))),
        Line::from(format!(
            "Model      {}",
            a.model.as_deref().unwrap_or("Provider CLI default")
        )),
        Line::from(format!(
            "Reasoning  {}",
            a.reasoning.as_deref().unwrap_or("Provider default")
        )),
        Line::from(""),
        Line::from(Span::styled("Role context", bold(BLUE))),
        Line::from(a.context.display().to_string()),
        Line::from(""),
        Line::from(Span::styled(
            format!("Skills / {} loaded", a.skills.len()),
            bold(BLUE),
        )),
    ];
    for path in &a.skills {
        lines.push(Line::from(format!("  {}", path.display())));
    }
    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            "Edit .ai-lead/team.toml to change this profile.",
            style(MUTED),
        )),
    ]);
    let block = panel(&format!("Profile / {}", a.name), false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines: Vec<_> = lines
        .into_iter()
        .flat_map(|line| {
            let s = line.to_string();
            wrap(&s, inner.width as usize)
                .into_iter()
                .map(move |s| {
                    Line::from(Span::styled(
                        s,
                        line.spans.first().map(|s| s.style).unwrap_or_default(),
                    ))
                })
                .collect::<Vec<_>>()
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).scroll((view.scroll, 0)), inner);
}
fn render_memory(frame: &mut Frame, area: Rect, view: &View, config: &Config) {
    let agent = selected(view, config);
    let block = panel(&format!("Persistent memory / {}", agent.name), false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let map = view
        .memory_maps
        .get(&agent.name)
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .unwrap_or_default();
    let shared =
        serde_json::from_str::<Vec<serde_json::Value>>(&view.shared_memory).unwrap_or_default();
    let mut lines = Vec::new();
    for (title, items) in [
        (
            "Facts",
            map["memory"]["facts"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|n| n["text"].as_str())
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        ),
        (
            "Recent work",
            map["memory"]["recent_work"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .rev()
                        .map(|r| {
                            format!(
                                "{}: {}",
                                r["status"].as_str().unwrap_or(""),
                                r["summary"].as_str().unwrap_or("")
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        (
            "Next steps",
            map["memory"]["next_steps"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        ),
        (
            "Pending tasks",
            map["pending_tasks"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|r| {
                            format!(
                                "[{}] {}",
                                r["task"]["status"].as_str().unwrap_or(""),
                                r["task"]["task"].as_str().unwrap_or("")
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        (
            "Blockers",
            map["memory"]["blockers"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        ),
        (
            "Shared team notes",
            shared
                .iter()
                .rev()
                .map(|n| {
                    format!(
                        "{}: {}",
                        n["author"].as_str().unwrap_or(""),
                        n["text"].as_str().unwrap_or("")
                    )
                })
                .collect(),
        ),
    ] {
        lines.push(Line::from(Span::styled(title, bold(FG))));
        if items.is_empty() {
            lines.push(Line::from(Span::styled("No saved entries.", style(MUTED))));
        }
        for item in items {
            for text in wrap(&format!("• {item}"), inner.width as usize) {
                lines.push(Line::from(Span::styled(text, style(MUTED))));
            }
        }
        lines.push(Line::from(""));
    }
    frame.render_widget(Paragraph::new(lines).scroll((view.scroll, 0)), inner);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width.min(area.width),
        height.min(area.height),
    )
}
fn render_help(frame: &mut Frame, area: Rect) {
    let popup = centered(area, 58, 19);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled("Move around your team", bold(PURPLE))),
            Line::from(""),
            Line::from("↑ / ↓, j / k      Select an agent"),
            Line::from("Tab / Shift+Tab   Select an agent"),
            Line::from("1 / 2 / 3 / 4     Logs / Tasks / Profile / Memory"),
            Line::from("Enter / a         Grid / selected agent log"),
            Line::from("PgUp / PgDn       Browse history"),
            Line::from("End               Return to latest activity"),
            Line::from("p / m             Pause display / toggle motion"),
            Line::from("i / n             Message the lead / follow-up"),
            Line::from("F2 / F3           Edit selected agent model/provider"),
            Line::from(""),
            Line::from("? / F1            Open this guide"),
            Line::from("q / Esc / Ctrl+C  Stop run or exit"),
            Line::from(""),
            Line::from(Span::styled(
                "Stopping keeps changes already made to files.",
                style(AMBER),
            )),
            Line::from(""),
            Line::from("Press any key to return."),
        ])
        .block(panel("Keyboard guide", true)),
        popup,
    );
}

fn render_permission_consent(frame: &mut Frame, project: &Path, response: &str) {
    let area = frame.area();
    frame.render_widget(Block::default().style(style(FG).bg(BG)), area);
    let content = centered(
        area,
        area.width.saturating_sub(4).min(96),
        area.height.saturating_sub(2).min(26),
    );
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(5),
        Constraint::Length(3),
    ])
    .split(content);
    frame.render_widget(
        Paragraph::new(Span::styled(
            "FULL ACCESS / consent for every agent",
            bold(AMBER),
        )),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(project.display().to_string()),
            Line::from(""),
            Line::from(Span::styled(
                "ALL agents, including the lead, can:",
                bold(AMBER),
            )),
            Line::from("- Run shell commands, install software, use network."),
            Line::from("- Read/change/delete files outside this project."),
            Line::from("- Use your account's available credentials."),
            Line::from("- Work without sandbox or permission prompts."),
            Line::from("OS/provider limits apply; no automatic root/admin."),
            Line::from(""),
            Line::from("Saved for this project; applies to your task."),
            Line::from("Revoke: ai-lead permissions restricted (or F5)."),
        ])
        .wrap(ratatui::widgets::Wrap { trim: false }),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Type ALLOW + Enter to authorize · Esc cancels",
                bold(AMBER),
            )),
            Line::from(""),
            Line::from(format!("› {response}")),
        ]),
        rows[2],
    );
}

pub fn render_onboarding(frame: &mut Frame, project: &Path) {
    let area = frame.area();
    frame.render_widget(Block::default().style(style(FG).bg(BG)), area);
    let content = centered(
        area,
        area.width.saturating_sub(6).min(96),
        area.height.saturating_sub(2).min(22),
    );
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(5),
        Constraint::Length(3),
    ])
    .split(content);
    frame.render_widget(
        Paragraph::new(Span::styled("AI LEAD / configure this project", bold(FG))),
        rows[0],
    );
    let mut lines = vec![
        Line::from(Span::styled(project.display().to_string(), bold(BLUE))),
        Line::from(""),
        Line::from("Set up a lead and six specialists for this directory."),
        Line::from("Choose providers, models, prompts and skills."),
        Line::from(""),
        Line::from("Files stay inside this project:"),
        Line::from("  .ai-lead/team.toml       team settings"),
        Line::from("  .ai-lead/contexts/       role prompts"),
        Line::from("  .ai-lead/skills/         attached skills"),
        Line::from("  .ai-lead/memory/         team memory"),
        Line::from("  .ai-lead/runs/           session logs"),
    ];
    if rows[1].height < 13 {
        lines = vec![
            Line::from(Span::styled(project.display().to_string(), bold(BLUE))),
            Line::from(""),
            Line::from("Choose your team's providers, models and skills."),
            Line::from("Project-local files:"),
            Line::from("  .ai-lead/team.toml"),
            Line::from("  .ai-lead/contexts/ and skills/"),
            Line::from("  .ai-lead/memory/ and runs/"),
        ];
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from("Ctrl+S in setup saves and opens task entry."),
            Line::from(""),
            Line::from(Span::styled(
                "Enter  Configure this directory     Esc  Exit",
                bold(FG),
            )),
        ])
        .wrap(ratatui::widgets::Wrap { trim: false }),
        rows[2],
    );
}

pub fn render_launcher(frame: &mut Frame, config: &Config, project: &Path, launch: &Launch) {
    let area = frame.area();
    frame.render_widget(Block::default().style(style(FG).bg(BG)), area);
    if area.width < 60 || area.height < 20 {
        frame.render_widget(
            Paragraph::new("AI LEAD — resize to 60 × 20. Esc exits."),
            area,
        );
        return;
    }
    let content = centered(
        area,
        area.width.saturating_sub(6).min(110),
        area.height.saturating_sub(2).min(29),
    );
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(3),
        Constraint::Length(4),
        Constraint::Length(2),
    ])
    .split(content);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled("AI LEAD / new session", bold(FG))),
            Line::from(Span::styled(
                fit(&project.display().to_string(), content.width as usize),
                style(MUTED),
            )),
        ]),
        rows[0],
    );
    let mut team = vec![
        Line::from(Span::styled(
            "Agent          Provider   Model                    Skills",
            style(MUTED),
        )),
        Line::from(""),
    ];
    for a in std::iter::once(&config.leader).chain(&config.agents) {
        team.push(Line::from(format!(
            "{:<14} {:<10} {:<24} {}",
            fit(&a.name, 13),
            provider(a),
            fit(a.model.as_deref().unwrap_or("CLI default"), 23),
            a.skills.len()
        )));
    }
    frame.render_widget(Paragraph::new(team), rows[1]);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "F3 Configure team · F5 Full access / Restricted",
                bold(FG),
            )),
            Line::from(Span::styled(
                "Full prompts, skill files, provider, model and reasoning. Ctrl+S saves.",
                style(MUTED),
            )),
        ]),
        rows[2],
    );
    let (mode, color) = match launch.mode {
        1 => ("Live / read only", BLUE),
        2 if config.permissions == crate::config::Permissions::FullAccess => {
            ("Live / Full access — all agents", AMBER)
        }
        2 => ("Live / edit files", AMBER),
        _ => ("Demo / no model calls", GREEN),
    };
    let block = panel("Task", true);
    let inner = block.inner(rows[3]);
    frame.render_widget(block, rows[3]);
    let width = inner.width.saturating_sub(3) as usize;
    let mut tail = String::new();
    let mut used = 0;
    for c in launch.goal.chars().rev() {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        tail.insert(0, c);
        used += w;
    }
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!(
                "› {}",
                if tail.is_empty() {
                    "Describe your task…"
                } else {
                    &tail
                }
            )),
            Line::from(Span::styled(mode, style(color))),
        ]),
        inner,
    );
    frame.set_cursor_position((inner.x + 2 + used as u16, inner.y));
    frame.render_widget(
        Paragraph::new("Enter start   F2 mode   F3 setup   F4 resume   Esc exit")
            .style(style(MUTED)),
        rows[4],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Provider;
    fn setup() -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        let config =
            Config::load(&crate::config::init(dir.path(), Provider::Codex).unwrap()).unwrap();
        (dir, config)
    }
    #[test]
    fn composer_keeps_unicode_draft_and_does_not_treat_typed_shortcuts_as_commands() {
        let mut view = View::default();
        assert!(!view.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE), 7));
        view.paste("Добавь поиск\nq и m");
        assert!(!view.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE), 7));
        assert!(view.composing);
        assert!(view.motion);
        view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 7);
        assert!(
            matches!(view.action.take(), Some(Action::Send(text)) if text == "Добавь поиск\nq и mq")
        );
        view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 7);
        assert!(!view.composing);
        assert_eq!(view.draft.value, "Добавь поиск\nq и mq");
        view.handle_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE), 7);
        assert!(matches!(view.action.take(), Some(Action::Configure)));
        let (_dir, config) = setup();
        view.composing = true;
        for size in [(120, 40), (80, 24), (60, 20)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(size.0, size.1)).unwrap();
            terminal
                .draw(|f| render(f, &view, &config, "Goal", false))
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(text.contains("Message to lead"));
            assert!(text.contains("Enter send"));
            assert!(text.contains("Добавь поиск"));
        }
    }
    #[test]
    fn dashboard_renders_at_multiple_sizes() {
        let (_dir, config) = setup();
        for (w, h) in [(150, 45), (120, 40), (80, 24), (60, 20), (30, 10)] {
            for tab in [Tab::Conversation, Tab::Tasks, Tab::Profile, Tab::Memory] {
                let mut view = View {
                    tab,
                    ..View::default()
                };
                view.apply(Event {
                    time: 10,
                    kind: "working".into(),
                    agent: "backend".into(),
                    to: None,
                    text: "Разработать API для кабинета".into(),
                });
                let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
                terminal
                    .draw(|f| render(f, &view, &config, "Создать приложение", true))
                    .unwrap();
                let text = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(text.contains("AI LEAD"));
            }
        }
    }
    #[test]
    fn keyboard_navigation_filters_and_help() {
        let mut view = View::default();
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        assert!(!view.handle_key(key(KeyCode::Down), 7));
        assert_eq!(view.selected, 1);
        view.handle_key(key(KeyCode::Char('a')), 7);
        assert!(!view.all);
        view.handle_key(key(KeyCode::Tab), 7);
        assert_eq!(view.selected, 2);
        view.handle_key(key(KeyCode::Char('?')), 7);
        assert!(view.help);
        assert!(!view.handle_key(key(KeyCode::Esc), 7));
        assert!(!view.help);
        assert!(view.handle_key(key(KeyCode::Char('q')), 7));
    }
    #[test]
    fn grid_keeps_every_agent_visible_and_retains_inactive_logs() {
        let (_dir, config) = setup();
        let mut view = View::default();
        view.apply(Event {
            time: 1,
            kind: "output".into(),
            agent: "designer".into(),
            to: None,
            text: "Design handoff retained".into(),
        });
        for i in 0..1100 {
            view.apply(Event {
                time: 2,
                kind: "output".into(),
                agent: "backend".into(),
                to: None,
                text: format!("backend output {i}"),
            });
        }
        assert_eq!(view.agent_logs["designer"].len(), 1);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| render(f, &view, &config, "Task", true))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        for a in std::iter::once(&config.leader).chain(&config.agents) {
            assert!(text.contains(&a.name), "Missing {}", a.name);
        }
    }
    #[test]
    fn unicode_wrapping_respects_terminal_cells() {
        for line in wrap("Привет команда 世界 Rust 界界界界界", 8) {
            assert!(line.width() <= 8);
        }
        assert!(fit("世界世界世界", 7).width() <= 7);
    }
    #[test]
    fn launcher_and_help_render_in_compact_terminal() {
        let (dir, config) = setup();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| {
                render_launcher(
                    f,
                    &config,
                    dir.path(),
                    &Launch {
                        goal: "Создать приложение".into(),
                        mode: 0,
                        resume: false,
                    },
                )
            })
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("Demo / no model calls"));
        let view = View {
            help: true,
            ..View::default()
        };
        terminal
            .draw(|f| render(f, &view, &config, "Task", true))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("Keyboard guide"));
    }
    #[test]
    fn busy_logs_are_bounded_pause_is_stable_and_deltas_join() {
        let mut view = View::with_log_capacity(100);
        let event = |kind: &str, text: String| Event {
            time: 1,
            kind: kind.into(),
            agent: "backend".into(),
            to: None,
            text,
        };
        for i in 0..500 {
            view.apply(event("output", format!("line {i}")));
        }
        assert_eq!(view.agent_logs["backend"].len(), 100);
        assert_eq!(view.agent_counts["backend"], 500);
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        view.handle_key(key('p'), 7);
        view.apply(event("output", "new while paused".into()));
        assert_eq!(view.logs()["backend"].back().unwrap().text, "line 499");
        view.handle_key(key('p'), 7);
        assert_eq!(
            view.logs()["backend"].back().unwrap().text,
            "new while paused"
        );
        view.apply(event("delta", "Hello ".into()));
        view.apply(event("delta", "world".into()));
        assert_eq!(view.logs()["backend"].back().unwrap().text, "Hello world");
        assert_eq!(view.total_events, 503);
        view.apply(event("working", "task".into()));
        assert_ne!(view.spinner("backend"), "·");
        view.handle_key(key('m'), 7);
        assert_eq!(view.spinner("backend"), "●");
        view.status = "COMPLETE".into();
        assert_eq!(view.spinner("backend"), "·");
    }
    #[test]
    fn first_run_screen_shows_project_and_actions() {
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| render_onboarding(f, Path::new("/projects/my-app")))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("/projects/my-app"));
        assert!(text.contains(".ai-lead/team.toml"));
        assert!(text.contains("Configure this directory"));
    }
    #[test]
    fn permission_consent_keeps_confirmation_visible_on_small_terminals() {
        for (width, height) in [(60, 20), (80, 24), (120, 40)] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|f| render_permission_consent(f, Path::new("/projects/my-app"), "ALL"))
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("Type ALLOW"));
            assert!(text.contains("Esc cancels"));
            assert!(
                text.split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains("outside this project")
            );
        }
    }
    #[test]
    fn delivered_limits_are_amber_terminal_without_marking_checks_as_approved() {
        let mut view = View::default();
        view.apply(Event {
            time: 1,
            kind: "done_with_limits".into(),
            agent: "lead".into(),
            to: None,
            text: "Implemented; browser cookie verification pending (EPERM)".into(),
        });
        assert_eq!(view.status, "DELIVERED_WITH_LIMITS");
        assert_eq!(view.states["lead"], "LIMITED");
        assert!(view.terminal());
        assert_eq!(view.error_count, 0);
        assert_eq!(status_color(&view.status), AMBER);
        assert_ne!(view.states["lead"], "APPROVED");
    }
}
