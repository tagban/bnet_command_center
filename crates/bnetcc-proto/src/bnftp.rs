//! BNFTP — the Battle.net file transfer protocol (protocol byte `0x02`).
//!
//! Clients open a **separate** TCP connection for each file: `icons.bni`, `tos.txt`, ad
//! banner images, and patch MPQs. The connection carries exactly one request and one
//! response, then closes.
//!
//! ```text
//! request   u16 length (whole request)
//!           u16 version (0x0100 = v1)
//!           u32 platform, u32 product
//!           u32 ad banner id, u32 ad banner extension   (0 for ordinary files)
//!           u32 start position                          (resume support)
//!           u64 filetime                                (0 = send me the latest)
//!           cstr filename
//!
//! response  u16 header length (excluding file data)
//!           u16 type
//!           u32 file size
//!           u32 ad banner id, u32 ad banner extension
//!           u64 filetime
//!           cstr filename
//!           file data
//! ```
//!
//! The ad-banner fields living in the *file transfer* header is not an accident: it is
//! how an advertisement image is fetched, tying the `SID_CHECKAD` response to the
//! transfer that satisfies it.
//!
//! **BNFTP v2 (`0x0200`)** is what WarCraft III uses to fetch its CheckRevision MPQ. Its
//! request omits `start_position`/`filetime` (a 20-byte header) and places the filename
//! *after* the header rather than inside the declared length. [`decode_request`] handles
//! both versions; we serve v2 with the same response header (no CD-key challenge — a
//! private server gates on nothing there, and a real patched WC3 client fetches the file
//! with a plain response, same as PvPGN).

use crate::buf::{Reader, Writer};
use crate::error::{FourCc, ProtoError, Result};

/// Protocol version 1.
pub const VERSION_1: u16 = 0x0100;

/// Protocol version 2, which adds a CD-key challenge. Not implemented.
pub const VERSION_2: u16 = 0x0200;

/// Longest filename we will accept in a request.
pub const MAX_FILENAME: usize = 260;

/// The largest request we will buffer from an unauthenticated peer.
pub const MAX_REQUEST: usize = 512;

/// A BNFTP file request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Protocol version.
    pub version: u16,
    /// Client platform (`IX86`, `PMAC`, `XMAC`).
    pub platform: FourCc,
    /// Client product (`STAR`, `WAR3`, …).
    pub product: FourCc,
    /// Advertisement id, or 0 for an ordinary file.
    pub ad_id: u32,
    /// Advertisement file extension tag, or 0 for an ordinary file.
    pub ad_extension: u32,
    /// Byte offset to resume from.
    pub start_position: u32,
    /// The client's cached filetime; 0 asks for the newest version.
    pub filetime: u64,
    /// Requested filename, as sent. **Not yet validated** — see [`sanitize_filename`].
    pub filename: Vec<u8>,
}

/// A BNFTP response header. The file bytes follow it on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseHeader {
    /// Response type. Zero in practice.
    pub kind: u16,
    /// Size of the file being sent.
    pub file_size: u32,
    /// Echoed advertisement id.
    pub ad_id: u32,
    /// Echoed advertisement extension tag.
    pub ad_extension: u32,
    /// The file's modification time.
    pub filetime: u64,
    /// The filename being served.
    pub filename: Vec<u8>,
}

