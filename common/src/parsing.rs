use crate::protocol::{
    CMD_PREFIX, ENROLL_PREFIX, FILE_CANCELLED_PREFIX, FILE_CANCEL_PREFIX, JOIN_PREFIX, SAY_PREFIX,
    USER_PREFIX,
};
use crate::types::{FileOffset, TransferId};

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
}
