use base64::{engine::general_purpose::STANDARD, Engine as _};

use crate::constants::{FILE_CHUNK_SIZE, MAX_FILE_SIZE};
use crate::protocol::{
    CMD_PREFIX, ENROLL_PREFIX, FILE_CANCELLED_PREFIX, FILE_CANCEL_PREFIX, JOIN_PREFIX, SAY_PREFIX,
    USER_PREFIX,
};
use crate::types::{FileOffset, TransferId};

const MAX_NAME_LENGTH: usize = 64;
const MAX_ROOM_LENGTH: usize = 64;
const SHA256_HEX_LENGTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolParseError {
    Empty,
    UnknownCommand,
    MissingField(&'static str),
    InvalidField {
        field: &'static str,
        reason: &'static str,
    },
    TrailingFields,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage<'a> {
    User {
        name: &'a str,
    },
    Join {
        room: &'a str,
    },
    Clip {
        room: &'a str,
        b64: &'a str,
    },
    Cmd {
        command: &'a str,
    },
    Say {
        text: &'a str,
    },
    Enroll {
        token: &'a str,
        username: &'a str,
    },
    FileOffer {
        room: &'a str,
        filename_b64: &'a str,
        size: u64,
    },
    FileAccept {
        room: &'a str,
        sender_username: &'a str,
        filename_b64: &'a str,
    },
    FileCancel {
        transfer_id: TransferId,
    },
    FileChunk {
        transfer_id: TransferId,
        offset: FileOffset,
        chunk_b64: &'a str,
    },
    FileEnd {
        transfer_id: TransferId,
        sha256: &'a str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerMessage<'a> {
    Info {
        text: &'a str,
    },
    Err {
        text: &'a str,
    },
    Clip {
        room: &'a str,
        b64: &'a str,
        id: Option<u64>,
    },
    Say {
        user: &'a str,
        text: &'a str,
    },
    Enrolled {
        cert_b64: &'a str,
        key_b64: &'a str,
        ca_b64: &'a str,
    },
    FileOffer {
        room: &'a str,
        username: &'a str,
        filename_b64: &'a str,
        size: u64,
    },
    FileStart {
        transfer_id: TransferId,
        filename_b64: &'a str,
        acceptor_count: usize,
    },
    FileIncoming {
        transfer_id: TransferId,
        filename_b64: &'a str,
        size: u64,
    },
    FileChunk {
        transfer_id: TransferId,
        offset: FileOffset,
        chunk_b64: &'a str,
    },
    FileDone {
        transfer_id: TransferId,
        sha256: &'a str,
    },
    FileSent {
        transfer_id: TransferId,
        count: usize,
    },
    FileCancelled {
        transfer_id: TransferId,
        reason: &'a str,
    },
}

pub fn parse_client_message(line: &str) -> Result<ClientMessage<'_>, ProtocolParseError> {
    if line.trim().is_empty() {
        return Err(ProtocolParseError::Empty);
    }

    let Some((command, rest)) = split_command(line) else {
        return Err(ProtocolParseError::UnknownCommand);
    };

    match command {
        "USER" => Ok(ClientMessage::User {
            name: validate_name(single_field(rest, "name")?, "name")?,
        }),
        "JOIN" => Ok(ClientMessage::Join {
            room: validate_room(single_field(rest, "room")?)?,
        }),
        "CLIP" => {
            let parts = exact_fields(rest, 2)?;
            let room = validate_room(parts[0])?;
            validate_base64(parts[1], "clipboard payload")?;
            Ok(ClientMessage::Clip {
                room,
                b64: parts[1],
            })
        }
        "CMD" => {
            let command = non_empty(rest.trim(), "command")?;
            if !command.starts_with('/') {
                return Err(invalid("command", "must start with '/'"));
            }
            Ok(ClientMessage::Cmd { command })
        }
        "SAY" => Ok(ClientMessage::Say {
            text: non_empty(rest.trim(), "text")?,
        }),
        "ENROLL" => {
            let parts = exact_fields(rest, 2)?;
            Ok(ClientMessage::Enroll {
                token: validate_token(parts[0])?,
                username: validate_name(parts[1], "username")?,
            })
        }
        "FILE_OFFER" => {
            let parts = exact_fields(rest, 3)?;
            let room = validate_room(parts[0])?;
            validate_base64(parts[1], "filename")?;
            let size = parse_size(parts[2])?;
            Ok(ClientMessage::FileOffer {
                room,
                filename_b64: parts[1],
                size,
            })
        }
        "FILE_ACCEPT" => {
            let parts = exact_fields(rest, 3)?;
            let room = validate_room(parts[0])?;
            let sender_username = validate_name(parts[1], "sender_username")?;
            validate_base64(parts[2], "filename")?;
            Ok(ClientMessage::FileAccept {
                room,
                sender_username,
                filename_b64: parts[2],
            })
        }
        "FILE_CANCEL" => Ok(ClientMessage::FileCancel {
            transfer_id: parse_transfer_id(single_field(rest, "transfer_id")?)?,
        }),
        "FILE_CHUNK" => {
            let parts = exact_fields(rest, 3)?;
            let transfer_id = parse_transfer_id(parts[0])?;
            let offset = parse_offset(parts[1])?;
            validate_chunk_base64(parts[2])?;
            Ok(ClientMessage::FileChunk {
                transfer_id,
                offset,
                chunk_b64: parts[2],
            })
        }
        "FILE_END" => {
            let parts = exact_fields(rest, 2)?;
            Ok(ClientMessage::FileEnd {
                transfer_id: parse_transfer_id(parts[0])?,
                sha256: validate_sha256(parts[1])?,
            })
        }
        _ => Err(ProtocolParseError::UnknownCommand),
    }
}

pub fn parse_broker_message(line: &str) -> Result<BrokerMessage<'_>, ProtocolParseError> {
    if line.trim().is_empty() {
        return Err(ProtocolParseError::Empty);
    }

    let Some((command, rest)) = split_command(line) else {
        return Err(ProtocolParseError::UnknownCommand);
    };

    match command {
        "INFO" => Ok(BrokerMessage::Info {
            text: non_empty(rest.trim(), "text")?,
        }),
        "ERR" => Ok(BrokerMessage::Err {
            text: non_empty(rest.trim(), "text")?,
        }),
        "CLIP" => {
            let parts = fields_between(rest, 2, 3)?;
            let room = validate_room(parts[0])?;
            validate_base64(parts[1], "clipboard payload")?;
            let id = match parts.get(2) {
                Some(value) => Some(parse_u64_field(value, "id")?),
                None => None,
            };
            Ok(BrokerMessage::Clip {
                room,
                b64: parts[1],
                id,
            })
        }
        "SAY" => {
            let (user, text) = split_once_required(rest, "user", "text")?;
            Ok(BrokerMessage::Say {
                user: validate_name(user, "user")?,
                text: non_empty(text.trim(), "text")?,
            })
        }
        "ENROLLED" => {
            let parts = exact_fields(rest, 3)?;
            validate_base64(parts[0], "cert")?;
            validate_base64(parts[1], "key")?;
            validate_base64(parts[2], "ca")?;
            Ok(BrokerMessage::Enrolled {
                cert_b64: parts[0],
                key_b64: parts[1],
                ca_b64: parts[2],
            })
        }
        "FILE_OFFER" => {
            let parts = exact_fields(rest, 4)?;
            let room = validate_room(parts[0])?;
            let username = validate_name(parts[1], "username")?;
            validate_base64(parts[2], "filename")?;
            let size = parse_size(parts[3])?;
            Ok(BrokerMessage::FileOffer {
                room,
                username,
                filename_b64: parts[2],
                size,
            })
        }
        "FILE_START" => {
            let parts = exact_fields(rest, 3)?;
            validate_base64(parts[1], "filename")?;
            Ok(BrokerMessage::FileStart {
                transfer_id: parse_transfer_id(parts[0])?,
                filename_b64: parts[1],
                acceptor_count: parse_usize_field(parts[2], "acceptor_count")?,
            })
        }
        "FILE_INCOMING" => {
            let parts = exact_fields(rest, 3)?;
            validate_base64(parts[1], "filename")?;
            Ok(BrokerMessage::FileIncoming {
                transfer_id: parse_transfer_id(parts[0])?,
                filename_b64: parts[1],
                size: parse_size(parts[2])?,
            })
        }
        "FILE_CHUNK" => {
            let parts = exact_fields(rest, 3)?;
            Ok(BrokerMessage::FileChunk {
                transfer_id: parse_transfer_id(parts[0])?,
                offset: parse_offset(parts[1])?,
                chunk_b64: validate_chunk_base64(parts[2])?,
            })
        }
        "FILE_DONE" => {
            let parts = exact_fields(rest, 2)?;
            Ok(BrokerMessage::FileDone {
                transfer_id: parse_transfer_id(parts[0])?,
                sha256: validate_sha256(parts[1])?,
            })
        }
        "FILE_SENT" => {
            let parts = exact_fields(rest, 2)?;
            Ok(BrokerMessage::FileSent {
                transfer_id: parse_transfer_id(parts[0])?,
                count: parse_usize_field(parts[1], "count")?,
            })
        }
        "FILE_CANCELLED" => {
            let (transfer_id, reason) = split_once_required(rest, "transfer_id", "reason")?;
            Ok(BrokerMessage::FileCancelled {
                transfer_id: parse_transfer_id(transfer_id)?,
                reason: non_empty(reason.trim(), "reason")?,
            })
        }
        _ => Err(ProtocolParseError::UnknownCommand),
    }
}

fn split_command(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim();
    let (command, rest) = trimmed.split_once(' ')?;
    Some((command, rest))
}

fn split_once_required<'a>(
    rest: &'a str,
    first_field: &'static str,
    second_field: &'static str,
) -> Result<(&'a str, &'a str), ProtocolParseError> {
    let (first, second) = rest
        .trim()
        .split_once(' ')
        .ok_or(ProtocolParseError::MissingField(second_field))?;
    let first = non_empty(first.trim(), first_field)?;
    let second = non_empty(second.trim(), second_field)?;
    Ok((first, second))
}

