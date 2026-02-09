//! Tab completion module for the Rustynaut TUI client.
//!
//! Provides context-aware completions for:
//! - Slash commands (/help, /join, /accept, etc.)
//! - Room names (for /join)
//! - Usernames (for /accept)
//! - Filenames (for /accept <user>)
//! - Transfer IDs (for /cancel)

use std::collections::HashMap;

/// Context for completion - tracks known entities
#[derive(Clone, Debug, Default)]
pub struct CompletionContext {
    /// Known room names
    pub known_rooms: Vec<String>,
    /// Users currently in the room
    pub users_in_room: Vec<String>,
    /// Pending file offers: username -> list of (filename, size)
    pub pending_offers: HashMap<String, Vec<(String, String)>>,
    /// Active transfer IDs
    pub active_transfers: Vec<u64>,
    /// Available slash commands
    pub commands: Vec<String>,
}

impl CompletionContext {
    /// Create a new completion context with default commands
    pub fn new() -> Self {
        Self {
            known_rooms: Vec::new(),
            users_in_room: Vec::new(),
            pending_offers: HashMap::new(),
            active_transfers: Vec::new(),
            commands: vec![
                "/help".to_string(),
                "/rooms".to_string(),
                "/who".to_string(),
                "/join".to_string(),
                "/offers".to_string(),
                "/accept".to_string(),
                "/cancel".to_string(),
                "/quit".to_string(),
                "/exit".to_string(),
            ],
        }
    }

    /// Add a room to known rooms
    pub fn add_room(&mut self, room: String) {
        if !self.known_rooms.contains(&room) {
            self.known_rooms.push(room);
        }
    }

    /// Add a user to the room
    pub fn add_user(&mut self, user: String) {
        if !self.users_in_room.contains(&user) {
            self.users_in_room.push(user);
        }
    }

    /// Remove a user from the room
    pub fn remove_user(&mut self, user: &str) {
        self.users_in_room.retain(|u| u != user);
        // Also remove their pending offers
        self.pending_offers.remove(user);
    }

    /// Add a file offer
    pub fn add_file_offer(&mut self, user: String, filename: String, size: String) {
        let offers = self.pending_offers.entry(user).or_default();
        // Don't add duplicates
        if !offers.iter().any(|(f, _)| f == &filename) {
            offers.push((filename, size));
        }
    }

    /// Remove a file offer (when accepted or cancelled)
    #[allow(dead_code)]
    pub fn remove_file_offer(&mut self, user: &str, filename: &str) {
        if let Some(offers) = self.pending_offers.get_mut(user) {
            offers.retain(|(f, _)| f != filename);
            if offers.is_empty() {
                self.pending_offers.remove(user);
            }
        }
    }

    /// Add an active transfer
    pub fn add_transfer(&mut self, id: u64) {
        if !self.active_transfers.contains(&id) {
            self.active_transfers.push(id);
        }
    }

    /// Remove an active transfer
    pub fn remove_transfer(&mut self, id: u64) {
        self.active_transfers.retain(|&t| t != id);
    }
}

/// A single completion suggestion
#[derive(Clone, Debug)]
pub struct Completion {
    /// The text to insert
    pub text: String,
    /// Display text (may include formatting)
    pub display: String,
    /// Description of what this completion is
    pub description: String,
}

/// The completer struct that generates completions
pub struct Completer {
    context: CompletionContext,
}

impl Completer {
    /// Create a new completer
    pub fn new() -> Self {
        Self {
            context: CompletionContext::new(),
        }
    }

    /// Get mutable access to the context
    pub fn context_mut(&mut self) -> &mut CompletionContext {
        &mut self.context
    }

