//! The BNCS session state machine.
//!
//! Every transition is explicit and every packet is checked against the current state.
//! A packet arriving in the wrong state is a protocol violation: log it, close the
//! connection. Nothing after authentication is reachable before it, which is the
//! structural fix for the class of bug behind CVE-2004-2705 (arbitrary account
//! attribute read, including the password hash, from an unauthenticated peer).

use cairn_proto::bncs::sid;

/// Where a BNCS connection is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Accepted; the protocol selector byte has been read.
    Connected,
    /// `SID_AUTH_INFO` seen; awaiting the version check.
    Versioning,
    /// Version accepted; awaiting a logon or account creation.
    Authenticating,
    /// Authenticated, but not yet in chat.
    LoggedIn,
    /// `SID_ENTERCHAT` done; may host, list games, and join a channel.
    Chatting,
    /// In a channel; may send chat.
    InChannel,
    /// Shutting down; no further packets accepted.
    Closing,
}

impl SessionState {
    /// A short name for logs and error messages.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Versioning => "versioning",
            Self::Authenticating => "authenticating",
            Self::LoggedIn => "logged-in",
            Self::Chatting => "chatting",
            Self::InChannel => "in-channel",
            Self::Closing => "closing",
        }
    }

    /// Whether the session has completed authentication.
    #[must_use]
    pub const fn authenticated(self) -> bool {
        matches!(self, Self::LoggedIn | Self::Chatting | Self::InChannel)
    }

    /// Whether a packet id is acceptable in this state.
    ///
    /// Two deliberate quirks, both from real Battle.net behaviour that clients depend on:
    ///
    /// - **`SID_PING` is accepted everywhere.** It is a keepalive and arrives unprompted.
    /// - **`SID_STOPADV` is accepted everywhere, including before login.** Every
    ///   `Battle.snp` client sends it on logoff even when not in a game, and StarCraft
    ///   1.16.1 may send it before login completes. Treating it as a violation would
    ///   disconnect legitimate clients.
    #[must_use]
    pub fn accepts(self, id: u8) -> bool {
        if matches!(self, Self::Closing) {
            return false;
        }
        if matches!(id, sid::PING | sid::STOPADV) {
            return true;
        }
        match self {
            Self::Connected => matches!(id, sid::AUTH_INFO),
            Self::Versioning => matches!(id, sid::AUTH_CHECK),
            Self::Authenticating => matches!(
                id,
                sid::LOGONRESPONSE
                    | sid::LOGONRESPONSE2
                    | sid::CREATEACCOUNT2
                    | sid::AUTH_ACCOUNTCREATE
                    | sid::AUTH_ACCOUNTLOGON
                    | sid::AUTH_ACCOUNTLOGONPROOF
            ),
            Self::LoggedIn => matches!(
                id,
                sid::ENTERCHAT
                    | sid::GETCHANNELLIST
                    | sid::QUERYREALMS2
                    | sid::LOGONREALMEX
                    | sid::NETGAMEPORT
                    | sid::WARCRAFTGENERAL
            ),
            Self::Chatting | Self::InChannel => matches!(
                id,
                sid::ENTERCHAT
                    | sid::JOINCHANNEL
                    | sid::CHATCOMMAND
                    | sid::GETCHANNELLIST
                    | sid::GETADVLISTEX
                    | sid::STARTADVEX3
                    | sid::NOTIFYJOIN
                    | sid::LEAVEGAME
                    | sid::QUERYREALMS2
                    | sid::LOGONREALMEX
                    | sid::NETGAMEPORT
                    | sid::WARCRAFTGENERAL
            ),
            Self::Closing => false,
        }
    }

    /// The state this packet moves the session to on success, if any.
    ///
    /// Returns `None` when the packet does not change state.
    #[must_use]
    pub const fn next_on_success(self, id: u8) -> Option<Self> {
        match (self, id) {
            (Self::Connected, sid::AUTH_INFO) => Some(Self::Versioning),
            (Self::Versioning, sid::AUTH_CHECK) => Some(Self::Authenticating),
            // Note that account creation does *not* log you in. Real Battle.net requires
            // a separate logon afterwards, and clients are written to expect that.
            (
                Self::Authenticating,
                sid::LOGONRESPONSE | sid::LOGONRESPONSE2 | sid::AUTH_ACCOUNTLOGONPROOF,
            ) => Some(Self::LoggedIn),
            (Self::LoggedIn, sid::ENTERCHAT) => Some(Self::Chatting),
            (Self::Chatting, sid::JOINCHANNEL) => Some(Self::InChannel),
            _ => None,
        }
    }
}

/// Deadlines that bound how long an unauthenticated peer can hold resources.
///
/// PvPGN enforces none of these, which is what makes slowloris work against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadlines {
    /// Accept to authenticated, in seconds.
    pub handshake_secs: u64,
    /// Idle timeout after authentication, in seconds.
    pub idle_secs: u64,
    /// Minimum interval between logins for one account, in milliseconds.
    ///
    /// Real Battle.net keeps a CD key marked "in use" if you reconnect within roughly
    /// 500 ms, so clients that relogin faster see a spurious "key in use" error.
    /// Replicating the cooldown makes us behave the way bots already expect.
    pub relogin_cooldown_ms: u64,
}

