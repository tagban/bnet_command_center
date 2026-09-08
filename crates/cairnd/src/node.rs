//! Shared node state.
//!
//! Channels, subscribers and accounts live behind a `std::sync::Mutex` with **no `await`
//! inside any critical section** — every lock is taken, mutated, and released without
//! yielding. That is deliberate: a `tokio::sync::Mutex` invites holding a lock across an
//! await point, which is how a chat server acquires a global stall.
//!
//! Fanout encodes each frame **once** and hands every subscriber an `Arc` of the same
//! bytes. A 200-user channel therefore costs one encode and 200 pointer clones, not 200
//! encodes. Channel fanout, not connection count, is the real scaling limit at this size.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cairn_core::channel::{AccountId, Channel, ChannelClass, JoinDenial};
use cairn_core::limits::AdmissionTable;
use cairn_core::policy::Policy;
use cairn_proto::bncs::{encode_frame, Frame};
use cairn_proto::chat::normalize_channel_name;
use tokio::sync::mpsc;

/// Bytes ready for the wire, shared by every recipient of a broadcast.
pub type Wire = Arc<Vec<u8>>;

/// The write side of one connection.
#[derive(Debug, Clone)]
pub struct Outbound {
    tx: mpsc::Sender<Wire>,
}

impl Outbound {
    /// Wrap a sender.
    #[must_use]
    pub const fn new(tx: mpsc::Sender<Wire>) -> Self {
        Self { tx }
    }

    /// Queue pre-encoded bytes.
    ///
    /// Returns `false` when the queue is full, which means the peer is not reading.
    /// The caller must then close the connection: buffering without bound is how PvPGN
    /// crashed until it added a hard cap in 2014, and silently dropping frames would
    /// desynchronise a client's view of a channel.
    #[must_use]
    pub fn send(&self, wire: &Wire) -> bool {
        self.tx.try_send(Arc::clone(wire)).is_ok()
    }

    /// Encode and queue one frame.
    #[must_use]
    pub fn send_frame(&self, frame: &Frame) -> bool {
        let mut buf = Vec::with_capacity(frame.wire_len());
        if encode_frame(frame, &mut buf).is_err() {
            return false;
        }
        self.send(&Arc::new(buf))
    }
}

/// A registered account.
///
/// In-memory for now. This is the seam `cairn-storage` plugs into: replace the map with
/// the `Storage` trait and keep this as a write-through cache. **Do not ship without
/// persistence** — that is exactly Atlas's situation, where every account evaporates on
/// restart, and it is the main reason Atlas appears more stable than it is.
#[derive(Debug, Clone)]
pub struct Account {
    /// Stable identifier.
    pub id: AccountId,
    /// Display name, as registered.
    pub name: String,
    /// `XSHA1(lowercase(password))`.
    ///
    /// Password-equivalent. In a federation this never leaves the hub; see
    /// `docs/FEDERATION.md` §4.
    pub password_hash: [u8; 20],
}

#[derive(Default)]
struct Inner {
    channels: HashMap<Vec<u8>, Channel>,
    /// Subscribers per channel, keyed by normalised channel name.
    subscribers: HashMap<Vec<u8>, Vec<(AccountId, Outbound)>>,
    /// Accounts by lowercased name.
    accounts: HashMap<String, Account>,
    next_account_id: AccountId,
}

/// Everything one node shares between connections.
pub struct Node {
    /// Effective policy, already narrowed by the hub if federated.
    pub policy: Policy,
    /// Server name shown to clients.
    pub name: String,
    /// Message of the day.
    pub motd: String,
    /// Maximum users per channel.
    pub channel_max_users: usize,
    inner: Mutex<Inner>,
    game_admission: Mutex<AdmissionTable>,
    gateway_admission: Mutex<AdmissionTable>,
    connections: AtomicU64,
}

impl Node {
    /// Build a node.
    #[must_use]
    pub fn new(policy: Policy, name: String, motd: String, gateway_allowlist: Vec<IpAddr>) -> Self {
        let mut gateway = AdmissionTable::new();
        gateway.set_allowlist(gateway_allowlist);
        Self {
            policy,
            name,
            motd,
            channel_max_users: 40,
            inner: Mutex::new(Inner::default()),
            game_admission: Mutex::new(AdmissionTable::new()),
            gateway_admission: Mutex::new(gateway),
            connections: AtomicU64::new(0),
        }
    }

    /// Live connection count, for metrics.
    #[must_use]
    pub fn connection_count(&self) -> u64 {
        self.connections.load(Ordering::Relaxed)
    }

    /// Admit a game-client connection.
    ///
    /// # Errors
    ///
    /// The rejection reason, for logging and metrics.
    pub fn admit_game(&self, ip: IpAddr) -> Result<(), cairn_core::limits::Rejection> {
        let r = self
            .game_admission
            .lock()
            .expect("admission lock")
            .admit_ip(ip, self.policy.game_limits);
        if r.is_ok() {
            self.connections.fetch_add(1, Ordering::Relaxed);
        }
        r
    }