fn single_field<'a>(rest: &'a str, field: &'static str) -> Result<&'a str, ProtocolParseError> {
    let parts = exact_fields(rest, 1)?;
    non_empty(parts[0], field)
}

fn exact_fields(rest: &str, expected: usize) -> Result<Vec<&str>, ProtocolParseError> {
    let parts: Vec<_> = rest.split_whitespace().collect();
    if parts.len() < expected {
        return Err(ProtocolParseError::MissingField("field"));
    }
    if parts.len() > expected {
        return Err(ProtocolParseError::TrailingFields);
    }
    Ok(parts)
}

fn fields_between(rest: &str, min: usize, max: usize) -> Result<Vec<&str>, ProtocolParseError> {
    let parts: Vec<_> = rest.split_whitespace().collect();
    if parts.len() < min {
        return Err(ProtocolParseError::MissingField("field"));
    }
    if parts.len() > max {
        return Err(ProtocolParseError::TrailingFields);
    }
    Ok(parts)
}

fn non_empty<'a>(value: &'a str, field: &'static str) -> Result<&'a str, ProtocolParseError> {
    if value.is_empty() {
        return Err(ProtocolParseError::MissingField(field));
    }
    Ok(value)
}

fn validate_name<'a>(value: &'a str, field: &'static str) -> Result<&'a str, ProtocolParseError> {
    validate_atom(value, field, MAX_NAME_LENGTH)
}

