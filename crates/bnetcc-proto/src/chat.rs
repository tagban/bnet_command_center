//! Chat events, flags, and the length limits real clients depend on.

use crate::buf::Writer;
use crate::bncs::{sid, Frame};

/// Maximum chat text including the NUL terminator.
///
/// The field allows 255, but official clients restrict to 224. Exceeding what the client
/// expects is a good way to produce a client-side buffer bug, so we enforce the lower
/// number in both directions.
pub const CHAT_TEXT_MAX: usize = 224;

/// Maximum channel name length. Longer names are trimmed by real Battle.net.
pub const CHANNEL_NAME_MAX: usize = 31;

/// Maximum account name length. Longer names are truncated by real Battle.net.
pub const USERNAME_MAX: usize = 15;

/// `SID_CHATEVENT` (0x0F) event identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum EventId {
    /// A user already present when you joined. One per occupant.
    ShowUser = 0x01,
    /// A user entered your channel.
    Join = 0x02,
    /// A user left your channel.
    Leave = 0x03,
    /// A private message you received.
    Whisper = 0x04,
    /// Channel chat from another user.
    Talk = 0x05,
    /// A server announcement.
    Broadcast = 0x06,
    /// Channel information — "you are in channel X".
    Channel = 0x07,
    /// A user's flags changed.
    UserFlags = 0x09,
    /// Your whisper was delivered.
    WhisperSent = 0x0A,
    /// The channel is at capacity.
    ChannelFull = 0x0D,
    /// The channel does not exist.
    ChannelDoesNotExist = 0x0E,
    /// Access to the channel was denied.
    ChannelRestricted = 0x0F,
    /// An informational message.
    Info = 0x12,
    /// An error message.
    Error = 0x13,
    /// An emote.
    Emote = 0x17,
}

/// User flags carried in `SID_CHATEVENT`.
pub mod user_flags {
    /// Blizzard representative.
    pub const BLIZZARD_REP: u32 = 0x01;
    /// Channel operator. The flag channel wars are fought over.
    pub const OPERATOR: u32 = 0x02;
    /// Channel speaker in a moderated channel.
    pub const SPEAKER: u32 = 0x04;
    /// Battle.net administrator — server staff, not a channel role.
    pub const ADMIN: u32 = 0x08;
    /// No UDP support.
    ///
    /// Real Battle.net never answers UDP for `DRTL`/`DSHR` (Diablo, Diablo Shareware), so
    /// those clients always show this flag — set it unconditionally for them (see
    /// `product::always_no_udp`, applied on both the modern and legacy logon paths).
    /// `W2BN` is **not** in that set: a real Warcraft II BNE client does complete the UDP
    /// check (captured 2026-09-09), so it must not be forced No-UDP.
    pub const NO_UDP: u32 = 0x10;
    /// Squelched (ignored) by the recipient.
    pub const SQUELCHED: u32 = 0x20;
    /// Special guest.
    pub const SPECIAL_GUEST: u32 = 0x40;
    /// Beep enabled.
    pub const BEEP: u32 = 0x100;
}

/// Channel flags.
pub mod channel_flags {
    /// Public channel.
    pub const PUBLIC: u32 = 0x0001;
    /// Moderated: only speakers may talk.
    pub const MODERATED: u32 = 0x0002;
    /// Restricted access.
    pub const RESTRICTED: u32 = 0x0004;
    /// Silent.
    pub const SILENT: u32 = 0x0008;
    /// System channel.
    pub const SYSTEM: u32 = 0x0010;
    /// Product-specific channel.
    pub const PRODUCT_SPECIFIC: u32 = 0x0020;
    /// Globally accessible across products.
    pub const GLOBAL: u32 = 0x1000;
    /// Redirected.
    pub const REDIRECTED: u32 = 0x4000;
    /// Chat-only channel.
    pub const CHAT: u32 = 0x8000;
}

