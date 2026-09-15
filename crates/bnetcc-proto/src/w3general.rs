//! `SID_WARCRAFTGENERAL` (0x44): WarCraft III's matchmaking, profile and icon requests.
//!
//! Every message starts with a `u8` subcommand; requests carry a `u32` cookie the reply echoes.
//! Layouts from BNETDocs (C>S packet 393, S>C packet 292). `docs/WARCRAFT3-MATCHMAKING.md` §3.1
//! lists them with what a real 1.27b client sends and when.
//!
//! The replies here are the empty ones — no tournament, no ladder records, no icons unlocked —
//! which a server without a ladder can give truthfully.

use crate::buf::{Reader, Writer};
use crate::error::FourCc;
use crate::{product, Result};

/// Subcommand ids.
pub mod sub {
    /// `WID_GAMESEARCH`: start a matchmaking search.
    pub const GAME_SEARCH: u8 = 0x00;
    /// `WID_MAPLIST`: the map list, game types, descriptions and ladder links, as blocks.
    pub const MAP_LIST: u8 = 0x02;
    /// `WID_CANCELSEARCH`.
    pub const CANCEL_SEARCH: u8 = 0x03;
    /// `WID_USERRECORD`: a player's ladder profile.
    pub const USER_RECORD: u8 = 0x04;
    /// `WID_TOURNAMENT`: tournament status; the client polls it.
    pub const TOURNAMENT: u8 = 0x07;
    /// `WID_CLANRECORD`: a clan's ladder profile.
    pub const CLAN_RECORD: u8 = 0x08;
    /// `WID_ICONLIST`: the icons a player may choose.
    pub const ICON_LIST: u8 = 0x09;
    /// `WID_SETICON`: choose an icon.
    pub const SET_ICON: u8 = 0x0A;
}

/// Longest account name accepted in a profile request.
const NAME_MAX: usize = 64;

/// A decoded request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// `WID_GAMESEARCH`.
    GameSearch {
        /// Echoed by the reply.
        cookie: u32,
        /// 0 1v1, 1 2v2, 2 3v3, 3 4v4, 4 free for all.
        game_type: u8,
        /// One bit per map in the pool the player allows.
        maps: u16,
        /// 1 Human, 2 Orc, 4 Night Elf, 8 Undead, 0x20 random.
        race: u32,
    },
    /// `WID_MAPLIST`: the blocks wanted, `(id, checksum of the client's cached copy)`.
    MapList {
        /// Echoed by the reply.
        cookie: u32,
        /// Block ids and cached checksums (0 when not cached).
        blocks: Vec<(u32, u32)>,
    },
    /// `WID_CANCELSEARCH`.
    CancelSearch,
    /// `WID_USERRECORD`.
    UserRecord {
        /// Echoed by the reply.
        cookie: u32,
        /// Whose profile.
        account: Vec<u8>,
        /// `WAR3` or `W3XP`.
        product: FourCc,
    },
    /// `WID_TOURNAMENT`.
    Tournament {
        /// Echoed by the reply.
        cookie: u32,
    },
    /// `WID_CLANRECORD`.
    ClanRecord {
        /// Echoed by the reply.
        cookie: u32,
        /// The clan tag.
        tag: FourCc,
        /// `WAR3` or `W3XP`.
        product: FourCc,
    },
    /// `WID_ICONLIST`.
    IconList {
        /// Echoed by the reply.
        cookie: u32,
    },
    /// `WID_SETICON`.
    SetIcon {
        /// The icon chosen, e.g. `W3O1`.
        icon: FourCc,
    },
    /// A subcommand not listed above.
    Other(u8),
}

/// Decode a request body.
///
/// # Errors
///
/// [`crate::ProtoError`] if a listed subcommand is truncated.
pub fn parse(body: &[u8]) -> Result<Request> {
    let mut r = Reader::new(body);
    let request = match r.u8()? {
        sub::GAME_SEARCH => {
            let cookie = r.u32()?;
            let _ = (r.u32()?, r.u8()?);
            let game_type = r.u8()?;
            let maps = r.u16()?;
            let _ = (r.u16()?, r.u8()?, r.u32()?);
            Request::GameSearch { cookie, game_type, maps, race: r.u32()? }
        }
        sub::MAP_LIST => {
            let cookie = r.u32()?;
            let n = r.u8()?;
            let blocks = (0..n).map(|_| Ok((r.u32()?, r.u32()?))).collect::<Result<Vec<_>>>()?;
            Request::MapList { cookie, blocks }
        }
        sub::CANCEL_SEARCH => Request::CancelSearch,
        sub::USER_RECORD => {
            let cookie = r.u32()?;
            let account = r.cstr(NAME_MAX)?.to_vec();
            Request::UserRecord { cookie, account, product: r.fourcc()? }
        }
        sub::TOURNAMENT => Request::Tournament { cookie: r.u32()? },
        sub::CLAN_RECORD => {
            let cookie = r.u32()?;
            let tag = r.fourcc()?;
            Request::ClanRecord { cookie, tag, product: r.fourcc()? }
        }
        sub::ICON_LIST => Request::IconList { cookie: r.u32()? },
        sub::SET_ICON => Request::SetIcon { icon: r.fourcc()? },
        other => Request::Other(other),
    };
    Ok(request)
}

