//!
//! Broker TUI (Terminal User Interface) module.
//! Mirrors the client TUI layout and input handling patterns.

use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io;
use std::time::SystemTime;

const BROKER_VERSION: &str = env!("CARGO_PKG_VERSION");

const COLOR_BG: Color = Color::Rgb(0x07, 0x36, 0x42);
const COLOR_BG_SOFT: Color = Color::Rgb(0x00, 0x2B, 0x36);
const COLOR_FG: Color = Color::Rgb(0x93, 0xA1, 0xA1);
const COLOR_ACCENT: Color = Color::Rgb(0x2A, 0xA1, 0x98);
const COLOR_HILITE: Color = Color::Rgb(0xB5, 0x89, 0x00);
const COLOR_ERR: Color = Color::Rgb(0xDC, 0x32, 0x2F);

#[derive(Clone, Debug)]
pub enum Message {
    Info {
        text: String,
        timestamp: SystemTime,
    },
    Error {
        text: String,
        timestamp: SystemTime,
    },
    Stats {
        peer_count: usize,
        rooms: Vec<String>,
    },
}

#[derive(Clone, Debug)]
pub struct Completion {
    pub text: String,
    pub display: String,
    pub description: String,
}

#[derive(Clone, Debug)]
struct Completer {
    commands: Vec<Completion>,
}

impl Completer {
    fn new() -> Self {
        Self {
            commands: vec![
                Completion {
                    text: "/help".to_string(),
                    display: "/help".to_string(),
                    description: "Show broker commands".to_string(),
                },
                Completion {
                    text: "/status".to_string(),
                    display: "/status".to_string(),
                    description: "Show connected clients and rooms".to_string(),
                },
                Completion {
                    text: "/quit".to_string(),
                    display: "/quit".to_string(),
                    description: "Gracefully shutdown broker".to_string(),
                },
                Completion {
                    text: "/exit".to_string(),
                    display: "/exit".to_string(),
                    description: "Gracefully shutdown broker".to_string(),
                },
                Completion {
                    text: "/shutdown".to_string(),
                    display: "/shutdown".to_string(),
                    description: "Gracefully shutdown broker".to_string(),
                },
            ],
        }
    }

    fn complete(&self, input: &str, cursor_pos: usize) -> Vec<Completion> {
        let before_cursor = &input[..cursor_pos];
        let last_word_start = before_cursor.rfind(' ').map(|i| i + 1).unwrap_or(0);
        let token = &before_cursor[last_word_start..];

        if !token.starts_with('/') {
            return Vec::new();
        }

        self.commands
            .iter()
            .filter(|c| c.text.starts_with(token))
            .cloned()
            .collect()
    }
}

pub struct App {
    pub bind_addr: String,
    pub tls_enabled: bool,
    pub peer_count: usize,
    pub rooms: Vec<String>,

    pub messages: Vec<Message>,
    pub input: String,
    pub cursor_pos: usize,
    pub scroll_offset: usize,
    pub show_sidebar: bool,
    pub should_quit: bool,
    pub terminal_height: u16,

    command_history: Vec<String>,
    history_index: Option<usize>,
    max_history: usize,

    completer: Completer,
    pub current_completions: Vec<Completion>,
    pub selected_completion: Option<usize>,
}

impl App {
    pub fn new(bind_addr: String, tls_enabled: bool) -> Self {
        Self {
            bind_addr,
            tls_enabled,
            peer_count: 0,
            rooms: Vec::new(),
            messages: Vec::new(),
            input: String::new(),
            cursor_pos: 0,
            scroll_offset: 0,
            show_sidebar: false,
            should_quit: false,
            terminal_height: 24,
            command_history: Vec::new(),
            history_index: None,
            max_history: 100,
            completer: Completer::new(),
            current_completions: Vec::new(),
            selected_completion: None,
        }
    }

