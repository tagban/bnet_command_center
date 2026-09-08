//! Advertisement banners.
//!
//! The client asks with `SID_CHECKAD` (0x15) roughly every fifteen seconds, carrying **the
//! id of the banner it is currently showing**. The server answers with the next one, and
//! the client fetches the image over BNFTP using the ad id and extension tag carried in
//! the transfer header.
//!
//! # The cursor lives on the client
//!
//! Because the request carries the previous id, the server needs **no per-connection
//! rotation state at all** — it is a pure function of (previous id, product, language).
//! That is a genuinely good property inherited from how real Battle.net worked, and it is
//! worth preserving deliberately: rotation survives a server restart, costs nothing per
//! connection, and cannot drift between federated nodes.
//!
//! WarCraft III is the exception: it gets a random pick rather than a sequential one.

use bnetcc_proto::FourCc;

/// One advertisement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdBanner {
    /// Rotation id, assigned sequentially from 1 in configuration order.
    ///
    /// **Not stable across config edits** — inserting a banner renumbers everything after
    /// it. That is acceptable because the id's only job is to be the rotation cursor.
    pub id: u32,
    /// File name only, no path. Served from the ad directory.
    pub filename: String,
    /// Where a click leads.
    pub url: String,
    /// Restrict to one product, or `None` for every product.
    pub product: Option<FourCc>,
    /// Restrict to one language (`enUS`, `deDE`, …), or `None` for every language.
    pub language: Option<FourCc>,
}

impl AdBanner {
    /// Whether this banner may be shown to a given client.
    #[must_use]
    pub fn matches(&self, product: FourCc, language: Option<FourCc>) -> bool {
        let product_ok = match self.product {
            None => true,
            Some(want) => want == product,
        };
        let language_ok = match (self.language, language) {
            (None, _) => true,
            (Some(want), Some(got)) => want == got,
            // A language-restricted banner is not shown to a client that did not say.
            (Some(_), None) => false,
        };
        product_ok && language_ok
    }
}

/// The configured banner set.
#[derive(Debug, Clone, Default)]
pub struct AdRotation {
    banners: Vec<AdBanner>,
}

impl AdRotation {
    /// Build a rotation, assigning ids sequentially from 1 in the given order.
    #[must_use]
    pub fn new(banners: impl IntoIterator<Item = AdBanner>) -> Self {
        let banners = banners
            .into_iter()
            .enumerate()
            .map(|(i, mut b)| {
                b.id = i as u32 + 1;
                b
            })
            .collect();
        Self { banners }
    }

    /// Whether any banners are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.banners.is_empty()
    }

    /// Number of configured banners.
    #[must_use]
    pub fn len(&self) -> usize {
        self.banners.len()
    }

    /// Look one up by id, for `SID_QUERYADURL` and click tracking.
    #[must_use]
    pub fn by_id(&self, id: u32) -> Option<&AdBanner> {
        self.banners.iter().find(|b| b.id == id)
    }

    /// Choose the banner to show next.
    ///
    /// `previous` is the id the client says it is currently showing; `0` means "none
    /// yet". `seed` is only consulted for the random path, so callers on the sequential
    /// path may pass anything.
    ///
    /// Returns `None` when no banner matches the client, in which case the server simply
    /// does not answer — the client keeps showing whatever it has.
    #[must_use]
    pub fn next(
        &self,
        product: FourCc,
        language: Option<FourCc>,
        previous: u32,
        seed: u64,
    ) -> Option<&AdBanner> {
        let candidates: Vec<&AdBanner> = self
            .banners
            .iter()
            .filter(|b| b.matches(product, language))
            .collect();
        if candidates.is_empty() {
            return None;
        }

        if is_random_product(product) {
            let idx = (seed % candidates.len() as u64) as usize;
            return Some(candidates[idx]);
        }

        // Sequential: find where the client is and advance one, wrapping at the end.
        // If the previous id is unknown to us (0, or a banner that has since been
        // removed), start at the beginning.
        let position = candidates.iter().position(|b| b.id == previous);
        let next = match position {
            Some(i) if i + 1 < candidates.len() => i + 1,
            _ => 0,
        };
        Some(candidates[next])
    }
}