fn validate_room(value: &str) -> Result<&str, ProtocolParseError> {
    validate_atom(value, "room", MAX_ROOM_LENGTH)
}

fn validate_token(value: &str) -> Result<&str, ProtocolParseError> {
    validate_atom(value, "token", 256)
}

fn validate_atom<'a>(
    value: &'a str,
    field: &'static str,
    max_len: usize,
) -> Result<&'a str, ProtocolParseError> {
    let value = non_empty(value.trim(), field)?;
    if value.len() > max_len {
        return Err(invalid(field, "too long"));
    }
    if value
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return Err(invalid(
            field,
            "must not contain whitespace or control characters",
        ));
    }
    Ok(value)
}

fn validate_base64<'a>(value: &'a str, field: &'static str) -> Result<&'a str, ProtocolParseError> {
    non_empty(value, field)?;
    STANDARD
        .decode(value)
        .map(|_| value)
        .map_err(|_| invalid(field, "must be valid base64"))
}

fn validate_chunk_base64(value: &str) -> Result<&str, ProtocolParseError> {
    let bytes = STANDARD
        .decode(non_empty(value, "chunk")?)
        .map_err(|_| invalid("chunk", "must be valid base64"))?;
    if bytes.len() > FILE_CHUNK_SIZE {
        return Err(invalid("chunk", "exceeds maximum chunk size"));
    }
    Ok(value)
}