impl Default for Deadlines {
    fn default() -> Self {
        Self {
            handshake_secs: 30,
            idle_secs: 1200,
            relogin_cooldown_ms: 500,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_happy_path_walks_the_states() {
        let mut s = SessionState::Connected;
        for (id, expect) in [
            (sid::AUTH_INFO, SessionState::Versioning),
            (sid::AUTH_CHECK, SessionState::Authenticating),
            (sid::LOGONRESPONSE2, SessionState::LoggedIn),
            (sid::ENTERCHAT, SessionState::Chatting),
            (sid::JOINCHANNEL, SessionState::InChannel),
        ] {
            assert!(s.accepts(id), "{} should accept {id:#04x}", s.name());
            s = s.next_on_success(id).expect("transition");
            assert_eq!(s, expect);
        }
    }

    #[test]
    fn chat_is_not_reachable_before_authentication() {
        for s in [
            SessionState::Connected,
            SessionState::Versioning,
            SessionState::Authenticating,
        ] {
            assert!(!s.accepts(sid::CHATCOMMAND), "{} accepted chat", s.name());
            assert!(!s.accepts(sid::JOINCHANNEL));
            assert!(!s.accepts(sid::GETADVLISTEX));
            assert!(!s.accepts(sid::STARTADVEX3));
        }
    }

    #[test]
    fn logon_packets_are_not_accepted_before_the_version_check() {
        assert!(!SessionState::Connected.accepts(sid::LOGONRESPONSE2));
        assert!(!SessionState::Versioning.accepts(sid::LOGONRESPONSE2));
        assert!(SessionState::Authenticating.accepts(sid::LOGONRESPONSE2));
    }

    #[test]
    fn ping_is_accepted_in_every_open_state() {
        for s in [
            SessionState::Connected,
            SessionState::Versioning,
            SessionState::Authenticating,
            SessionState::LoggedIn,
            SessionState::Chatting,
            SessionState::InChannel,
        ] {
            assert!(s.accepts(sid::PING), "{} rejected ping", s.name());
        }
    }

    #[test]
    fn stopadv_is_accepted_before_login() {
        // StarCraft 1.16.1 sends this before login completes; every Battle.snp client
        // sends it on logoff even when not in a game. Rejecting it disconnects
        // legitimate clients.
        for s in [
            SessionState::Connected,
            SessionState::Versioning,
            SessionState::Authenticating,
            SessionState::InChannel,
        ] {
            assert!(s.accepts(sid::STOPADV), "{} rejected stopadv", s.name());
        }
        assert_eq!(SessionState::Connected.next_on_success(sid::STOPADV), None);
    }

    #[test]
    fn closing_accepts_nothing() {
        for id in [sid::PING, sid::STOPADV, sid::CHATCOMMAND, sid::AUTH_INFO] {
            assert!(!SessionState::Closing.accepts(id));
        }
    }

    #[test]
    fn authenticated_flag_matches_the_states() {
        assert!(!SessionState::Connected.authenticated());
        assert!(!SessionState::Versioning.authenticated());
        assert!(!SessionState::Authenticating.authenticated());
        assert!(SessionState::LoggedIn.authenticated());
        assert!(SessionState::Chatting.authenticated());
        assert!(SessionState::InChannel.authenticated());
        assert!(!SessionState::Closing.authenticated());
    }

    #[test]
    fn no_unauthenticated_state_accepts_a_data_bearing_packet() {
        // The structural guard against CVE-2004-2705's class: sweep every packet id and
        // assert that a pre-auth session accepts only the handshake set.
        let handshake = [
            sid::AUTH_INFO,
            sid::AUTH_CHECK,
            sid::LOGONRESPONSE,
            sid::LOGONRESPONSE2,
            sid::CREATEACCOUNT2,
            sid::AUTH_ACCOUNTCREATE,
            sid::AUTH_ACCOUNTLOGON,
            sid::AUTH_ACCOUNTLOGONPROOF,
            sid::PING,
            sid::STOPADV,
        ];
        for state in [
            SessionState::Connected,
            SessionState::Versioning,
            SessionState::Authenticating,
        ] {
            for id in 0u8..=255 {
                if state.accepts(id) {
                    assert!(
                        handshake.contains(&id),
                        "{} accepted non-handshake packet {id:#04x}",
                        state.name()
                    );
                }
            }
        }
    }

    #[test]
    fn deadlines_have_sane_defaults() {
        let d = Deadlines::default();
        assert!(d.handshake_secs > 0 && d.handshake_secs <= 60);
        assert!(d.idle_secs >= 300);
        assert!(
            d.relogin_cooldown_ms >= 500,
            "below 500ms real Battle.net reports the CD key as still in use"
        );
    }
}