/// Whether a product gets a random banner rather than a sequential one.
///
/// WarCraft III does; everything else rotates in order.
#[must_use]
pub fn is_random_product(product: FourCc) -> bool {
    matches!(
        product,
        p if p == bnetcc_proto::product::WAR3 || p == bnetcc_proto::product::W3XP
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bnetcc_proto::product;

    fn banner(name: &str, product: Option<FourCc>, language: Option<FourCc>) -> AdBanner {
        AdBanner {
            id: 0,
            filename: name.into(),
            url: "https://example.invalid".into(),
            product,
            language,
        }
    }

    fn rotation() -> AdRotation {
        AdRotation::new([
            banner("ad000001.smk", None, None),
            banner("ad000002.smk", None, None),
            banner("ad000003.smk", None, None),
        ])
    }

    #[test]
    fn ids_are_assigned_sequentially_from_one() {
        let r = rotation();
        assert_eq!(r.by_id(1).unwrap().filename, "ad000001.smk");
        assert_eq!(r.by_id(3).unwrap().filename, "ad000003.smk");
        assert!(r.by_id(0).is_none());
        assert!(r.by_id(4).is_none());
    }

    #[test]
    fn a_fresh_client_gets_the_first_banner() {
        let r = rotation();
        assert_eq!(r.next(product::SEXP, None, 0, 0).unwrap().id, 1);
    }

    #[test]
    fn rotation_advances_and_wraps() {
        let r = rotation();
        assert_eq!(r.next(product::SEXP, None, 1, 0).unwrap().id, 2);
        assert_eq!(r.next(product::SEXP, None, 2, 0).unwrap().id, 3);
        assert_eq!(r.next(product::SEXP, None, 3, 0).unwrap().id, 1, "wraps");
    }

    #[test]
    fn rotation_is_a_pure_function_of_the_clients_cursor() {
        // The property worth protecting: no per-connection state, so the same request
        // always gets the same answer, across restarts and across federated nodes.
        let r = rotation();
        for previous in 0..5 {
            let a = r.next(product::SEXP, None, previous, 0).unwrap().id;
            let b = r.next(product::SEXP, None, previous, 999).unwrap().id;
            assert_eq!(a, b, "sequential rotation must ignore the seed");
        }
    }

    #[test]
    fn an_unknown_previous_id_restarts_the_rotation() {
        // A banner removed from the config leaves clients holding a stale id.
        let r = rotation();
        assert_eq!(r.next(product::SEXP, None, 99, 0).unwrap().id, 1);
    }

    #[test]
    fn warcraft_three_gets_a_random_pick() {
        let r = rotation();
        let ids: Vec<u32> = (0..3)
            .map(|seed| r.next(product::WAR3, None, 1, seed).unwrap().id)
            .collect();
        assert_eq!(ids, vec![1, 2, 3], "seed selects the candidate");
        // And it does not depend on the client's cursor.
        assert_eq!(r.next(product::W3XP, None, 3, 1).unwrap().id, 2);
    }

    #[test]
    fn product_restrictions_are_honoured() {
        let r = AdRotation::new([
            banner("all.smk", None, None),
            banner("sc_only.smk", Some(product::SEXP), None),
        ]);
        // Diablo II sees only the unrestricted banner, so rotation stays on it.
        assert_eq!(r.next(product::D2XP, None, 0, 0).unwrap().filename, "all.smk");
        assert_eq!(r.next(product::D2XP, None, 1, 0).unwrap().filename, "all.smk");
        // Brood War sees both.
        assert_eq!(r.next(product::SEXP, None, 1, 0).unwrap().filename, "sc_only.smk");
    }

    #[test]
    fn language_restrictions_are_honoured() {
        let en = FourCc::from_ascii(b"enUS");
        let de = FourCc::from_ascii(b"deDE");
        let r = AdRotation::new([
            banner("global.smk", None, None),
            banner("german.smk", None, Some(de)),
        ]);
        assert_eq!(
            r.next(product::SEXP, Some(en), 1, 0).unwrap().filename,
            "global.smk",
            "an English client must not be shown the German banner"
        );
        assert_eq!(
            r.next(product::SEXP, Some(de), 1, 0).unwrap().filename,
            "german.smk"
        );
        // A client that did not state a language gets only unrestricted banners.
        assert_eq!(
            r.next(product::SEXP, None, 1, 0).unwrap().filename,
            "global.smk"
        );
    }

    #[test]
    fn an_empty_rotation_answers_nothing() {
        let r = AdRotation::default();
        assert!(r.is_empty());
        assert!(r.next(product::SEXP, None, 0, 0).is_none());
    }

    #[test]
    fn a_client_with_no_matching_banner_is_simply_not_answered() {
        // Better than sending a banner it cannot render: the client keeps what it has.
        let r = AdRotation::new([banner("w3.mng", Some(product::WAR3), None)]);
        assert!(r.next(product::SEXP, None, 0, 0).is_none());
    }

    #[test]
    fn rotation_visits_every_candidate_before_repeating() {
        let r = rotation();
        let mut seen = Vec::new();
        let mut previous = 0;
        for _ in 0..3 {
            let id = r.next(product::SEXP, None, previous, 0).unwrap().id;
            seen.push(id);
            previous = id;
        }
        seen.sort_unstable();
        assert_eq!(seen, vec![1, 2, 3]);
    }
}