fn validate_sha256(value: &str) -> Result<&str, ProtocolParseError> {
    non_empty(value, "sha256")?;
    if value.len() != SHA256_HEX_LENGTH || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("sha256", "must be 64 hexadecimal characters"));
    }
    Ok(value)
}

fn parse_transfer_id(value: &str) -> Result<TransferId, ProtocolParseError> {
    parse_u64_field(value, "transfer_id")
}

fn parse_offset(value: &str) -> Result<FileOffset, ProtocolParseError> {
    parse_u64_field(value, "offset")
}

fn parse_size(value: &str) -> Result<u64, ProtocolParseError> {
    let size = parse_u64_field(value, "size")?;
    if size > MAX_FILE_SIZE {
        return Err(invalid("size", "exceeds maximum file size"));
    }
    Ok(size)
}

fn parse_u64_field(value: &str, field: &'static str) -> Result<u64, ProtocolParseError> {
    value
        .parse()
        .map_err(|_| invalid(field, "must be a number"))
}

fn parse_usize_field(value: &str, field: &'static str) -> Result<usize, ProtocolParseError> {
    value
        .parse()
        .map_err(|_| invalid(field, "must be a number"))
}

fn invalid(field: &'static str, reason: &'static str) -> ProtocolParseError {
    ProtocolParseError::InvalidField { field, reason }
}

pub fn parse_user(line: &str) -> Option<&str> {
    line.strip_prefix(USER_PREFIX)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

pub fn parse_join(line: &str) -> Option<&str> {
    line.strip_prefix(JOIN_PREFIX)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

pub fn parse_clip(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.splitn(4, ' ');
    match (parts.next()?, parts.next()?, parts.next()?) {
        ("CLIP", room, b64) => Some((room, b64)),
        _ => None,
    }
}

pub fn parse_clip_fields(line: &str) -> Option<(&str, &str, Option<&str>)> {
    let mut parts = line.splitn(4, ' ');
    match (parts.next()?, parts.next()?, parts.next()?) {
        ("CLIP", room, b64) => Some((room, b64, parts.next())),
        _ => None,
    }
}

/// Parse FILE_OFFER command: FILE_OFFER <room> <filename_b64> <size>
pub fn parse_file_offer(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.splitn(4, ' ');
    match (parts.next()?, parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_OFFER", room, filename_b64, size) => Some((room, filename_b64, size)),
        _ => None,
    }
}

/// Parse FILE_OFFER from broker: FILE_OFFER <room> <username> <filename_b64> <size>
pub fn parse_file_offer_fields(line: &str) -> Option<(&str, &str, &str, &str)> {
    let mut parts = line.splitn(5, ' ');
    match (
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
    ) {
        ("FILE_OFFER", room, username, filename_b64, size) => {
            Some((room, username, filename_b64, size))
        }
        _ => None,
    }
}

/// Parse FILE_ACCEPT command: FILE_ACCEPT <room> <sender_username> <filename_b64>
pub fn parse_file_accept(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.splitn(4, ' ');
    match (parts.next()?, parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_ACCEPT", room, sender_username, filename_b64) => {
            Some((room, sender_username, filename_b64))
        }
        _ => None,
    }
}

/// Parse FILE_CANCEL command: FILE_CANCEL <transfer_id>
pub fn parse_file_cancel(line: &str) -> Option<TransferId> {
    let rest = line.strip_prefix(FILE_CANCEL_PREFIX)?;
    rest.trim().parse().ok()
}

/// Parse FILE_CHUNK command: FILE_CHUNK <transfer_id> <offset> <chunk_b64>
pub fn parse_file_chunk(line: &str) -> Option<(TransferId, FileOffset, &str)> {
    let mut parts = line.splitn(4, ' ');
    match (parts.next()?, parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_CHUNK", tid, offset, chunk_b64) => {
            Some((tid.parse().ok()?, offset.parse().ok()?, chunk_b64))
        }
        _ => None,
    }
}

/// Parse FILE_END command: FILE_END <transfer_id> <sha256>
pub fn parse_file_end(line: &str) -> Option<(TransferId, &str)> {
    let mut parts = line.splitn(3, ' ');
    match (parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_END", tid, sha256) => Some((tid.parse().ok()?, sha256)),
        _ => None,
    }
}