/// Clean chat text arriving from a client.
///
/// Two rules, both load-bearing:
///
/// 1. **Strip bytes below `0x20`.** Real Battle.net disconnects and IP-bans for five
///    minutes on receiving a bare CR or LF in `SID_CHATCOMMAND`. If we relayed one into
///    a channel broadcast, we would trigger that behaviour on every *recipient's* client.
///    A control byte in a chat line is never legitimate.
/// 2. **Truncate to the documented ceiling**, so we never hand a client more than it
///    expects.
///
/// Returns the cleaned bytes. Encoding is not validated here because it is
/// product-dependent — UTF-8 for `STAR`/`SEXP`/`SSHR`/`JSTR`, ISO 8859-1 otherwise.
#[must_use]
pub fn sanitize_chat_text(input: &[u8]) -> Vec<u8> {
    input
        .iter()
        .copied()
        .filter(|&b| b >= 0x20 && b != 0x7F)
        .take(CHAT_TEXT_MAX - 1)
        .collect()
}

/// Normalise a channel name for lookup.
///
/// Battle.net channel names are case-insensitive and space-normalised. Doing this in one
/// place is what stops `Op Clan Xyz` and `op clan xyz` becoming two channels.
#[must_use]
pub fn normalize_channel_name(name: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(name.len());
    let mut last_space = true; // trims leading whitespace
    for &b in name.iter().filter(|&&b| b >= 0x20 && b != 0x7F) {
        let c = b.to_ascii_lowercase();
        if c == b' ' {
            if !last_space {
                out.push(c);
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    while out.last() == Some(&b' ') {
        out.pop();
    }
    out.truncate(CHANNEL_NAME_MAX);
    out
}

/// Build a `SID_CHATEVENT` frame.
///
/// The three defunct fields (IP address, account number, registration authority) are sent
/// as zero, matching what real Battle.net does today.
#[must_use]
pub fn chat_event(event: EventId, flags: u32, ping: u32, username: &[u8], text: &[u8]) -> Frame {
    let mut w = Writer::with_capacity(32 + username.len() + text.len());
    w.u32(event as u32)
        .u32(flags)
        .u32(ping)
        .u32(0) // IP address — defunct
        .u32(0) // account number — defunct
        .u32(0) // registration authority — defunct
        .cstr(username)
        .cstr(text);
    Frame::new(sid::CHATEVENT, w.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_control_bytes() {
        // A relayed CR or LF triggers a five-minute IP ban on the receiving client.
        assert_eq!(sanitize_chat_text(b"hello\r\nworld"), b"helloworld");
        assert_eq!(sanitize_chat_text(b"tab\there"), b"tabhere");
        assert_eq!(sanitize_chat_text(b"del\x7Fete"), b"delete");
    }

    #[test]
    fn sanitize_truncates_to_the_client_limit() {
        let long = vec![b'x'; 500];
        assert_eq!(sanitize_chat_text(&long).len(), CHAT_TEXT_MAX - 1);
    }

    #[test]
    fn sanitize_preserves_ordinary_text() {
        assert_eq!(sanitize_chat_text(b"gg wp :)"), b"gg wp :)");
    }

    #[test]
    fn channel_names_normalize_case_and_spacing() {
        assert_eq!(normalize_channel_name(b"Op Clan XYZ"), b"op clan xyz");
        assert_eq!(normalize_channel_name(b"  Blizzard   Tech  "), b"blizzard tech");
        assert_eq!(normalize_channel_name(b"BROOD WAR"), b"brood war");
    }

    #[test]
    fn channel_names_are_truncated_to_31() {
        let long = vec![b'a'; 60];
        assert_eq!(normalize_channel_name(&long).len(), CHANNEL_NAME_MAX);
    }

    #[test]
    fn chat_event_layout_is_stable() {
        let f = chat_event(EventId::Talk, user_flags::OPERATOR, 42, b"Zealot", b"hi");
        assert_eq!(f.id, sid::CHATEVENT);
        let mut r = f.reader();
        assert_eq!(r.u32().unwrap(), 0x05); // EID_TALK
        assert_eq!(r.u32().unwrap(), 0x02); // operator
        assert_eq!(r.u32().unwrap(), 42);
        assert_eq!(r.u32().unwrap(), 0);
        assert_eq!(r.u32().unwrap(), 0);
        assert_eq!(r.u32().unwrap(), 0);
        assert_eq!(r.cstr(USERNAME_MAX).unwrap(), b"Zealot");
        assert_eq!(r.cstr(CHAT_TEXT_MAX).unwrap(), b"hi");
        assert!(r.is_empty());
    }
}
