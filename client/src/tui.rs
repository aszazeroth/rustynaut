//! TUI (Terminal User Interface) module for the Rustynaut client.
//!
//! Provides a ratatui-based interface with:
//! - Scrollable message history
//! - Input prompt with tab completion support
//! - Status bar showing room and users
//! - Color-coded message types

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

use crate::completion::{Completer, Completion, CompletionContext};

const CLIENT_VERSION: &str = "0.1.0-dev";

const COLOR_BG: Color = Color::Rgb(0x07, 0x36, 0x42);
const COLOR_BG_SOFT: Color = Color::Rgb(0x00, 0x2B, 0x36);
const COLOR_FG: Color = Color::Rgb(0x93, 0xA1, 0xA1);
const COLOR_ACCENT: Color = Color::Rgb(0x2A, 0xA1, 0x98);
const COLOR_HILITE: Color = Color::Rgb(0xB5, 0x89, 0x00);
const COLOR_ERR: Color = Color::Rgb(0xDC, 0x32, 0x2F);

/// A single message in the chat history with timestamp
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
    Chat {
        user: String,
        text: String,
        timestamp: SystemTime,
    },
    Clip {
        room: String,
        preview: String,
        timestamp: SystemTime,
    },
    FileOffer {
        room: String,
        user: String,
        filename: String,
        size: String,
        timestamp: SystemTime,
    },
    FileTransfer {
        text: String,
        timestamp: SystemTime,
    },
}

/// Application state for the TUI
pub struct App {
    /// Connection state info
    pub username: String,
    pub current_room: String,
    pub connected: bool,

    /// Chat history
    pub messages: Vec<Message>,

    /// Users in current room
    pub users_in_room: Vec<String>,

    /// Current input buffer
    pub input: String,

    /// Cursor position in input
    pub cursor_pos: usize,

    /// Scroll offset for message area
    pub scroll_offset: usize,

    /// Whether to show the sidebar
    pub show_sidebar: bool,

    /// Whether the app should quit
    pub should_quit: bool,

    /// Terminal height (for scroll calculations)
    pub terminal_height: u16,

    /// Command history for up/down navigation
    command_history: Vec<String>,

    /// Current position in history (None = not browsing history)
    history_index: Option<usize>,

    /// Maximum number of commands to keep in history
    max_history: usize,

    /// Tab completer
    completer: Completer,

    /// Current completions (if any)
    pub current_completions: Vec<Completion>,

    /// Selected completion index
    pub selected_completion: Option<usize>,
}