    pub fn handle_message(&mut self, msg: Message) {
        match msg {
            Message::Stats { peer_count, rooms } => {
                self.peer_count = peer_count;
                self.rooms = rooms;
            }
            Message::Info { .. } | Message::Error { .. } => {
                self.add_message(msg);
            }
        }
    }

    pub fn add_info(&mut self, text: impl Into<String>) {
        self.add_message(Message::Info {
            text: text.into(),
            timestamp: SystemTime::now(),
        });
    }

    pub fn add_error(&mut self, text: impl Into<String>) {
        self.add_message(Message::Error {
            text: text.into(),
            timestamp: SystemTime::now(),
        });
    }

    fn add_message(&mut self, msg: Message) {
        if matches!(msg, Message::Stats { .. }) {
            return;
        }
        self.messages.push(msg);
        if self.scroll_offset == 0 || self.scroll_offset >= self.messages.len().saturating_sub(10) {
            self.scroll_offset = self.messages.len().saturating_sub(1);
        }
    }

    fn format_timestamp(timestamp: &SystemTime) -> String {
        let duration = timestamp
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = duration.as_secs();
        let hours = (secs / 3600) % 24;
        let mins = (secs / 60) % 60;
        let secs = secs % 60;
        format!("{:02}:{:02}:{:02}", hours, mins, secs)
    }

    pub fn handle_input(&mut self, c: char) -> Option<String> {
        if c == '\n' {
            let cmd = self.input.clone();
            if !cmd.is_empty() {
                self.add_to_history(&cmd);
                self.input.clear();
                self.cursor_pos = 0;
                self.history_index = None;
                return Some(cmd);
            }
        } else {
            self.input.insert(self.cursor_pos, c);
            self.cursor_pos += 1;
            self.history_index = None;
            self.current_completions.clear();
            self.selected_completion = None;
        }
        None
    }