    /// Admit a chat-gateway connection.
    ///
    /// # Errors
    ///
    /// The rejection reason, for logging and metrics.
    pub fn admit_gateway(&self, ip: IpAddr) -> Result<(), cairn_core::limits::Rejection> {
        let r = self
            .gateway_admission
            .lock()
            .expect("admission lock")
            .admit_ip(ip, self.policy.gateway_limits);
        if r.is_ok() {
            self.connections.fetch_add(1, Ordering::Relaxed);
        }
        r
    }

    /// Release a game-client connection.
    pub fn release_game(&self, ip: IpAddr) {
        self.game_admission
            .lock()
            .expect("admission lock")
            .release_ip(ip);
        self.connections.fetch_sub(1, Ordering::Relaxed);
    }

    /// Release a chat-gateway connection.
    pub fn release_gateway(&self, ip: IpAddr) {
        self.gateway_admission
            .lock()
            .expect("admission lock")
            .release_ip(ip);
        self.connections.fetch_sub(1, Ordering::Relaxed);
    }

    /// Look up an account by name, case-insensitively.
    #[must_use]
    pub fn account(&self, name: &str) -> Option<Account> {
        self.inner
            .lock()
            .expect("node lock")
            .accounts
            .get(&name.to_ascii_lowercase())
            .cloned()
    }

    /// Create an account.
    ///
    /// # Errors
    ///
    /// Returns `()` if the name is already taken.
    pub fn create_account(&self, name: &str, password_hash: [u8; 20]) -> Result<Account, ()> {
        let key = name.to_ascii_lowercase();
        let mut inner = self.inner.lock().expect("node lock");
        if inner.accounts.contains_key(&key) {
            return Err(());
        }
        inner.next_account_id += 1;
        let account = Account {
            id: inner.next_account_id,
            name: name.to_string(),
            password_hash,
        };
        inner.accounts.insert(key, account.clone());
        Ok(account)
    }

    /// Names of all live channels, for `SID_GETCHANNELLIST`.
    #[must_use]
    pub fn channel_names(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("node lock")
            .channels
            .values()
            .map(|c| c.display().to_string())
            .collect()
    }

    /// Join a channel, creating it if it does not exist.
    ///
    /// Returns the roster as seen *before* this user arrived (for `EID_SHOWUSER`), plus
    /// the join outcome.
    ///
    /// # Errors
    ///
    /// [`JoinDenial`] when the channel is full, the user is banned, or already present.
    pub fn join_channel(
        &self,
        raw_name: &[u8],
        account: AccountId,
        display_name: &str,
        base_flags: u32,
        out: Outbound,
    ) -> Result<(JoinedChannel, Vec<(String, u32)>), JoinDenial> {
        let key = normalize_channel_name(raw_name);
        if key.is_empty() {
            return Err(JoinDenial::Full);
        }
        let display = String::from_utf8_lossy(raw_name).trim().to_string();

        let mut inner = self.inner.lock().expect("node lock");
        let max_users = self.channel_max_users;
        let channel = inner
            .channels
            .entry(key.clone())
            .or_insert_with(|| Channel::new(key.clone(), display, ChannelClass::Local, max_users));

        let existing: Vec<(String, u32)> = channel
            .members()
            .iter()
            .map(|m| (m.name.clone(), m.flags))
            .collect();

        let outcome = channel.join(account, display_name.to_string(), base_flags)?;
        let joined = JoinedChannel {
            key: key.clone(),
            display: channel.display().to_string(),
            flags: channel.flags(),
            outcome,
        };
        inner
            .subscribers
            .entry(key)
            .or_default()
            .push((account, out));
        Ok((joined, existing))
    }

    /// Leave a channel, destroying it if it is now empty and not persistent.
    pub fn leave_channel(&self, key: &[u8], account: AccountId) -> Option<AccountId> {
        let mut inner = self.inner.lock().expect("node lock");
        let Some(channel) = inner.channels.get_mut(key) else {
            return None;
        };
        let outcome = channel.leave(account);
        if let Some(subs) = inner.subscribers.get_mut(key) {
            subs.retain(|(id, _)| *id != account);
        }
        if outcome.should_destroy {
            inner.channels.remove(key);
            inner.subscribers.remove(key);
        }
        outcome.new_operator
    }

    /// Send pre-encoded bytes to every subscriber of a channel, optionally excluding one.
    ///
    /// Encoding happens once in the caller; this clones an `Arc` per recipient. A
    /// subscriber whose queue is full is dropped from the fanout and returned, so the
    /// caller can close it — it must never block the other 199 users.
    pub fn broadcast(&self, key: &[u8], wire: &Wire, exclude: Option<AccountId>) -> Vec<AccountId> {
        let inner = self.inner.lock().expect("node lock");
        let Some(subs) = inner.subscribers.get(key) else {
            return Vec::new();
        };
        let mut stalled = Vec::new();
        for (id, out) in subs {
            if Some(*id) == exclude {
                continue;
            }
            if !out.send(wire) {
                stalled.push(*id);
            }
        }
        stalled
    }