impl App {
    /// Create a new App instance
    pub fn new(username: String, room: String) -> Self {
        Self {
            username,
            current_room: room,
            connected: false,
            messages: Vec::new(),
            users_in_room: Vec::new(),
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

    /// Add a message to the history
    pub fn add_message(&mut self, msg: Message) {
        self.messages.push(msg);
        // Auto-scroll to bottom if user hasn't scrolled up
        if self.scroll_offset == 0 || self.scroll_offset >= self.messages.len().saturating_sub(10) {
            self.scroll_offset = self.messages.len().saturating_sub(1);
        }
    }

    /// Add a plain text info message
    pub fn add_info(&mut self, text: impl Into<String>) {
        self.add_message(Message::Info {
            text: text.into(),
            timestamp: SystemTime::now(),
        });
    }

    /// Add an error message
    pub fn add_error(&mut self, text: impl Into<String>) {
        self.add_message(Message::Error {
            text: text.into(),
            timestamp: SystemTime::now(),
        });
    }

    /// Add a chat message
    pub fn add_chat(&mut self, user: impl Into<String>, text: impl Into<String>) {
        self.add_message(Message::Chat {
            user: user.into(),
            text: text.into(),
            timestamp: SystemTime::now(),
        });
    }

    /// Handle an info message and update completion context
    pub fn handle_info(&mut self, text: &str) {
        let text = text.strip_prefix("INFO ").unwrap_or(text);
        if let Some(room) = text.strip_prefix("joined ") {
            self.handle_room_join(room.trim());
        }
        // Track user join/leave messages
        if let Some((user, is_join)) = extract_user_from_join_msg(text) {
            if is_join {
                self.completion_context_mut().add_user(user.to_string());
            } else {
                self.completion_context_mut().remove_user(user);
            }
        }
        self.add_info(text);
    }

    /// Handle a file offer and update completion context
    pub fn handle_file_offer(&mut self, room: &str, user: &str, filename: &str, size: &str) {
        self.completion_context_mut().add_user(user.to_string());
        self.completion_context_mut().add_file_offer(
            user.to_string(),
            filename.to_string(),
            size.to_string(),
        );
        self.add_message(Message::FileOffer {
            room: room.to_string(),
            user: user.to_string(),
            filename: filename.to_string(),
            size: size.to_string(),
            timestamp: SystemTime::now(),
        });
    }

    /// Handle a file transfer message and update completion context
    pub fn handle_file_transfer(&mut self, text: &str) {
        // Extract transfer ID if present
        if let Some(id) = extract_transfer_id(text) {
            if text.contains("started") || text.contains("Receiving") {
                self.completion_context_mut().add_transfer(id);
            } else if text.contains("cancelled") || text.contains("failed") {
                self.completion_context_mut().remove_transfer(id);
            }
        }
        self.add_message(Message::FileTransfer {
            text: text.to_string(),
            timestamp: SystemTime::now(),
        });
    }

    /// Handle a clip message
    pub fn handle_clip(&mut self, room: &str, preview: &str) {
        self.add_message(Message::Clip {
            room: room.to_string(),
            preview: preview.to_string(),
            timestamp: SystemTime::now(),
        });
    }

    /// Update context when joining a room
    pub fn handle_room_join(&mut self, room: &str) {
        self.current_room = room.to_string();
        self.completion_context_mut().add_room(room.to_string());
    }

    /// Format a timestamp as HH:MM:SS
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

    /// Handle input character
    pub fn handle_input(&mut self, c: char) -> Option<String> {
        if c == '\n' {
            // Submit command
            let cmd = self.input.clone();
            if !cmd.is_empty() {
                self.add_to_history(&cmd);
                self.input.clear();
                self.cursor_pos = 0;
                self.history_index = None;
                return Some(cmd);
            }
        } else {
            // Insert character at cursor
            self.input.insert(self.cursor_pos, c);
            self.cursor_pos += 1;
            // Exit history browsing mode when typing
            self.history_index = None;
            // Clear completions when typing
            self.current_completions.clear();
            self.selected_completion = None;
        }
        None
    }

    /// Handle backspace
    pub fn handle_backspace(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
            self.input.remove(self.cursor_pos);
        }
    }

    /// Handle delete key
    pub fn handle_delete(&mut self) {
        if self.cursor_pos < self.input.len() {
            self.input.remove(self.cursor_pos);
        }
    }

    /// Move cursor left
    pub fn cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
        }
    }

    /// Move cursor right
    pub fn cursor_right(&mut self) {
        if self.cursor_pos < self.input.len() {
            self.cursor_pos += 1;
        }
    }

    /// Move cursor to start of line
    pub fn cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    /// Move cursor to end of line
    pub fn cursor_end(&mut self) {
        self.cursor_pos = self.input.len();
    }

    /// Scroll up in message history
    pub fn scroll_up(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset -= 1;
        }
    }

    /// Scroll down in message history
    pub fn scroll_down(&mut self) {
        if self.scroll_offset < self.messages.len().saturating_sub(1) {
            self.scroll_offset += 1;
        }
    }

    /// Navigate to previous command in history (Up arrow)
    pub fn history_previous(&mut self) {
        if self.command_history.is_empty() {
            return;
        }

        match self.history_index {
            None => {
                // Start from the most recent command
                let last_idx = self.command_history.len() - 1;
                self.history_index = Some(last_idx);
                self.input = self.command_history[last_idx].clone();
                self.cursor_pos = self.input.len();
            }
            Some(idx) if idx > 0 => {
                // Go to older command
                let new_idx = idx - 1;
                self.history_index = Some(new_idx);
                self.input = self.command_history[new_idx].clone();
                self.cursor_pos = self.input.len();
            }
            _ => {
                // Already at oldest command, do nothing
            }
        }
    }

    /// Navigate to next command in history (Down arrow)
    pub fn history_next(&mut self) {
        match self.history_index {
            None => {
                // Not browsing history, do nothing
            }
            Some(idx) => {
                let history_len = self.command_history.len();
                if idx + 1 < history_len {
                    // Go to newer command
                    let new_idx = idx + 1;
                    self.history_index = Some(new_idx);
                    self.input = self.command_history[new_idx].clone();
                    self.cursor_pos = self.input.len();
                } else {
                    // Exit history browsing, clear input
                    self.history_index = None;
                    self.input.clear();
                    self.cursor_pos = 0;
                }
            }
        }
    }

    /// Add a command to history
    fn add_to_history(&mut self, cmd: &str) {
        // Don't add empty commands or duplicates of the most recent command
        if cmd.is_empty() {
            return;
        }
        if let Some(last) = self.command_history.last() {
            if last == cmd {
                return;
            }
        }

        self.command_history.push(cmd.to_string());

        // Limit history size
        if self.command_history.len() > self.max_history {
            self.command_history.remove(0);
        }
    }

    /// Get mutable access to completion context
    pub fn completion_context_mut(&mut self) -> &mut CompletionContext {
        self.completer.context_mut()
    }

    /// Trigger tab completion
    pub fn complete(&mut self) {
        self.current_completions = self.completer.complete(&self.input, self.cursor_pos);
        if !self.current_completions.is_empty() {
            self.selected_completion = Some(0);
        } else {
            self.selected_completion = None;
        }
    }

    /// Handle tab key: apply completion or show menu
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

    /// Cycle to next completion
    pub fn next_completion(&mut self) {
        if let Some(idx) = self.selected_completion {
            let new_idx = (idx + 1) % self.current_completions.len();
            self.selected_completion = Some(new_idx);
        }
    }

    /// Apply the currently selected completion
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

    /// Cancel completion mode
    pub fn cancel_completion(&mut self) {
        self.current_completions.clear();
        self.selected_completion = None;
    }

    /// Toggle sidebar visibility
    pub fn toggle_sidebar(&mut self) {
        self.show_sidebar = !self.show_sidebar;
    }

    /// Draw the UI
    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        self.terminal_height = area.height;

        // Split layout: banner | messages [sidebar] | status | input
        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(8), // Banner
                Constraint::Min(5),    // Messages (flexible)
                Constraint::Length(1), // Status bar
                Constraint::Length(3), // Input area
            ])
            .split(area);

        // Banner area
        self.draw_banner(frame, main_layout[0]);

        // Messages area - split horizontally if sidebar is shown
        let message_area = if self.show_sidebar {
            let h_split = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(20), Constraint::Length(20)])
                .split(main_layout[1]);
            self.draw_sidebar(frame, h_split[1]);
            h_split[0]
        } else {
            main_layout[1]
        };
        self.draw_messages(frame, message_area);

        // Status bar
        self.draw_status_bar(frame, main_layout[2]);

        // Input area
        self.draw_input(frame, main_layout[3]);
    }


    /// Draw the ASCII banner (explicit top padding, flat ANSI, no shadow)