    pub fn handle_backspace(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
            self.input.remove(self.cursor_pos);
        }
    }

    pub fn handle_delete(&mut self) {
        if self.cursor_pos < self.input.len() {
            self.input.remove(self.cursor_pos);
        }
    }

    pub fn cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
        }
    }

    pub fn cursor_right(&mut self) {
        if self.cursor_pos < self.input.len() {
            self.cursor_pos += 1;
        }
    }

    pub fn cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    pub fn cursor_end(&mut self) {
        self.cursor_pos = self.input.len();
    }

    pub fn scroll_up(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset -= 1;
        }
    }

    pub fn scroll_down(&mut self) {
        if self.scroll_offset < self.messages.len().saturating_sub(1) {
            self.scroll_offset += 1;
        }
    }

    pub fn history_previous(&mut self) {
        if self.command_history.is_empty() {
            return;
        }

        match self.history_index {
            None => {
                let last_idx = self.command_history.len() - 1;
                self.history_index = Some(last_idx);
                self.input = self.command_history[last_idx].clone();
                self.cursor_pos = self.input.len();
            }
            Some(idx) if idx > 0 => {
                let new_idx = idx - 1;
                self.history_index = Some(new_idx);
                self.input = self.command_history[new_idx].clone();
                self.cursor_pos = self.input.len();
            }
            _ => {}
        }
    }

    pub fn history_next(&mut self) {
        match self.history_index {
            None => {}
            Some(idx) => {
                let history_len = self.command_history.len();
                if idx + 1 < history_len {
                    let new_idx = idx + 1;
                    self.history_index = Some(new_idx);
                    self.input = self.command_history[new_idx].clone();
                    self.cursor_pos = self.input.len();
                } else {
                    self.history_index = None;
                    self.input.clear();
                    self.cursor_pos = 0;
                }
            }
        }
    }

    fn add_to_history(&mut self, cmd: &str) {
        if cmd.is_empty() {
            return;
        }
        if let Some(last) = self.command_history.last() {
            if last == cmd {
                return;
            }
        }

        self.command_history.push(cmd.to_string());
        if self.command_history.len() > self.max_history {
            self.command_history.remove(0);
        }
    }

    pub fn complete(&mut self) {
        self.current_completions = self.completer.complete(&self.input, self.cursor_pos);
        if !self.current_completions.is_empty() {
            self.selected_completion = Some(0);
        } else {
            self.selected_completion = None;
        }
    }

    pub fn handle_tab(&mut self) {
        let already_visible = !self.current_completions.is_empty();
        if !already_visible {
            self.complete();
        }

        if self.current_completions.is_empty() {
            return;
        }

        if self.current_completions.len() == 1 {
            self.selected_completion = Some(0);
            self.apply_completion_internal(true);
            return;
        }

        if self.selected_completion.is_none() {
            self.selected_completion = Some(0);
        }

        if self.apply_common_prefix() {
            return;
        }

        if already_visible {
            self.next_completion();
        }
        self.apply_completion_internal(false);
    }

    pub fn next_completion(&mut self) {
        if let Some(idx) = self.selected_completion {
            let new_idx = (idx + 1) % self.current_completions.len();
            self.selected_completion = Some(new_idx);
        }
    }

    pub fn apply_completion(&mut self) {
        self.apply_completion_internal(true);
    }

    fn apply_completion_internal(&mut self, clear_after: bool) {
        if let Some(idx) = self.selected_completion {
            if let Some(completion) = self.current_completions.get(idx) {
                let replacement = completion.text.clone();
                self.replace_current_token(&replacement);
            }
        }

        if clear_after {
            self.current_completions.clear();
            self.selected_completion = None;
        }
    }

    fn replace_current_token(&mut self, replacement: &str) {
        let before_cursor = &self.input[..self.cursor_pos];

        if self.input.is_empty() || (before_cursor == self.input && !before_cursor.contains(' ')) {
            self.input = replacement.to_string();
        } else if before_cursor.ends_with(' ') {
            self.input.push_str(replacement);
        } else {
            let last_word_start = before_cursor.rfind(' ').map(|i| i + 1).unwrap_or(0);
            let after_cursor = &self.input[self.cursor_pos..];
            self.input = format!(
                "{}{}{}",
                &before_cursor[..last_word_start],
                replacement,
                after_cursor
            );
        }
        self.cursor_pos = self.input.len();
    }

    fn apply_common_prefix(&mut self) -> bool {
        if self.current_completions.is_empty() {
            return false;
        }

        let mut prefix = self.current_completions[0].text.clone();
        for completion in &self.current_completions[1..] {
            prefix = common_prefix(&prefix, &completion.text);
            if prefix.is_empty() {
                break;
            }
        }

        if prefix.is_empty() {
            return false;
        }

        let before_cursor = &self.input[..self.cursor_pos];
        let last_word_start = before_cursor.rfind(' ').map(|i| i + 1).unwrap_or(0);
        let current_token = &before_cursor[last_word_start..];

        if prefix.len() > current_token.len() && prefix.starts_with(current_token) {
            self.replace_current_token(&prefix);
            true
        } else {
            false
        }
    }

    pub fn cancel_completion(&mut self) {
        self.current_completions.clear();
        self.selected_completion = None;
    }

    pub fn toggle_sidebar(&mut self) {
        self.show_sidebar = !self.show_sidebar;
    }

    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        self.terminal_height = area.height;

        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(8),
                Constraint::Min(5),
                Constraint::Length(1),
                Constraint::Length(3),
            ])
            .split(area);

        self.draw_banner(frame, main_layout[0]);

        let message_area = if self.show_sidebar {
            let h_split = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(20), Constraint::Length(24)])
                .split(main_layout[1]);
            self.draw_sidebar(frame, h_split[1]);
            h_split[0]
        } else {
            main_layout[1]
        };
        self.draw_messages(frame, message_area);

        self.draw_status_bar(frame, main_layout[2]);
        self.draw_input(frame, main_layout[3]);
    }

    fn draw_banner(&self, frame: &mut Frame<'_>, area: Rect) {
        let art = [
            " ██████  ██████   ██████  ██   ██ ███████ ██████",
            " ██   ██ ██   ██ ██    ██ ██  ██  ██      ██   ██",
            " ██████  ██████  ██    ██ █████   █████   ██████",
            " ██   ██ ██   ██ ██    ██ ██  ██  ██      ██   ██",
            " ██████  ██   ██  ██████  ██   ██ ███████ ██   ██",
        ];

        let meta_text = "https://github.com/aszazeroth/rustynaut";
        let version_text = format!(" {}", BROKER_VERSION);

        let max_art_width = art.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let meta_width = meta_text.len() + version_text.len();
        let content_width = max_art_width.max(meta_width);

        let area_width = area.width as usize;
        let left_pad = area_width.saturating_sub(content_width) / 2;

        let mut lines: Vec<Line<'_>> = Vec::new();

        const TOP_PADDING_LINES: usize = 1;
        for _ in 0..TOP_PADDING_LINES {
            lines.push(Line::from(Span::raw("")));
        }

        for line in art {
            lines.push(Line::from(Span::styled(
                format!("{:pad$}{}", "", line, pad = left_pad),
                Style::default().fg(COLOR_ACCENT),
            )));
        }

        lines.push(Line::from(vec![
            Span::styled(
                format!("{:pad$}{}", "", meta_text, pad = left_pad),
                Style::default().fg(COLOR_FG),
            ),
            Span::styled(version_text, Style::default().fg(COLOR_HILITE)),
        ]));

        let banner = Paragraph::new(lines)
            .alignment(ratatui::layout::Alignment::Left)
            .style(Style::default().bg(COLOR_BG))
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .style(Style::default().fg(COLOR_ACCENT)),
            );

        frame.render_widget(banner, area);
    }

    fn draw_messages(&self, frame: &mut Frame<'_>, area: Rect) {
        let items: Vec<ListItem<'_>> = self
            .messages
            .iter()
            .map(|msg| self.format_message(msg))
            .collect();

        let visible_count = (area.height as usize).saturating_sub(2);
        let start = self
            .scroll_offset
            .saturating_sub(visible_count.saturating_sub(1));
        let end = (start + visible_count).min(items.len());
        let visible_items: Vec<_> = items[start..end].to_vec();

        let messages_list = List::new(visible_items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Broker Logs")
                    .style(Style::default().fg(COLOR_ACCENT)),
            )
            .style(Style::default().fg(COLOR_FG).bg(COLOR_BG_SOFT));

        frame.render_widget(messages_list, area);
    }

    fn format_message<'a>(&self, msg: &'a Message) -> ListItem<'a> {
        let timestamp = match msg {
            Message::Info { timestamp, .. } => timestamp,
            Message::Error { timestamp, .. } => timestamp,
            Message::Stats { .. } => return ListItem::new(Line::from("")),
        };
        let ts_str = Self::format_timestamp(timestamp);
        let ts_span = Span::styled(
            format!("[{}] ", ts_str),
            Style::default().fg(Color::Rgb(0x58, 0x6E, 0x75)),
        );

        match msg {
            Message::Info { text, .. } => {
                let spans = vec![
                    ts_span,
                    Span::styled("INFO ", Style::default().fg(COLOR_ACCENT)),
                    Span::raw(text.clone()),
                ];
                ListItem::new(Line::from(spans))
            }
            Message::Error { text, .. } => {
                let spans = vec![
                    ts_span,
                    Span::styled("ERR  ", Style::default().fg(COLOR_ERR)),
                    Span::raw(text.clone()),
                ];
                ListItem::new(Line::from(spans))
            }
            Message::Stats { .. } => ListItem::new(Line::from("")),
        }
    }

    fn draw_sidebar(&self, frame: &mut Frame<'_>, area: Rect) {
        let room_items: Vec<ListItem<'_>> = self
            .rooms
            .iter()
            .map(|room| ListItem::new(room.as_str()))
            .collect();

        let room_list = List::new(room_items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Rooms")
                    .style(Style::default().fg(COLOR_ACCENT)),
            )
            .style(Style::default().fg(COLOR_FG).bg(COLOR_BG_SOFT));

        frame.render_widget(room_list, area);
    }

    fn draw_status_bar(&self, frame: &mut Frame<'_>, area: Rect) {
        let status_text = format!(
            " Addr: {} | TLS: {} | Clients: {} | Rooms: {} | Scroll: {} ",
            self.bind_addr,
            if self.tls_enabled { "Y" } else { "N" },
            self.peer_count,
            self.rooms.len(),
            if self.scroll_offset == self.messages.len().saturating_sub(1) {
                "bottom".to_string()
            } else {
                format!("{}/{}", self.scroll_offset + 1, self.messages.len())
            }
        );

        let status = Paragraph::new(status_text).style(Style::default().fg(COLOR_FG).bg(COLOR_BG));
        frame.render_widget(status, area);
    }

    fn draw_input(&self, frame: &mut Frame<'_>, area: Rect) {
        let prompt = "broker> ";

        let mut spans = vec![Span::styled(
            prompt.to_string(),
            Style::default().fg(COLOR_ACCENT),
        )];

        if self.input.is_empty() {
            spans.push(Span::styled(" ", Style::default().bg(COLOR_BG)));
        } else {
            if self.cursor_pos < self.input.len() {
                let before = &self.input[..self.cursor_pos];
                let at_cursor = &self.input[self.cursor_pos..self.cursor_pos + 1];
                let after = &self.input[self.cursor_pos + 1..];

                spans.push(Span::raw(before.to_string()));
                spans.push(Span::styled(
                    at_cursor,
                    Style::default().bg(COLOR_FG).fg(COLOR_BG_SOFT),
                ));
                spans.push(Span::raw(after.to_string()));
            } else {
                spans.push(Span::raw(self.input.clone()));
                spans.push(Span::styled(" ", Style::default().bg(COLOR_BG)));
            }
        }

        let input = Paragraph::new(Line::from(spans))
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: true });

        frame.render_widget(input, area);

        if !self.current_completions.is_empty() {
            let completion_area = Rect {
                x: area.x,
                y: area
                    .y
                    .saturating_sub(self.current_completions.len().min(5) as u16 + 1),
                width: area.width,
                height: self.current_completions.len().min(5) as u16 + 1,
            };
            self.draw_completion_menu(frame, completion_area);
        }
    }

    fn draw_completion_menu(&self, frame: &mut Frame<'_>, area: Rect) {
        let items: Vec<ListItem<'_>> = self
            .current_completions
            .iter()
            .enumerate()
            .map(|(idx, completion)| {
                let is_selected = Some(idx) == self.selected_completion;
                let style = if is_selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };

                let text = format!("{} - {}", completion.display, completion.description);
                ListItem::new(text).style(style)
            })
            .collect();

        let title = format!("Completions ({})", self.current_completions.len());
        let completion_list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .style(Style::default().fg(COLOR_ACCENT)),
            )
            .style(Style::default().fg(COLOR_FG).bg(COLOR_BG_SOFT))
            .highlight_style(Style::default().add_modifier(Modifier::BOLD));

        frame.render_widget(completion_list, area);
    }
}

fn common_prefix(left: &str, right: &str) -> String {
    let mut result = String::new();
    for (l, r) in left.chars().zip(right.chars()) {
        if l == r {
            result.push(l);
        } else {
            break;
        }
    }
    result
}

pub fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
}

pub fn restore_terminal() -> io::Result<()> {
    crossterm::terminal::disable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen
    )?;
    Ok(())
}