/// Try to decode a request from a buffer.
///
/// Returns `Ok(None)` if the whole request has not arrived yet.
///
/// # Errors
///
/// [`ProtoError::FrameTooLarge`] if the declared length is implausible, or a parse error
/// for a malformed body. Both are fatal to the connection and to nothing else.
pub fn decode_request(buf: &[u8]) -> Result<Option<Request>> {
    // Both the length and version words are needed to choose the layout.
    if buf.len() < 4 {
        return Ok(None);
    }
    let declared = u16::from_le_bytes([buf[0], buf[1]]) as usize;
    if declared > MAX_REQUEST {
        return Err(ProtoError::FrameTooLarge {
            len: declared,
            max: MAX_REQUEST,
        });
    }
    let version = u16::from_le_bytes([buf[2], buf[3]]);

    // WarCraft III speaks BNFTP **v2** (0x0200), and its request differs from v1 in two ways
    // that together made our v1-only decoder reject it outright — which is why a WC3 client
    // could never fetch its CheckRevision MPQ and stalled forever in "versioning" (it never
    // reached SID_AUTH_CHECK). In v2 the `declared` length is the *header* length (it omits
    // `start_position` and `filetime`, so the real header is 20 bytes, not v1's 32+) and the
    // filename follows the header rather than living inside `declared`. Only platform,
    // product and the filename are needed to serve a file, so read those and ignore the rest
    // of the header (the ad-banner dwords), which keeps this robust to header-size variation.
    if version == VERSION_2 {
        // len(2) + ver(2) + platform(4) + product(4) = 12 bytes minimum; real clients send 20.
        if declared < 12 {
            return Err(ProtoError::FrameTooLarge {
                len: declared,
                max: MAX_REQUEST,
            });
        }
        if buf.len() < declared {
            return Ok(None);
        }
        let mut r = Reader::new(&buf[4..declared]);
        let platform = r.fourcc()?;
        let product = r.fourcc()?;
        // Remaining header bytes (ad-banner id/extension) are not needed to serve a file.
        let ad_id = r.u32().unwrap_or(0);
        let ad_extension = r.u32().unwrap_or(0);
        // The NUL-terminated filename follows the fixed header, starting at `declared`.
        let rest = &buf[declared..];
        let Some(nul) = rest.iter().position(|&b| b == 0) else {
            return Ok(None); // filename has not fully arrived yet
        };
        let filename = rest[..nul].to_vec();
        if filename.len() > MAX_FILENAME {
            return Err(ProtoError::FrameTooLarge {
                len: filename.len(),
                max: MAX_FILENAME,
            });
        }
        return Ok(Some(Request {
            version,
            platform,
            product,
            ad_id,
            ad_extension,
            start_position: 0,
            filetime: 0,
            filename,
        }));
    }

    // v1 (StarCraft, Diablo, Warcraft II BNE, old Mac): `declared` spans the whole request,
    // filename included, and the header carries start_position + filetime.
    if !(24..=MAX_REQUEST).contains(&declared) {
        return Err(ProtoError::FrameTooLarge {
            len: declared,
            max: MAX_REQUEST,
        });
    }
    if buf.len() < declared {
        return Ok(None);
    }

    let mut r = Reader::new(&buf[2..declared]);
    let version = r.u16()?;
    let platform = r.fourcc()?;
    let product = r.fourcc()?;
    let ad_id = r.u32()?;
    let ad_extension = r.u32()?;
    let start_position = r.u32()?;
    let filetime = r.u64()?;
    let filename = r.cstr(MAX_FILENAME)?.to_vec();

    Ok(Some(Request {
        version,
        platform,
        product,
        ad_id,
        ad_extension,
        start_position,
        filetime,
        filename,
    }))
}