pub fn parse_cmd(line: &str) -> Option<&str> {
    line.strip_prefix(CMD_PREFIX)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

pub fn parse_say(line: &str) -> Option<&str> {
    line.strip_prefix(SAY_PREFIX)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Parse ENROLL command: ENROLL <token> <username>
pub fn parse_enroll(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix(ENROLL_PREFIX)?;
    let mut parts = rest.splitn(2, ' ');
    let token = parts.next()?.trim();
    let username = parts.next()?.trim();
    if token.is_empty() || username.is_empty() {
        return None;
    }
    Some((token, username))
}

/// Parse FILE_START from broker: FILE_START <transfer_id> <filename_b64> <acceptor_count>
pub fn parse_file_start_fields(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.splitn(4, ' ');
    match (parts.next()?, parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_START", transfer_id, filename_b64, acceptor_count) => {
            Some((transfer_id, filename_b64, acceptor_count))
        }
        _ => None,
    }
}

/// Parse FILE_INCOMING from broker: FILE_INCOMING <transfer_id> <filename_b64> <size>
pub fn parse_file_incoming_fields(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.splitn(4, ' ');
    match (parts.next()?, parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_INCOMING", transfer_id, filename_b64, size) => {
            Some((transfer_id, filename_b64, size))
        }
        _ => None,
    }
}

/// Parse FILE_CANCELLED from broker: FILE_CANCELLED <transfer_id> <reason>
pub fn parse_file_cancelled_fields(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix(FILE_CANCELLED_PREFIX)?;
    let mut parts = rest.splitn(2, ' ');
    let transfer_id = parts.next()?;
    let reason = parts.next().unwrap_or("unknown");
    Some((transfer_id, reason))
}

/// Parse FILE_CHUNK from broker: FILE_CHUNK <transfer_id> <offset> <chunk_b64>
pub fn parse_file_chunk_fields(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.splitn(4, ' ');
    match (parts.next()?, parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_CHUNK", transfer_id, offset, chunk_b64) => Some((transfer_id, offset, chunk_b64)),
        _ => None,
    }
}

/// Parse FILE_DONE from broker: FILE_DONE <transfer_id> <sha256>
pub fn parse_file_done_fields(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.splitn(3, ' ');
    match (parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_DONE", transfer_id, sha256) => Some((transfer_id, sha256)),
        _ => None,
    }
}

/// Parse FILE_SENT from broker: FILE_SENT <transfer_id> <count>
pub fn parse_file_sent_fields(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.splitn(3, ' ');
    match (parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_SENT", transfer_id, count) => Some((transfer_id, count)),
        _ => None,
    }
}

