//! Battle.net login-time UDP check on port 6112.
//!
//! Classic clients (StarCraft, Diablo, Warcraft II BNE) will not enable game hosting or
//! joining until they confirm UDP connectivity with the server: the Create/Join buttons
//! stay greyed out otherwise.
//!
//! The exchange, confirmed against a real Warcraft II BNE client on 2026-09-09 (legacy
//! logon flow, LAN, both firewalls off):
//!
//! 1. The client binds UDP `:6112` at startup and **waits** — it does *not* probe the
//!    server on its own.
//! 2. The **server** sends the client a `PKT_SERVERPING` datagram (from its own `:6112`,
//!    so the source port is what the client expects) carrying a code.
//! 3. The client receives it and echoes the code back over TCP in `SID_UDPPINGRESPONSE`
//!    (0x14), and un-greys Create/Join.
//!
//! [`run`] also answers any datagram a client *does* send (some clients/flows probe first),
//! so both directions are covered.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use tracing::debug;

/// Server-to-client UDP ping. The client echoes `code` back in `SID_UDPPINGRESPONSE`.
pub const PKT_SERVERPING: u32 = 0x05;

/// The classic Battle.net game UDP port. Clients bind it to host/join and to run the check.
pub const GAME_PORT: u16 = 6112;

/// The code the server sends in `PKT_SERVERPING`. The value is arbitrary; the client only
/// has to echo it. `bnet` is the conventional token seen in `SID_UDPPINGRESPONSE`.
pub const UDP_CODE: u32 = u32::from_le_bytes(*b"bnet");

/// Encode a `PKT_SERVERPING` datagram: `(UINT32) command`, `(UINT32) code`.
#[must_use]
pub fn server_ping() -> [u8; 8] {
    let mut buf = [0u8; 8];
    buf[..4].copy_from_slice(&PKT_SERVERPING.to_le_bytes());
    buf[4..].copy_from_slice(&UDP_CODE.to_le_bytes());
    buf
}

/// A short hex preview for logging captured datagrams.
fn hex(bytes: &[u8]) -> String {
    let shown = &bytes[..bytes.len().min(64)];
    let mut s = String::with_capacity(shown.len() * 2);
    for b in shown {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Run the UDP receive loop: log each datagram and reply with `PKT_SERVERPING` to its
/// source, so a client that probes first still sees the round trip succeed.
pub async fn run(socket: Arc<UdpSocket>) {
    let mut buf = [0u8; 512];
    loop {
        match socket.recv_from(&mut buf).await {
            Ok((n, from)) => {
                // Never reply to a datagram that appears to come from our own socket
                // (loopback, our port). A reply to that address would land back on this
                // socket and loop forever. No real client's probe originates there.
                if from.ip().is_loopback() && from.port() == GAME_PORT {
                    debug!(%from, len = n, "dropping self-addressed UDP datagram");
                    continue;
                }
                debug!(%from, len = n, body = %hex(&buf[..n]), "recv UDP datagram");
                let reply = server_ping();
                if let Err(e) = socket.send_to(&reply, from).await {
                    debug!(%from, error = %e, "failed to reply PKT_SERVERPING");
                }
            }
            Err(e) => {
                debug!(error = %e, "UDP recv error");
            }
        }
    }
}

/// Proactively send `PKT_SERVERPING` to a client's game port so its login-time UDP check
/// succeeds (un-greying Create/Join). Sent from the server's own `:6112` socket so the
/// datagram's source port is 6112, which is where the client expects the ping to originate.
///
/// Skips clients that share the server's host: a send to one of our own addresses from the
/// `:6112` socket is delivered right back to that socket, and its receive loop would then
/// reply to it forever. (A client on the same host can't bind `:6112` for hosting anyway,
/// since the server holds it, so its UDP check cannot pass locally regardless.)
///
/// UDP is lossy and the client may not have drained its socket yet, so a few spaced
/// datagrams are sent. Fire-and-forget: every failure is non-fatal.
pub async fn ping_client(socket: Arc<UdpSocket>, client_ip: IpAddr) {
    if is_local_host(client_ip).await {
        debug!(%client_ip, "skipping UDP ping to a client sharing our host (would self-loop)");
        return;
    }
    let target = SocketAddr::new(client_ip, GAME_PORT);
    // Retry over several seconds. The first sends right after the client connects can hit a
    // transient EHOSTUNREACH on this host (the outbound ARP/route to the client not yet
    // primed, e.g. while the client's network stack is settling), and UDP is lossy on top of
    // that — so keep trying on a spread-out schedule until a few datagrams actually land.
    let schedule = [0u64, 300, 700, 1500, 3000, 5000, 8000];
    let mut sent = 0u32;
    for (attempt, delay) in schedule.into_iter().enumerate() {
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        match socket.send_to(&server_ping(), target).await {
            Ok(_) => {
                debug!(%target, attempt = attempt + 1, "sent PKT_SERVERPING (UDP)");
                sent += 1;
                if sent >= 3 {
                    break; // enough copies landed to beat UDP loss
                }
            }
            Err(e) => {
                debug!(%target, attempt = attempt + 1, error = %e, "failed to send PKT_SERVERPING");
            }
        }
    }
    if sent == 0 {
        debug!(%target, "every PKT_SERVERPING send failed; client keeps the No-UDP state");
    }
}

/// Whether `ip` is one of this host's own addresses.
///
/// Dependency-free and `#![forbid(unsafe_code)]`-clean: no `getifaddrs`. It connects a
/// throwaway UDP socket to `ip` and reads the source address the kernel selected — which
/// equals the destination only when the destination is a local interface address. Loopback
/// and unspecified addresses are treated as local outright. Conservative on error (returns
/// `false`), because a wrongly-skipped ping just leaves games greyed, while a wrong "remote"
/// on a same-host client is caught anyway by [`run`]'s self-addressed-datagram guard.
async fn is_local_host(ip: IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    let bind: SocketAddr = if ip.is_ipv4() {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (Ipv6Addr::UNSPECIFIED, 0).into()
    };
    match UdpSocket::bind(bind).await {
        Ok(probe) => {
            if probe.connect(SocketAddr::new(ip, GAME_PORT)).await.is_err() {
                return false;
            }
            probe.local_addr().map(|a| a.ip() == ip).unwrap_or(false)
        }
        Err(_) => false,
    }
}