/// Encode a request. Used by tests and by any client-side tooling.
#[must_use]
pub fn encode_request(req: &Request) -> Vec<u8> {
    let mut body = Writer::with_capacity(64);
    body.u16(req.version)
        .fourcc(req.platform)
        .fourcc(req.product)
        .u32(req.ad_id)
        .u32(req.ad_extension)
        .u32(req.start_position)
        .u64(req.filetime)
        .cstr(&req.filename);
    let body = body.finish();

    let mut out = Vec::with_capacity(body.len() + 2);
    out.extend_from_slice(&((body.len() + 2) as u16).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Encode a response header. The file bytes are written after it.
#[must_use]
pub fn encode_response_header(header: &ResponseHeader) -> Vec<u8> {
    let mut body = Writer::with_capacity(64);
    body.u16(header.kind)
        .u32(header.file_size)
        .u32(header.ad_id)
        .u32(header.ad_extension)
        .u64(header.filetime)
        .cstr(&header.filename);
    let body = body.finish();

    let mut out = Vec::with_capacity(body.len() + 2);
    out.extend_from_slice(&((body.len() + 2) as u16).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Reject anything that is not a plain filename.
///
/// **This is the security boundary of the whole file-serving path.** The filename comes
/// from an unauthenticated peer and is about to be joined to a directory, so every
/// traversal form has to die here rather than at the filesystem:
///
/// - `..` in any position, including inside a longer name
/// - any `/` or `\` separator, so no subdirectories at all
/// - absolute paths and Windows drive letters
/// - NUL and other control bytes, which truncate a path in C APIs
/// - leading dots, which hide files
/// - empty names and names past [`MAX_FILENAME`]
///
/// Returns the name as `&str` only if it is safe to join to the serving directory.
#[must_use]
pub fn sanitize_filename(raw: &[u8]) -> Option<&str> {
    if raw.is_empty() || raw.len() > MAX_FILENAME {
        return None;
    }
    let name = std::str::from_utf8(raw).ok()?;
    if name.bytes().any(|b| b < 0x20 || b == 0x7F) {
        return None;
    }
    if name.contains('/') || name.contains('\\') {
        return None;
    }
    if name.contains("..") || name.starts_with('.') {
        return None;
    }
    // A Windows drive-relative name like `C:file` resolves against that drive's current
    // directory, which is not the serving directory.
    if name.contains(':') {
        return None;
    }
    // Trailing dots and spaces are stripped by Windows, so `tos.txt.` opens `tos.txt`
    // but compares unequal to it. Refuse rather than normalise.
    if name.ends_with('.') || name.ends_with(' ') {
        return None;
    }
    Some(name)
}

/// Extension tags used in the advertisement fields.
///
/// These are four-character codes holding a **dotted extension in forward order**:
/// the `u32` is built from `"kms."` so that its little-endian wire bytes read `.smk`.
///
/// ⚠️ The wire byte order here is inferred from PvPGN's tag encoding rather than from a
/// packet capture. Verify against a real client before relying on it — the failure mode
/// is a client that silently refuses to display an ad.
pub mod extension {
    use crate::error::FourCc;

    /// Smacker video. StarCraft, Warcraft II, Diablo, Diablo II.
    #[must_use]
    pub fn smk() -> FourCc {
        FourCc::from_ascii(b"kms.")
    }

    /// MNG animation. WarCraft III. PvPGN also announces PNG files with this tag.
    #[must_use]
    pub fn mng() -> FourCc {
        FourCc::from_ascii(b"gnm.")
    }

    /// PCX still image. StarCraft, Warcraft II, Diablo.
    #[must_use]
    pub fn pcx() -> FourCc {
        FourCc::from_ascii(b"xcp.")
    }

    /// The tag for a filename's extension, if it is one the clients accept.
    #[must_use]
    pub fn for_filename(name: &str) -> Option<FourCc> {
        let ext = name.rsplit('.').next()?.to_ascii_lowercase();
        match ext.as_str() {
            "smk" => Some(smk()),
            // PvPGN maps .png to the MNG tag as well. Reproduced deliberately: WarCraft
            // III accepts both, and the filename string carries the real extension.
            "mng" | "png" => Some(mng()),
            "pcx" => Some(pcx()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product;

    fn sample() -> Request {
        Request {
            version: VERSION_1,
            platform: FourCc::from_ascii(b"IX86"),
            product: product::SEXP,
            ad_id: 0,
            ad_extension: 0,
            start_position: 0,
            filetime: 0,
            filename: b"icons.bni".to_vec(),
        }
    }

    #[test]
    fn a_request_round_trips() {
        let req = sample();
        let bytes = encode_request(&req);
        assert_eq!(decode_request(&bytes).unwrap().unwrap(), req);
    }

    #[test]
    fn an_ad_request_carries_the_banner_fields() {
        let req = Request {
            ad_id: 7,
            ad_extension: extension::smk().0,
            filename: b"ad000007.smk".to_vec(),
            ..sample()
        };
        let decoded = decode_request(&encode_request(&req)).unwrap().unwrap();
        assert_eq!(decoded.ad_id, 7);
        assert_eq!(decoded.ad_extension, extension::smk().0);
    }

    #[test]
    fn a_warcraft_three_v2_request_decodes() {
        // Built from a real WC3/W3XP capture: 20-byte header (len, ver=0x0200, platform,
        // product, ad_id, ad_ext), then the filename after the header — no start_position or
        // filetime. This is the request our v1-only decoder used to reject, stranding WC3.
        let mut buf = Vec::new();
        buf.extend_from_slice(&20u16.to_le_bytes()); // header length
        buf.extend_from_slice(&VERSION_2.to_le_bytes()); // 0x0200
        buf.extend_from_slice(b"68XI"); // IX86 on the wire (reversed)
        buf.extend_from_slice(b"PX3W"); // W3XP on the wire (reversed)
        buf.extend_from_slice(&0u32.to_le_bytes()); // ad_id
        buf.extend_from_slice(&0u32.to_le_bytes()); // ad_extension
        buf.extend_from_slice(b"ver-IX86-1.mpq\0"); // filename, after the 20-byte header

        // A partial buffer (header only, no filename yet) must wait, not error.
        assert_eq!(decode_request(&buf[..20]).unwrap(), None);

        let req = decode_request(&buf).unwrap().expect("v2 request decodes");
        assert_eq!(req.version, VERSION_2);
        assert_eq!(req.platform, FourCc::from_ascii(b"IX86"));
        assert_eq!(req.product, product::W3XP);
        assert_eq!(req.filename, b"ver-IX86-1.mpq");
        // And that name passes the traversal guard, so it will actually be served.
        assert_eq!(sanitize_filename(&req.filename), Some("ver-IX86-1.mpq"));
    }

    #[test]
    fn a_partial_request_waits() {
        let bytes = encode_request(&sample());
        assert_eq!(decode_request(&bytes[..4]).unwrap(), None);
        assert_eq!(decode_request(&bytes[..bytes.len() - 1]).unwrap(), None);
        assert!(decode_request(&bytes).unwrap().is_some());
    }

    #[test]
    fn an_implausible_length_is_refused_before_buffering() {
        let mut bytes = encode_request(&sample());
        bytes[0..2].copy_from_slice(&60000u16.to_le_bytes());
        assert!(matches!(
            decode_request(&bytes),
            Err(ProtoError::FrameTooLarge { .. })
        ));
        // And a length too small to hold the fixed fields.
        bytes[0..2].copy_from_slice(&8u16.to_le_bytes());
        assert!(decode_request(&bytes).is_err());
    }

    #[test]
    fn a_response_header_round_trips_through_its_own_reader() {
        let header = ResponseHeader {
            kind: 0,
            file_size: 16_384,
            ad_id: 0,
            ad_extension: 0,
            filetime: 0x01D9_0000_0000_0000,
            filename: b"icons.bni".to_vec(),
        };
        let bytes = encode_response_header(&header);
        let declared = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
        assert_eq!(declared, bytes.len(), "header length excludes file data");

        let mut r = Reader::new(&bytes[2..]);
        assert_eq!(r.u16().unwrap(), 0);
        assert_eq!(r.u32().unwrap(), 16_384);
        assert_eq!(r.u32().unwrap(), 0);
        assert_eq!(r.u32().unwrap(), 0);
        assert_eq!(r.u64().unwrap(), 0x01D9_0000_0000_0000);
        assert_eq!(r.cstr(MAX_FILENAME).unwrap(), b"icons.bni");
    }

    #[test]
    fn ordinary_filenames_are_accepted() {
        for name in ["icons.bni", "tos.txt", "ad000001.smk", "IX86-1.mpq"] {
            assert_eq!(sanitize_filename(name.as_bytes()), Some(name));
        }
    }

    #[test]
    fn every_traversal_form_is_refused() {
        // The filename comes from an unauthenticated peer and is about to be joined to a
        // directory. Each of these is a real technique, not a hypothetical.
        let attacks: &[&[u8]] = &[
            b"../etc/passwd",
            b"..\\..\\windows\\system32\\config\\sam",
            b"..",
            b"....//etc/passwd",
            b"/etc/passwd",
            b"\\\\server\\share\\file",
            b"C:\\Windows\\win.ini",
            b"C:file",
            b"subdir/icons.bni",
            b"icons.bni\0.txt",
            b".hidden",
            b"tos.txt.",
            b"tos.txt ",
            b"",
        ];
        for a in attacks {
            assert_eq!(
                sanitize_filename(a),
                None,
                "accepted {:?}",
                String::from_utf8_lossy(a)
            );
        }
    }

    #[test]
    fn an_overlong_filename_is_refused() {
        let long = vec![b'a'; MAX_FILENAME + 1];
        assert_eq!(sanitize_filename(&long), None);
    }

    #[test]
    fn extension_tags_have_the_documented_wire_bytes() {
        // The u32 is built from "kms." so that its little-endian encoding reads ".smk".
        let mut w = Writer::new();
        w.fourcc(extension::smk());
        assert_eq!(&w.finish()[..], b".smk");

        let mut w = Writer::new();
        w.fourcc(extension::mng());
        assert_eq!(&w.finish()[..], b".mng");

        let mut w = Writer::new();
        w.fourcc(extension::pcx());
        assert_eq!(&w.finish()[..], b".pcx");
    }

    #[test]
    fn extension_tags_are_derived_from_filenames() {
        assert_eq!(extension::for_filename("ad000001.smk"), Some(extension::smk()));
        assert_eq!(extension::for_filename("banner.MNG"), Some(extension::mng()));
        // PvPGN announces PNG under the MNG tag; WarCraft III accepts both.
        assert_eq!(extension::for_filename("banner.png"), Some(extension::mng()));
        assert_eq!(extension::for_filename("icons.bni"), None);
        assert_eq!(extension::for_filename("noextension"), None);
    }

    #[test]
    fn decoder_never_panics_on_arbitrary_input() {
        let mut seed = 0xBEEF_0001u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..20_000 {
            let n = (next() % 96) as usize;
            let bytes: Vec<u8> = (0..n).map(|_| (next() & 0xFF) as u8).collect();
            let _ = decode_request(&bytes);
        }
    }
}