/// Parse SAY from broker: SAY <user> <text>
pub fn parse_say_fields(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.splitn(3, ' ');
    match (parts.next()?, parts.next()?, parts.next()?) {
        ("SAY", user, text) => Some((user, text)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== parse_user tests ====================
    #[test]
    fn test_parse_user_valid() {
        assert_eq!(parse_user("USER alice"), Some("alice"));
        assert_eq!(parse_user("USER bob_123"), Some("bob_123"));
    }

    #[test]
    fn test_parse_user_with_whitespace() {
        assert_eq!(parse_user("USER   alice  "), Some("alice"));
    }

    #[test]
    fn test_parse_user_empty() {
        assert_eq!(parse_user("USER "), None);
        assert_eq!(parse_user("USER"), None);
    }

    #[test]
    fn test_parse_user_wrong_prefix() {
        assert_eq!(parse_user("JOIN alice"), None);
        assert_eq!(parse_user("alice"), None);
    }

    // ==================== parse_join tests ====================
    #[test]
    fn test_parse_join_valid() {
        assert_eq!(parse_join("JOIN lobby"), Some("lobby"));
        assert_eq!(parse_join("JOIN room-1"), Some("room-1"));
    }

    #[test]
    fn test_parse_join_with_whitespace() {
        assert_eq!(parse_join("JOIN   lobby  "), Some("lobby"));
    }

    #[test]
    fn test_parse_join_empty() {
        assert_eq!(parse_join("JOIN "), None);
        assert_eq!(parse_join("JOIN"), None);
    }

    #[test]
    fn test_parse_join_wrong_prefix() {
        assert_eq!(parse_join("USER lobby"), None);
    }

    // ==================== parse_clip tests ====================
    #[test]
    fn test_parse_clip_valid() {
        assert_eq!(
            parse_clip("CLIP lobby SGVsbG8="),
            Some(("lobby", "SGVsbG8="))
        );
        assert_eq!(
            parse_clip("CLIP room-1 dGVzdA=="),
            Some(("room-1", "dGVzdA=="))
        );
    }

    #[test]
    fn test_parse_clip_with_extra_spaces_in_payload() {
        let result = parse_clip("CLIP lobby SGVsbG8= extra");
        assert_eq!(result, Some(("lobby", "SGVsbG8=")));
    }

    #[test]
    fn test_parse_clip_missing_parts() {
        assert_eq!(parse_clip("CLIP lobby"), None);
        assert_eq!(parse_clip("CLIP"), None);
    }

    #[test]
    fn test_parse_clip_wrong_prefix() {
        assert_eq!(parse_clip("JOIN lobby SGVsbG8="), None);
    }

    // ==================== parse_clip_fields tests ====================
    #[test]
    fn test_parse_clip_fields_with_id() {
        let result = parse_clip_fields("CLIP lobby SGVsbG8= 42");
        assert_eq!(result, Some(("lobby", "SGVsbG8=", Some("42"))));
    }

    #[test]
    fn test_parse_clip_fields_without_id() {
        let result = parse_clip_fields("CLIP lobby SGVsbG8=");
        assert_eq!(result, Some(("lobby", "SGVsbG8=", None)));
    }

    #[test]
    fn test_parse_clip_fields_different_room() {
        let result = parse_clip_fields("CLIP room-1 dGVzdA== 123");
        assert_eq!(result, Some(("room-1", "dGVzdA==", Some("123"))));
    }

    #[test]
    fn test_parse_clip_fields_missing_parts() {
        assert_eq!(parse_clip_fields("CLIP lobby"), None);
        assert_eq!(parse_clip_fields("CLIP"), None);
        assert_eq!(parse_clip_fields(""), None);
    }

    #[test]
    fn test_parse_clip_fields_wrong_prefix() {
        assert_eq!(parse_clip_fields("JOIN lobby SGVsbG8="), None);
        assert_eq!(parse_clip_fields("INFO some text"), None);
    }

    // ==================== parse_cmd tests ====================
    #[test]
    fn test_parse_cmd_valid() {
        assert_eq!(parse_cmd("CMD /help"), Some("/help"));
        assert_eq!(parse_cmd("CMD /rooms"), Some("/rooms"));
        assert_eq!(parse_cmd("CMD /who"), Some("/who"));
    }

    #[test]
    fn test_parse_cmd_with_args() {
        assert_eq!(parse_cmd("CMD /join lobby"), Some("/join lobby"));
    }

    #[test]
    fn test_parse_cmd_empty() {
        assert_eq!(parse_cmd("CMD "), None);
        assert_eq!(parse_cmd("CMD"), None);
    }

    #[test]
    fn test_parse_cmd_wrong_prefix() {
        assert_eq!(parse_cmd("/help"), None);
    }

    // ==================== parse_say tests ====================
    #[test]
    fn test_parse_say_valid() {
        assert_eq!(parse_say("SAY hello world"), Some("hello world"));
        assert_eq!(parse_say("SAY hi"), Some("hi"));
    }

    #[test]
    fn test_parse_say_empty() {
        assert_eq!(parse_say("SAY "), None);
        assert_eq!(parse_say("SAY"), None);
    }

    #[test]
    fn test_parse_say_wrong_prefix() {
        assert_eq!(parse_say("CMD hello"), None);
    }

    // ==================== file transfer parser tests ====================
    #[test]
    fn test_parse_file_transfer_numeric_fields_reject_invalid_numbers() {
        assert_eq!(parse_file_cancel("FILE_CANCEL 42"), Some(42));
        assert_eq!(
            parse_file_chunk("FILE_CHUNK 42 65536 SGVsbG8="),
            Some((42, 65536, "SGVsbG8="))
        );
        assert_eq!(parse_file_end("FILE_END 42 abc123"), Some((42, "abc123")));

        assert_eq!(parse_file_cancel("FILE_CANCEL nope"), None);
        assert_eq!(parse_file_chunk("FILE_CHUNK nope 0 SGVsbG8="), None);
        assert_eq!(parse_file_chunk("FILE_CHUNK 42 nope SGVsbG8="), None);
        assert_eq!(parse_file_end("FILE_END nope abc123"), None);
    }

    #[test]
    fn test_parse_file_transfer_fields_preserve_tail_payloads() {
        assert_eq!(
            parse_file_offer("FILE_OFFER lobby report.pdf 12345"),
            Some(("lobby", "report.pdf", "12345"))
        );
        assert_eq!(
            parse_file_offer_fields("FILE_OFFER lobby alice report.pdf 12345"),
            Some(("lobby", "alice", "report.pdf", "12345"))
        );
        assert_eq!(
            parse_file_accept("FILE_ACCEPT lobby alice report.pdf"),
            Some(("lobby", "alice", "report.pdf"))
        );
        assert_eq!(
            parse_file_start_fields("FILE_START 7 report.pdf 2"),
            Some(("7", "report.pdf", "2"))
        );
        assert_eq!(
            parse_file_incoming_fields("FILE_INCOMING 7 report.pdf 12345"),
            Some(("7", "report.pdf", "12345"))
        );
        assert_eq!(
            parse_file_chunk_fields("FILE_CHUNK 7 65536 SGVsbG8= with-tail"),
            Some(("7", "65536", "SGVsbG8= with-tail"))
        );
        assert_eq!(
            parse_file_done_fields("FILE_DONE 7 abc123"),
            Some(("7", "abc123"))
        );
        assert_eq!(parse_file_sent_fields("FILE_SENT 7 2"), Some(("7", "2")));
        assert_eq!(
            parse_file_cancelled_fields("FILE_CANCELLED 7 user requested cancel"),
            Some(("7", "user requested cancel"))
        );
        assert_eq!(
            parse_file_cancelled_fields("FILE_CANCELLED 7"),
            Some(("7", "unknown"))
        );

        assert_eq!(parse_file_offer("SAY lobby report.pdf 12345"), None);
        assert_eq!(parse_file_offer("FILE_OFFER lobby report.pdf"), None);
        assert_eq!(
            parse_file_offer_fields("FILE_OFFER lobby alice report.pdf"),
            None
        );
        assert_eq!(parse_file_accept("FILE_ACCEPT lobby alice"), None);
        assert_eq!(parse_file_start_fields("FILE_START 7 report.pdf"), None);
        assert_eq!(
            parse_file_incoming_fields("FILE_INCOMING 7 report.pdf"),
            None
        );
        assert_eq!(parse_file_done_fields("FILE_DONE 7"), None);
        assert_eq!(parse_file_sent_fields("FILE_SENT 7"), None);
    }

    #[test]
    fn test_parse_client_message_validates_core_commands() {
        assert_eq!(
            parse_client_message("USER alice"),
            Ok(ClientMessage::User { name: "alice" })
        );
        assert_eq!(
            parse_client_message("JOIN lobby"),
            Ok(ClientMessage::Join { room: "lobby" })
        );
        assert_eq!(
            parse_client_message("CLIP lobby SGVsbG8="),
            Ok(ClientMessage::Clip {
                room: "lobby",
                b64: "SGVsbG8="
            })
        );
        assert_eq!(
            parse_client_message("CMD /who"),
            Ok(ClientMessage::Cmd { command: "/who" })
        );
        assert_eq!(
            parse_client_message("SAY hello world"),
            Ok(ClientMessage::Say {
                text: "hello world"
            })
        );
    }

    #[test]
    fn test_parse_client_message_validates_file_transfer_commands() {
        assert_eq!(
            parse_client_message("FILE_OFFER lobby cmVwb3J0LnBkZg== 12345"),
            Ok(ClientMessage::FileOffer {
                room: "lobby",
                filename_b64: "cmVwb3J0LnBkZg==",
                size: 12345,
            })
        );
        assert_eq!(
            parse_client_message("FILE_ACCEPT lobby alice cmVwb3J0LnBkZg=="),
            Ok(ClientMessage::FileAccept {
                room: "lobby",
                sender_username: "alice",
                filename_b64: "cmVwb3J0LnBkZg==",
            })
        );
        assert_eq!(
            parse_client_message("FILE_CANCEL 42"),
            Ok(ClientMessage::FileCancel { transfer_id: 42 })
        );
        assert_eq!(
            parse_client_message("FILE_CHUNK 42 65536 SGVsbG8="),
            Ok(ClientMessage::FileChunk {
                transfer_id: 42,
                offset: 65536,
                chunk_b64: "SGVsbG8=",
            })
        );
        assert_eq!(
            parse_client_message(
                "FILE_END 42 e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            Ok(ClientMessage::FileEnd {
                transfer_id: 42,
                sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            })
        );
    }

    #[test]
    fn test_parse_client_message_rejects_malformed_values() {
        assert_eq!(
            parse_client_message("USER alice bob"),
            Err(ProtocolParseError::TrailingFields)
        );
        assert_eq!(
            parse_client_message("JOIN bad room"),
            Err(ProtocolParseError::TrailingFields)
        );
        assert_eq!(
            parse_client_message("CLIP lobby not-base64!"),
            Err(ProtocolParseError::InvalidField {
                field: "clipboard payload",
                reason: "must be valid base64",
            })
        );
        assert_eq!(
            parse_client_message("CMD who"),
            Err(ProtocolParseError::InvalidField {
                field: "command",
                reason: "must start with '/'",
            })
        );
        assert_eq!(
            parse_client_message("FILE_OFFER lobby cmVwb3J0LnBkZg== 1073741825"),
            Err(ProtocolParseError::InvalidField {
                field: "size",
                reason: "exceeds maximum file size",
            })
        );
        assert_eq!(
            parse_client_message(
                "FILE_END 42 e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b85z",
            ),
            Err(ProtocolParseError::InvalidField {
                field: "sha256",
                reason: "must be 64 hexadecimal characters",
            })
        );
    }

    #[test]
    fn test_parse_client_message_rejects_oversized_chunks() {
        let oversized_chunk = STANDARD.encode(vec![0_u8; FILE_CHUNK_SIZE + 1]);
        let line = format!("FILE_CHUNK 42 0 {oversized_chunk}");
        assert_eq!(
            parse_client_message(&line),
            Err(ProtocolParseError::InvalidField {
                field: "chunk",
                reason: "exceeds maximum chunk size",
            })
        );
    }

    #[test]
    fn test_parse_broker_message_validates_messages() {
        assert_eq!(
            parse_broker_message("INFO connected"),
            Ok(BrokerMessage::Info { text: "connected" })
        );
        assert_eq!(
            parse_broker_message("CLIP lobby SGVsbG8= 77"),
            Ok(BrokerMessage::Clip {
                room: "lobby",
                b64: "SGVsbG8=",
                id: Some(77),
            })
        );
        assert_eq!(
            parse_broker_message("SAY alice hello world"),
            Ok(BrokerMessage::Say {
                user: "alice",
                text: "hello world",
            })
        );
        assert_eq!(
            parse_broker_message("FILE_INCOMING 9 cmVwb3J0LnBkZg== 12345"),
            Ok(BrokerMessage::FileIncoming {
                transfer_id: 9,
                filename_b64: "cmVwb3J0LnBkZg==",
                size: 12345,
            })
        );
        assert_eq!(
            parse_broker_message("FILE_CANCELLED 9 user requested cancel"),
            Ok(BrokerMessage::FileCancelled {
                transfer_id: 9,
                reason: "user requested cancel",
            })
        );
    }

    #[test]
    fn test_parse_broker_message_rejects_malformed_values() {
        assert_eq!(
            parse_broker_message("CLIP lobby SGVsbG8= nope"),
            Err(ProtocolParseError::InvalidField {
                field: "id",
                reason: "must be a number",
            })
        );
        assert_eq!(
            parse_broker_message("FILE_SENT 9 nope"),
            Err(ProtocolParseError::InvalidField {
                field: "count",
                reason: "must be a number",
            })
        );
        assert_eq!(
            parse_broker_message("FILE_DONE 9 short"),
            Err(ProtocolParseError::InvalidField {
                field: "sha256",
                reason: "must be 64 hexadecimal characters",
            })
        );
        assert_eq!(
            parse_broker_message("FILE_CANCELLED 9"),
            Err(ProtocolParseError::MissingField("reason"))
        );
    }
}