/// How many per-race records a profile carries: Human, Orc, Night Elf, Undead, Random, and the
/// expansion adds one.
#[must_use]
pub fn race_records(p: FourCc) -> u8 {
    if p == product::W3XP {
        6
    } else {
        5
    }
}

/// `WID_TOURNAMENT` reply: status 0, no tournament, and zeroed fields.
#[must_use]
pub fn no_tournament(cookie: u32) -> Vec<u8> {
    let mut w = Writer::with_capacity(25);
    w.u8(sub::TOURNAMENT).u32(cookie);
    w.u8(0); // status: no tournament
    w.u64(0); // FILETIME of the last status change
    w.u16(0).u16(0);
    w.u8(0).u8(0).u8(0); // wins, losses, draws
    w.bytes(&[0; 4]);
    w.finish()
}

/// `WID_USERRECORD` reply for a player with no ladder games: icon 0, no ladder or team
/// records, `races` zeroed race records, no last game, no partners.
#[must_use]
pub fn empty_user_record(cookie: u32, races: u8) -> Vec<u8> {
    let mut w = Writer::with_capacity(32);
    w.u8(sub::USER_RECORD).u32(cookie);
    w.u32(0); // icon
    w.u8(0); // ladder records
    w.u8(races);
    for _ in 0..races {
        w.u16(0).u16(0);
    }
    w.u8(0); // team records
    w.u64(0); // FILETIME of the last game
    w.u8(0); // partners
    w.finish()
}

/// `WID_CLANRECORD` reply for a clan with no ladder games.
#[must_use]
pub fn empty_clan_record(cookie: u32, races: u8) -> Vec<u8> {
    let mut w = Writer::with_capacity(8 + usize::from(races) * 8);
    w.u8(sub::CLAN_RECORD).u32(cookie);
    w.u8(0); // ladder records
    w.u8(races);
    for _ in 0..races {
        w.u32(0).u32(0);
    }
    w.finish()
}

/// `WID_ICONLIST` reply offering nothing: no icon selected, no tiers, no icons.
#[must_use]
pub fn empty_icon_list(cookie: u32) -> Vec<u8> {
    let mut w = Writer::with_capacity(11);
    w.u8(sub::ICON_LIST).u32(cookie).u32(0).u8(0).u8(0);
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    #[test]
    fn a_logon_map_list_request_names_five_blocks() {
        // tagban's 1.27b client, right after logon (docs/WARCRAFT3-MATCHMAKING.md §3.1).
        let body = hex(
            "02 01 00 00 00 05 4c 52 55 00 00 00 00 00 50 41 4d 00 00 00 00 00 45 50 59 54 00 00 00 00 \
             43 53 45 44 00 00 00 00 52 44 41 4c 00 00 00 00",
        );
        let Request::MapList { cookie, blocks } = parse(&body).unwrap() else { panic!() };
        assert_eq!(cookie, 1);
        let ids: Vec<[u8; 4]> = blocks.iter().map(|&(id, _)| id.to_be_bytes()).collect();
        assert_eq!(ids, [*b"\0URL", *b"\0MAP", *b"TYPE", *b"DESC", *b"LADR"], "multi-character constants");
        assert!(blocks.iter().all(|&(_, checksum)| checksum == 0));
    }

    #[test]
    fn a_game_search_carries_type_maps_and_race() {
        // A search posted as a comment on BNETDocs' C>S 0x44 page (body after the header).
        let body = hex("00 06 00 00 00 00 00 00 00 00 00 3f 0a 00 00 08 20 03 ff 00 08 00 00 00");
        assert_eq!(parse(&body).unwrap(), Request::GameSearch { cookie: 6, game_type: 0, maps: 0x0A3F, race: 8 });
    }

    #[test]
    fn other_requests_decode() {
        assert_eq!(parse(&hex("07 1a 00 00 00")).unwrap(), Request::Tournament { cookie: 0x1A });
        let mut body = vec![sub::USER_RECORD, 2, 0, 0, 0];
        body.extend_from_slice(b"Tagban\0PX3W");
        assert_eq!(
            parse(&body).unwrap(),
            Request::UserRecord { cookie: 2, account: b"Tagban".to_vec(), product: product::W3XP }
        );
        assert_eq!(parse(&[sub::CANCEL_SEARCH]).unwrap(), Request::CancelSearch);
        assert_eq!(parse(&[0x42]).unwrap(), Request::Other(0x42));
        assert!(parse(&[sub::TOURNAMENT, 1]).is_err(), "truncated");
    }

    #[test]
    fn empty_replies_have_the_documented_sizes() {
        let t = no_tournament(0x1A);
        assert_eq!((t.len(), &t[..5], t[5]), (25, &[7, 0x1A, 0, 0, 0][..], 0));
        let u = empty_user_record(3, race_records(product::W3XP));
        assert_eq!(u.len(), 1 + 4 + 4 + 1 + 1 + 6 * 4 + 1 + 8 + 1);
        assert_eq!((u[10], u[11 + 24]), (6, 0), "six races, then no team records");
        assert_eq!(empty_clan_record(3, race_records(product::WAR3)).len(), 1 + 4 + 1 + 1 + 5 * 8);
        assert_eq!(empty_icon_list(9), [9, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }
}
