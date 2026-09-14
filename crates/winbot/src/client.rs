//! One win-bot account on the chat server: its connection, its login, and the chat and game
//! packets a duel needs.
//!
//! The login flows follow `crates/massload`'s bots, chosen by the product's auth family as the
//! server chooses: modern X-SHA-1 (`STAR`, `SEXP`), legacy X-SHA-1 (`W2BN`), and NLS/SRP (`WAR3`,
//! `W3XP`). CD keys are made up per account: this server keys uniqueness on the key's product
//! and public values and does not check them against real keys.

use std::time::Duration;

use bnetcc_crypto::{logon_proof, password_hash};
use bnetcc_proto::bncs::{decode_frame, encode_frame, sid, Frame, DEFAULT_MAX_FRAME};
use bnetcc_proto::buf::{RecvBuf, Writer};
use bnetcc_proto::product::{self, AuthFamily};
use bnetcc_proto::FourCc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A step the server refused, or a dropped connection.
#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self(format!("connection: {e}"))
    }
}

impl From<bnetcc_proto::ProtoError> for Error {
    fn from(e: bnetcc_proto::ProtoError) -> Self {
        Self(format!("malformed reply: {e}"))
    }
}

/// The version byte the final classic build of `product` reports.
fn version_byte(product: FourCc) -> u32 {
    match product {
        p if p == product::W2BN => 0x4F,
        p if p == product::WAR3 || p == product::W3XP => 0x1E,
        _ => 0xD3,
    }
}

/// One logged-in account.
pub struct Client {
    stream: TcpStream,
    buf: RecvBuf,
    /// The product it plays.
    pub product: FourCc,
    /// The account it logged in as: the name game results and records go by.
    pub account: String,
    /// The name chat shows it by (the account, a realm suffix, or a `#N` for a second login).
    pub name: String,
}

impl Client {
    /// Connect, log in as `name` (creating the account if it is new), enter chat and join
    /// `channel`.
    pub async fn login(server: &str, product: FourCc, name: &str, password: &str, channel: &str) -> Result<Self, Error> {
        let family = product::auth_family(product).ok_or_else(|| Error(format!("{product} is not a product this bot plays")))?;
        let mut stream = TcpStream::connect(server).await?;
        stream.set_nodelay(true)?;
        stream.write_all(&[0x01]).await?; // protocol selector: a game client
        let mut client = Self { stream, buf: RecvBuf::with_capacity(4096), product, account: name.to_string(), name: name.to_string() };
        match family {
            AuthFamily::Xsha1 => client.modern_login(password).await?,
            AuthFamily::LegacyXsha1 => client.legacy_login(password).await?,
            AuthFamily::Srp => client.srp_login(password).await?,
        }
        client.enter_chat(channel).await?;
        Ok(client)
    }

    /// Send a packet.
    pub async fn send(&mut self, frame: &Frame) -> Result<(), Error> {
        let mut out = Vec::with_capacity(frame.wire_len());
        encode_frame(frame, &mut out).map_err(|e| Error(e.to_string()))?;
        self.stream.write_all(&out).await?;
        Ok(())
    }