fn draw_banner(&self, frame: &mut Frame<'_>, area: Rect) {
    // ORIGINAL art — do NOT modify spacing
    let art = [
        " ██████ ██      ██ ███████ ███    ██ ████████",
        "██      ██      ██ ██      ████   ██    ██",
        "██      ██      ██ █████   ██ ██  ██    ██",
        "██      ██      ██ ██      ██  ██ ██    ██",
        " ██████ ███████ ██ ███████ ██   ████    ██",
    ];

    let meta_text = "https://github.com/aszazeroth/rustynaut";
    let version_text = format!(" {}", CLIENT_VERSION);

    let max_art_width = art.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let meta_width = meta_text.len() + version_text.len();
    let content_width = max_art_width.max(meta_width);

    let area_width = area.width as usize;
    let left_pad = area_width.saturating_sub(content_width) / 2;

    let mut lines: Vec<Line<'_>> = Vec::new();

    // ---- EXPLICIT vertical padding (this is what you were missing) ----
    const TOP_PADDING_LINES: usize = 1;

    for _ in 0..TOP_PADDING_LINES {
        lines.push(Line::from(Span::raw("")));
    }

    // ---- ASCII art (flat, single color, no shadow) ----
    for line in art {
        lines.push(Line::from(Span::styled(
            format!("{:pad$}{}", "", line, pad = left_pad),
            Style::default().fg(COLOR_ACCENT),
        )));
    }

    // Tight spacing before URL (intentional)
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


    /// Draw the scrollable message area
    fn draw_messages(&self, frame: &mut Frame<'_>, area: Rect) {
        // Convert messages to list items with styling
        let items: Vec<ListItem<'_>> = self
            .messages
            .iter()
            .map(|msg| self.format_message(msg))
            .collect();

        // Calculate visible range based on scroll offset
        let visible_count = (area.height as usize).saturating_sub(2); // Account for borders
        let start = self
            .scroll_offset
            .saturating_sub(visible_count.saturating_sub(1));
        let end = (start + visible_count).min(items.len());
        let visible_items: Vec<_> = items[start..end].to_vec();

        let messages_list = List::new(visible_items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Messages")
                    .style(Style::default().fg(COLOR_ACCENT)),
            )
            .style(Style::default().fg(COLOR_FG).bg(COLOR_BG_SOFT));

        frame.render_widget(messages_list, area);
    }

    /// Format a message for display
    fn format_message<'a>(&self, msg: &'a Message) -> ListItem<'a> {
        let timestamp = match msg {
            Message::Info { timestamp, .. } => timestamp,
            Message::Error { timestamp, .. } => timestamp,
            Message::Chat { timestamp, .. } => timestamp,
            Message::Clip { timestamp, .. } => timestamp,
            Message::FileOffer { timestamp, .. } => timestamp,
            Message::FileTransfer { timestamp, .. } => timestamp,
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
            Message::Chat { user, text, .. } => {
                let spans = vec![
                    ts_span,
                    Span::styled(format!("{}: ", user), Style::default().fg(COLOR_HILITE)),
                    Span::raw(text.clone()),
                ];
                ListItem::new(Line::from(spans))
            }
            Message::Clip { room, preview, .. } => {
                let spans = vec![
                    ts_span,
                    Span::styled("CLIP ", Style::default().fg(Color::Rgb(0x26, 0x8B, 0xD2))),
                    Span::styled(format!("[{}] ", room), Style::default().fg(COLOR_ACCENT)),
                    Span::raw(preview.clone()),
                ];
                ListItem::new(Line::from(spans))
            }
            Message::FileOffer {
                room,
                user,
                filename,
                size,
                ..
            } => {
                let spans = vec![
                    ts_span,
                    Span::styled("FILE ", Style::default().fg(Color::Rgb(0x6C, 0x71, 0xC4))),
                    Span::styled(format!("[{}] ", room), Style::default().fg(COLOR_ACCENT)),
                    Span::styled(format!("{} ", user), Style::default().fg(COLOR_HILITE)),
                    Span::raw(format!("offers: {} ({})", filename, size)),
                ];
                ListItem::new(Line::from(spans))
            }
            Message::FileTransfer { text, .. } => {
                let spans = vec![
                    ts_span,
                    Span::styled("FILE ", Style::default().fg(Color::Rgb(0x6C, 0x71, 0xC4))),
                    Span::raw(text.clone()),
                ];
                ListItem::new(Line::from(spans))
            }
        }
    }

    /// Draw the sidebar showing users
    fn draw_sidebar(&self, frame: &mut Frame<'_>, area: Rect) {
        let user_items: Vec<ListItem<'_>> = self
            .users_in_room
            .iter()
            .map(|u| ListItem::new(u.as_str()))
            .collect();

        let user_list = List::new(user_items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Users")
                    .style(Style::default().fg(COLOR_ACCENT)),
            )
            .style(Style::default().fg(COLOR_FG).bg(COLOR_BG_SOFT));

        frame.render_widget(user_list, area);
    }

    /// Draw the status bar
    fn draw_status_bar(&self, frame: &mut Frame<'_>, area: Rect) {
        let status_text = format!(
            " Room: {} | User: {} | Connected: {} | Users: {} | Scroll: {} ",
            self.current_room,
            self.username,
            if self.connected { "Y" } else { "N" },
            self.users_in_room.len(),
            if self.scroll_offset == self.messages.len().saturating_sub(1) {
                "bottom".to_string()
            } else {
                format!("{}/{}", self.scroll_offset + 1, self.messages.len())
            }
        );

        let status = Paragraph::new(status_text).style(Style::default().fg(COLOR_FG).bg(COLOR_BG));

        frame.render_widget(status, area);
    }

    /// Draw the input area
    fn draw_input(&self, frame: &mut Frame<'_>, area: Rect) {
        // Split into prompt and input
        let prompt = format!("[{}] > ", self.current_room);

        // Create text with cursor indicator
        let mut spans = vec![Span::styled(
            prompt.clone(),
            Style::default().fg(COLOR_ACCENT),
        )];

        // Add input text
        if self.input.is_empty() {
            spans.push(Span::styled(" ", Style::default().bg(COLOR_BG)));
        } else {
            // Show text with cursor position
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

        // Draw completion menu if active
        if !self.current_completions.is_empty() {
            // Calculate area for completion menu (below input)
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

    /// Draw the completion menu
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

/// Extract username from join messages like "[lobby] alice joined"
fn extract_user_from_join_msg(text: &str) -> Option<(&str, bool)> {
    // Pattern: "[room] user joined" or "[room] user left"
    if let Some(rest) = text.strip_prefix('[') {
        if let Some(space_pos) = rest.find(' ') {
            let room_and_rest = &rest[space_pos + 1..];
            if let Some(joined_pos) = room_and_rest.find(" joined") {
                return Some((&room_and_rest[..joined_pos], true));
            }
            if let Some(left_pos) = room_and_rest.find(" left") {
                return Some((&room_and_rest[..left_pos], false));
            }
        }
    }

    // Pattern: "user joined" or "user left"
    if let Some(joined_pos) = text.find(" joined") {
        return Some((&text[..joined_pos], true));
    }
    if let Some(left_pos) = text.find(" left") {
        return Some((&text[..left_pos], false));
    }

    None
}

/// Extract transfer ID from messages
fn extract_transfer_id(text: &str) -> Option<u64> {
    // Look for "transfer_id=N" or "transfer N"
    if let Some(id_start) = text.find("transfer_id=") {
        let after_eq = &text[id_start + 12..];
        if let Some(end) = after_eq.find(|c: char| !c.is_ascii_digit()) {
            return after_eq[..end].parse().ok();
        } else {
            return after_eq.parse().ok();
        }
    }
    if let Some(space_pos) = text.find(" transfer ") {
        let after_space = &text[space_pos + 10..];
        if let Some(end) = after_space.find(|c: char| !c.is_ascii_digit() && c != ')') {
            return after_space[..end].trim().parse().ok();
        }
    }
    None
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

/// Initialize the terminal
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

/// Restore the terminal to normal state
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