    /// Get completions for the current input
    pub fn complete(&self, input: &str, cursor_pos: usize) -> Vec<Completion> {
        // Determine what we're completing based on cursor position and content
        let before_cursor = &input[..cursor_pos];
        let parts: Vec<&str> = before_cursor.split_whitespace().collect();

        if parts.is_empty() {
            // Completing at the start - suggest commands
            return self.complete_commands("");
        }

        let first = parts[0];

        if !first.starts_with('/') {
            // Not a command, no completions
            return Vec::new();
        }

        // Check if we're completing the command itself or its arguments
        if parts.len() == 1 && !before_cursor.ends_with(' ') {
            // Completing the command name
            let cmd_prefix = &first[1..]; // Remove leading /
            return self.complete_commands(cmd_prefix);
        }

        // Completing arguments
        match first {
            "/join" => {
                let prefix = parts.get(1).copied().unwrap_or("");
                self.complete_rooms(prefix)
            }
            "/accept" => {
                if parts.len() == 1 {
                    // No args yet, suggest users with offers
                    self.complete_users("")
                } else if parts.len() == 2 && !before_cursor.ends_with(' ') {
                    // Completing username
                    let prefix = parts[1];
                    self.complete_users(prefix)
                } else if parts.len() >= 2 {
                    // Completing filename for a user
                    let user = parts[1];
                    let prefix = parts.get(2).copied().unwrap_or("");
                    self.complete_filenames(user, prefix)
                } else {
                    Vec::new()
                }
            }
            "/cancel" => {
                let prefix = parts.get(1).copied().unwrap_or("");
                self.complete_transfers(prefix)
            }
            _ => Vec::new(),
        }
    }

    /// Complete command names
    fn complete_commands(&self, prefix: &str) -> Vec<Completion> {
        self.context
            .commands
            .iter()
            .filter(|cmd| cmd[1..].starts_with(prefix))
            .map(|cmd| Completion {
                text: cmd.clone(),
                display: cmd.clone(),
                description: self.command_description(cmd),
            })
            .collect()
    }

    /// Complete room names
    fn complete_rooms(&self, prefix: &str) -> Vec<Completion> {
        self.context
            .known_rooms
            .iter()
            .filter(|room| room.starts_with(prefix))
            .map(|room| Completion {
                text: room.clone(),
                display: room.clone(),
                description: "Room".to_string(),
            })
            .collect()
    }

    /// Complete usernames
    fn complete_users(&self, prefix: &str) -> Vec<Completion> {
        let mut matches: Vec<Completion> = self
            .context
            .pending_offers
            .keys()
            .filter(|user| user.starts_with(prefix))
            .map(|user| Completion {
                text: user.clone(),
                display: user.clone(),
                description: "User (has offer)".to_string(),
            })
            .collect();

        if matches.is_empty() {
            matches = self
                .context
                .users_in_room
                .iter()
                .filter(|user| user.starts_with(prefix))
                .map(|user| Completion {
                    text: user.clone(),
                    display: user.clone(),
                    description: "User".to_string(),
                })
                .collect();
        }

        matches
    }

    /// Complete filenames for a specific user
    fn complete_filenames(&self, user: &str, prefix: &str) -> Vec<Completion> {
        self.context
            .pending_offers
            .get(user)
            .map(|offers| {
                offers
                    .iter()
                    .filter(|(filename, _)| filename.starts_with(prefix))
                    .map(|(filename, size)| Completion {
                        text: filename.clone(),
                        display: format!("{} ({})", filename, size),
                        description: "File".to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Complete transfer IDs
    fn complete_transfers(&self, prefix: &str) -> Vec<Completion> {
        self.context
            .active_transfers
            .iter()
            .filter(|&&id| id.to_string().starts_with(prefix))
            .map(|&id| Completion {
                text: id.to_string(),
                display: format!("Transfer {}", id),
                description: "Transfer ID".to_string(),
            })
            .collect()
    }

    /// Get description for a command
    fn command_description(&self, cmd: &str) -> String {
        match cmd {
            "/help" => "Show available commands".to_string(),
            "/rooms" => "List active rooms".to_string(),
            "/who" => "Show users in current room".to_string(),
            "/join" => "Join a room".to_string(),
            "/offers" => "List pending file offers".to_string(),
            "/accept" => "Accept a file offer".to_string(),
            "/cancel" => "Cancel a file transfer".to_string(),
            "/quit" => "Exit the client".to_string(),
            "/exit" => "Exit the client".to_string(),
            _ => "Command".to_string(),
        }
    }
}

impl Default for Completer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_complete_commands() {
        let completer = Completer::new();
        let completions = completer.complete_commands("ro");
        assert!(completions.iter().any(|c| c.text == "/rooms"));
    }

    #[test]
    fn test_complete_with_context() {
        let mut completer = Completer::new();
        completer.context_mut().add_room("lobby".to_string());
        completer.context_mut().add_room("testing".to_string());

        let completions = completer.complete("/join ", 6);
        assert!(completions.iter().any(|c| c.text == "lobby"));
    }
}