    /// Whether an account currently holds operator in a channel.
    #[must_use]
    pub fn is_operator(&self, key: &[u8], account: AccountId) -> bool {
        self.inner
            .lock()
            .expect("node lock")
            .channels
            .get(key)
            .is_some_and(|c| c.is_operator(account))
    }
}

/// The result of joining a channel.
#[derive(Debug, Clone)]
pub struct JoinedChannel {
    /// Normalised lookup key.
    pub key: Vec<u8>,
    /// Display name.
    pub display: String,
    /// Channel flags.
    pub flags: u32,
    /// Join outcome, including whether operator was granted.
    pub outcome: cairn_core::channel::JoinOutcome,
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::policy::ServerMode;
    use cairn_proto::chat::user_flags;

    fn node() -> Node {
        Node::new(
            Policy::for_mode(ServerMode::Gaming),
            "Test".into(),
            "motd".into(),
            Vec::new(),
        )
    }

    fn outbound(cap: usize) -> (Outbound, mpsc::Receiver<Wire>) {
        let (tx, rx) = mpsc::channel(cap);
        (Outbound::new(tx), rx)
    }

    #[test]
    fn accounts_are_case_insensitive_and_unique() {
        let n = node();
        let a = n.create_account("Zealot", [1u8; 20]).unwrap();
        assert_eq!(n.account("zealot").unwrap().id, a.id);
        assert_eq!(n.account("ZEALOT").unwrap().id, a.id);
        assert!(n.create_account("zEaLoT", [2u8; 20]).is_err());
    }

    #[test]
    fn first_joiner_of_a_channel_gets_operator() {
        let n = node();
        let (out, _rx) = outbound(4);
        let (joined, existing) = n
            .join_channel(b"Op Clan XYZ", 1, "Zealot", 0, out)
            .unwrap();
        assert!(joined.outcome.granted_operator);
        assert_eq!(joined.outcome.flags & user_flags::OPERATOR, user_flags::OPERATOR);
        assert!(existing.is_empty());
        assert_eq!(joined.key, b"op clan xyz");
    }

    #[test]
    fn channel_names_are_normalized_so_variants_share_one_channel() {
        let n = node();
        let (o1, _r1) = outbound(4);
        let (o2, _r2) = outbound(4);
        n.join_channel(b"Blizzard Tech", 1, "a", 0, o1).unwrap();
        let (_, existing) = n.join_channel(b"  blizzard   TECH ", 2, "b", 0, o2).unwrap();
        assert_eq!(existing.len(), 1, "second user must land in the same channel");
    }

    #[test]
    fn a_broadcast_reaches_everyone_except_the_excluded_sender() {
        let n = node();
        let (o1, mut r1) = outbound(4);
        let (o2, mut r2) = outbound(4);
        n.join_channel(b"chat", 1, "a", 0, o1).unwrap();
        n.join_channel(b"chat", 2, "b", 0, o2).unwrap();

        let wire = Arc::new(vec![0xFFu8, 0x0F, 4, 0]);
        let stalled = n.broadcast(b"chat", &wire, Some(1));
        assert!(stalled.is_empty());
        assert!(r1.try_recv().is_err(), "sender must not receive their own talk");
        assert!(r2.try_recv().is_ok());
    }

    #[test]
    fn a_stalled_subscriber_is_reported_and_does_not_block_others() {
        let n = node();
        let (slow, _slow_rx) = outbound(1); // fills immediately
        let (fast, mut fast_rx) = outbound(64);
        n.join_channel(b"chat", 1, "slow", 0, slow).unwrap();
        n.join_channel(b"chat", 2, "fast", 0, fast).unwrap();

        let wire = Arc::new(vec![0xFFu8, 0x0F, 4, 0]);
        let mut stalled = Vec::new();
        for _ in 0..8 {
            stalled = n.broadcast(b"chat", &wire, None);
        }
        assert_eq!(stalled, vec![1], "the slow subscriber must be named");
        // The healthy subscriber still got every message.
        let mut got = 0;
        while fast_rx.try_recv().is_ok() {
            got += 1;
        }
        assert_eq!(got, 8);
    }

    #[test]
    fn an_empty_channel_is_destroyed_on_the_last_departure() {
        let n = node();
        let (out, _rx) = outbound(4);
        n.join_channel(b"temp", 1, "a", 0, out).unwrap();
        assert_eq!(n.channel_names().len(), 1);
        n.leave_channel(b"temp", 1);
        assert!(n.channel_names().is_empty());
    }

    #[test]
    fn admission_counts_are_released() {
        let n = node();
        let ip: IpAddr = "10.1.2.3".parse().unwrap();
        n.admit_game(ip).unwrap();
        assert_eq!(n.connection_count(), 1);
        n.release_game(ip);
        assert_eq!(n.connection_count(), 0);
    }

    #[test]
    fn gateway_admission_honours_the_one_per_ip_default() {
        let n = node();
        let ip: IpAddr = "10.1.2.4".parse().unwrap();
        assert!(n.admit_gateway(ip).is_ok());
        assert!(n.admit_gateway(ip).is_err(), "gaming mode allows one bot per IP");
    }
}