    /// The next packet, answering the server's pings on the way. Safe to drop while waiting
    /// (as [`Self::idle`] does): bytes join the buffer only once they are read.
    pub async fn recv(&mut self) -> Result<Frame, Error> {
        loop {
            match decode_frame(&mut self.buf, DEFAULT_MAX_FRAME) {
                Ok(Some(frame)) if frame.id == sid::PING => {
                    self.send(&frame).await?;
                    continue;
                }
                Ok(Some(frame)) => return Ok(frame),
                Ok(None) => {}
                Err(e) => return Err(Error(format!("unreadable packet from the server: {e}"))),
            }
            let mut chunk = [0u8; 4096];
            let n = self.stream.read(&mut chunk).await?;
            if n == 0 {
                return Err(Error("the server closed the connection".into()));
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// Read until a packet `id` arrives; everything else (chat events, ads) is dropped.
    pub async fn expect(&mut self, id: u8) -> Result<Frame, Error> {
        loop {
            let frame = self.recv().await?;
            if frame.id == id {
                return Ok(frame);
            }
        }
    }

    /// Stay connected for `wait`: answer pings, drain chat, and send a keepalive every 30 s.
    pub async fn idle(&mut self, wait: Duration) -> Result<(), Error> {
        let until = tokio::time::Instant::now() + wait;
        let mut keepalive = tokio::time::interval(Duration::from_secs(30));
        keepalive.tick().await;
        loop {
            tokio::select! {
                () = tokio::time::sleep_until(until) => return Ok(()),
                _ = keepalive.tick() => self.send(&Frame::empty(sid::NULL)).await?,
                frame = self.recv() => { frame?; }
            }
        }
    }

    async fn modern_login(&mut self, password: &str) -> Result<(), Error> {
        let (client_token, server_token) = self.version_handshake().await?;
        self.create_account(sid::CREATEACCOUNT2, password).await?;
        let proof = logon_proof(client_token, server_token, &password_hash(password));
        let mut w = Writer::new();
        w.u32(client_token).u32(server_token).bytes(&proof).cstr(self.name.as_bytes());
        self.send(&Frame::new(sid::LOGONRESPONSE2, w.finish())).await?;
        match self.expect(sid::LOGONRESPONSE2).await?.reader().u32()? {
            0 => Ok(()),
            1 => Err(Error(format!("no account {}", self.name))),
            2 => Err(Error(format!("wrong password for {}", self.name))),
            s => Err(Error(format!("logon refused, status {s:#x}"))),
        }
    }

    async fn legacy_login(&mut self, password: &str) -> Result<(), Error> {
        let mut w = Writer::new();
        w.fourcc(FourCc::from_ascii(b"IX86")).fourcc(self.product).u32(version_byte(self.product));
        self.send(&Frame::new(sid::STARTVERSIONING, w.finish())).await?;
        self.expect(sid::STARTVERSIONING).await?;
        let mut w = Writer::new();
        w.u32(0).fourcc(self.product).u32(0).u32(0).cstr(b"winbot");
        self.send(&Frame::new(sid::REPORTVERSION, w.finish())).await?;
        if self.expect(sid::REPORTVERSION).await?.reader().u32()? != 2 {
            return Err(Error("version check failed".into()));
        }
        let (product_value, public_value) = self.key(0);
        let mut w = Writer::new();
        w.u32(0).u32(16).u32(product_value).u32(public_value).u32(0).bytes(&[0; 20]).cstr(self.name.as_bytes());
        self.send(&Frame::new(sid::CDKEY2, w.finish())).await?;
        if self.expect(sid::CDKEY2).await?.reader().u32()? != 1 {
            return Err(Error("CD-key check failed".into()));
        }
        self.create_account(sid::CREATEACCOUNT, password).await?;
        let client_token = self.client_token(0);
        let proof = logon_proof(client_token, 0, &password_hash(password));
        let mut w = Writer::new();
        w.u32(client_token).u32(0).bytes(&proof).cstr(self.name.as_bytes());
        self.send(&Frame::new(sid::LOGONRESPONSE, w.finish())).await?;
        if self.expect(sid::LOGONRESPONSE).await?.reader().u32()? != 1 {
            return Err(Error(format!("logon refused for {}", self.name)));
        }
        Ok(())
    }

    async fn srp_login(&mut self, password: &str) -> Result<(), Error> {
        use bnetcc_crypto::nls;
        self.version_handshake().await?;
        let (user, salt, a) = (self.name.clone(), self.bytes32("salt"), self.bytes32("a"));
        let mut w = Writer::new();
        w.bytes(&salt).bytes(&nls::verifier(&user, password, &salt)).cstr(user.as_bytes());
        self.send(&Frame::new(sid::AUTH_ACCOUNTCREATE, w.finish())).await?;
        let status = self.expect(sid::AUTH_ACCOUNTCREATE).await?.reader().u32()?;
        if status != 0 && status != 4 {
            return Err(Error(format!("account creation refused, status {status:#x}")));
        }
        let client_public = nls::client_public(&a);
        let mut w = Writer::new();
        w.bytes(&client_public).cstr(user.as_bytes());
        self.send(&Frame::new(sid::AUTH_ACCOUNTLOGON, w.finish())).await?;
        let reply = self.expect(sid::AUTH_ACCOUNTLOGON).await?;
        let mut r = reply.reader();
        let status = r.u32()?;
        if status != 0 {
            return Err(Error(format!("logon refused, status {status:#x}")));
        }
        let (server_salt, server_public): ([u8; 32], [u8; 32]) = (r.array()?, r.array()?);
        let (m1, key) = nls::client_proof(&user, password, &server_salt, &a, &server_public).ok_or_else(|| Error("server sent B = 0".into()))?;
        let mut w = Writer::new();
        w.bytes(&m1);
        self.send(&Frame::new(sid::AUTH_ACCOUNTLOGONPROOF, w.finish())).await?;
        let reply = self.expect(sid::AUTH_ACCOUNTLOGONPROOF).await?;
        let mut r = reply.reader();
        let status = r.u32()?;
        if status != 0 {
            return Err(Error(format!("wrong password for {user} (status {status:#x})")));
        }
        let m2: [u8; 20] = r.array()?;
        if m2 != nls::server_proof_from_key(&client_public, &m1, &key) {
            return Err(Error("the server's proof does not verify".into()));
        }
        Ok(())
    }

    /// `SID_AUTH_INFO` and `SID_AUTH_CHECK` with this account's made-up keys.
    async fn version_handshake(&mut self) -> Result<(u32, u32), Error> {
        let mut w = Writer::new();
        w.u32(0).fourcc(FourCc::from_ascii(b"IX86")).fourcc(self.product).u32(version_byte(self.product));
        w.u32(0).u32(0).u32(0).u32(0).u32(0).cstr(b"USA").cstr(b"United States");
        self.send(&Frame::new(sid::AUTH_INFO, w.finish())).await?;
        let reply = self.expect(sid::AUTH_INFO).await?;
        let mut r = reply.reader();
        let _logon_type = r.u32()?;
        let server_token = r.u32()?;
        let client_token = self.client_token(server_token);
        let keys: &[usize] = if self.product == product::W3XP { &[0, 1] } else { &[0] };
        let mut w = Writer::new();
        w.u32(client_token).u32(0).u32(0).u32(keys.len() as u32).u32(0);
        for &i in keys {
            let (product_value, public_value) = self.key(i);
            w.u32(if self.product == product::WAR3 || self.product == product::W3XP { 26 } else { 13 });
            w.u32(product_value).u32(public_value).u32(0).bytes(&[0; 20]);
        }
        w.cstr(b"winbot").cstr(self.name.as_bytes());
        self.send(&Frame::new(sid::AUTH_CHECK, w.finish())).await?;
        match self.expect(sid::AUTH_CHECK).await?.reader().u32()? {
            0 => Ok((client_token, server_token)),
            0x201 | 0x211 => Err(Error(format!("{}'s made-up CD key is in use by another session", self.name))),
            s => Err(Error(format!("version or CD-key check failed, status {s:#x}"))),
        }
    }

    /// Create the account; one that already exists is fine.
    async fn create_account(&mut self, packet: u8, password: &str) -> Result<(), Error> {
        let mut w = Writer::new();
        w.bytes(&password_hash(password)).cstr(self.name.as_bytes());
        self.send(&Frame::new(packet, w.finish())).await?;
        let status = self.expect(packet).await?.reader().u32()?;
        let created = if packet == sid::CREATEACCOUNT { status == 1 } else { status == 0 };
        // Modern: 0 created, 4 name taken. Legacy: 1 created, 0 failed (taken, most often).
        if created || status == 4 || (packet == sid::CREATEACCOUNT && status == 0) {
            Ok(())
        } else {
            Err(Error(format!("account creation refused for {}, status {status:#x}", self.name)))
        }
    }

    async fn enter_chat(&mut self, channel: &str) -> Result<(), Error> {
        let mut w = Writer::new();
        w.cstr(self.name.as_bytes()).cstr(b"");
        self.send(&Frame::new(sid::ENTERCHAT, w.finish())).await?;
        let reply = self.expect(sid::ENTERCHAT).await?;
        let unique = String::from_utf8_lossy(reply.reader().cstr(64)?).into_owned();
        if !unique.is_empty() {
            self.name = unique;
        }
        let mut w = Writer::new();
        w.u32(if channel.is_empty() { 1 } else { 2 }).cstr(channel.as_bytes());
        self.send(&Frame::new(sid::JOINCHANNEL, w.finish())).await?;
        self.expect(sid::CHATEVENT).await?;
        Ok(())
    }

    /// Advertise a game (`SID_STARTADVEX3`) of `game_type` and `ladder` type.
    pub async fn host(&mut self, game: &str, game_type: u16, ladder: u32) -> Result<(), Error> {
        let statstring = format!(",,,,1,,,,,,{}\rWin Bot Arena\r", self.account);
        let mut w = Writer::new();
        w.u32(0).u32(0).u16(game_type).u16(1).u32(0xFF).u32(ladder).cstr(game.as_bytes()).cstr(b"").cstr(statstring.as_bytes());
        self.send(&Frame::new(sid::STARTADVEX3, w.finish())).await?;
        match self.expect(sid::STARTADVEX3).await?.reader().u32()? {
            0 => Ok(()),
            s => Err(Error(format!("the server would not advertise {game:?}, status {s:#x}"))),
        }
    }

    /// Join a game (`SID_NOTIFYJOIN`) and leave chat, as a client does.
    pub async fn join(&mut self, game: &str) -> Result<(), Error> {
        let mut w = Writer::new();
        w.fourcc(self.product).u32(version_byte(self.product)).cstr(game.as_bytes()).cstr(b"");
        self.send(&Frame::new(sid::NOTIFYJOIN, w.finish())).await?;
        self.send(&Frame::empty(sid::LEAVECHAT)).await
    }

    /// The host starts its game: the ad comes down (`SID_STOPADV`) and it leaves chat.
    pub async fn start(&mut self) -> Result<(), Error> {
        self.send(&Frame::empty(sid::STOPADV)).await?;
        self.send(&Frame::empty(sid::LEAVECHAT)).await
    }

    /// Report the game (`SID_GAMERESULT`): `game_type` 0 normal, 1 ladder, 3 Iron Man; each
    /// slot's player and code (1 win, 2 loss, 3 draw, 4 disconnect).
    pub async fn report(&mut self, game_type: u32, slots: &[(&str, u32)]) -> Result<(), Error> {
        let mut w = Writer::new();
        w.u32(game_type).u32(slots.len() as u32);
        for (_, code) in slots {
            w.u32(*code);
        }
        for (name, _) in slots {
            w.cstr(name.as_bytes());
        }
        w.cstr(b"Win Bot Arena").cstr(b"");
        self.send(&Frame::new(sid::GAMERESULT, w.finish())).await
    }

    /// Leave the game (`SID_LEAVEGAME`) and go back to chat in `channel`.
    pub async fn leave(&mut self, channel: &str) -> Result<(), Error> {
        self.send(&Frame::empty(sid::LEAVEGAME)).await?;
        self.enter_chat(channel).await
    }

    /// Stored values for `keys` of this account (`SID_READUSERDATA`).
    pub async fn read(&mut self, keys: &[String]) -> Result<Vec<String>, Error> {
        let mut w = Writer::new();
        w.u32(1).u32(keys.len() as u32).u32(0x5742).cstr(self.account.as_bytes());
        for k in keys {
            w.cstr(k.as_bytes());
        }
        self.send(&Frame::new(sid::READUSERDATA, w.finish())).await?;
        let reply = self.expect(sid::READUSERDATA).await?;
        let mut r = reply.reader();
        r.bytes(12)?;
        keys.iter().map(|_| Ok(String::from_utf8_lossy(r.cstr(512)?).into_owned())).collect()
    }

    /// A player's ladder rank (1 the best), `None` when unranked (`SID_FINDLADDERUSER`).
    pub async fn rank(&mut self, league: u32, name: &str) -> Result<Option<u32>, Error> {
        let mut w = Writer::new();
        w.fourcc(self.product).u32(league).u32(0).cstr(name.as_bytes());
        self.send(&Frame::new(sid::FINDLADDERUSER, w.finish())).await?;
        let index = self.expect(sid::FINDLADDERUSER).await?.reader().u32()?;
        Ok((index != u32::MAX).then(|| index + 1))
    }

    /// This account's made-up CD key `i`: `(product value, public value)`.
    fn key(&self, i: usize) -> (u32, u32) {
        let product_value = fnv(format!("{}:{i}", self.product).as_bytes()).max(1);
        let public_value = fnv(format!("winbot:{}:{}:{i}", self.product, self.account.to_ascii_lowercase()).as_bytes()).max(1);
        (product_value, public_value)
    }

    fn client_token(&self, server_token: u32) -> u32 {
        fnv(format!("{}:{server_token}", self.account).as_bytes()) | 1
    }

    /// 32 bytes derived from the name, for an SRP salt or exponent: stable across runs, so an
    /// existing account logs on again.
    fn bytes32(&self, label: &str) -> [u8; 32] {
        let a = bnetcc_crypto::xsha1_bytes(format!("{}:{label}:1", self.account).as_bytes());
        let b = bnetcc_crypto::xsha1_bytes(format!("{}:{label}:2", self.account).as_bytes());
        let mut out = [0u8; 32];
        out[..20].copy_from_slice(&a);
        out[20..].copy_from_slice(&b[..12]);
        out
    }
}

fn fnv(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5u32, |h, &b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193))
}
