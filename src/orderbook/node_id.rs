//! Identities of the hierarchy nodes (expirations, chains, strikes and
//! leaves).
//!
//! With a trade-ID root namespace configured, every node gets a UUIDv5 of
//! the root and an unambiguous encoding of its path in the hierarchy, so
//! the same root and the same path give the same id in any hierarchy and in
//! any run, and building a node reads no OS entropy. Without a root, a node
//! gets a random UUIDv4, as before.
//!
//! # Path encoding (version 1)
//!
//! `b"ocob-node/1"`, then the level tag (`1` expiration, `2` chain, `3`
//! strike, `4` leaf), then the fields of that level, in this order:
//!
//! - underlying: its UTF-8 length as an unsigned LEB128 varint, then its
//!   bytes;
//! - expiration: `0x01` and the 16-byte serialization of the normalized
//!   `Decimal` day count for [`ExpirationDate::Days`] (so `1.0` and `1.00`,
//!   which are the same key, are the same id), or `0x02`, the Unix seconds as
//!   an `i64` and the sub-second nanoseconds as a `u32`, both little endian,
//!   for [`ExpirationDate::DateTime`] (the full instant, not only the day);
//! - strike (strike and leaf levels): the `u64` strike, little endian;
//! - style (leaf level): `0` call, `1` put.
//!
//! So each level encodes exactly these fields after its tag:
//!
//! | level | tag | fields |
//! |---|---|---|
//! | expiration | `1` | underlying, expiration |
//! | chain | `2` | underlying, expiration |
//! | strike | `3` | underlying, expiration, strike |
//! | leaf | `4` | underlying, expiration, strike, style |
//!
//! The level tag fixes which fields follow, and every field has a fixed
//! width or a length prefix, so two distinct paths never encode to the same
//! bytes. Distinct encodings give distinct ids up to the collision
//! resistance of UUIDv5 (SHA-1, truncated to 122 bits): the encoding is
//! unambiguous, the hash is not a proof of injectivity.

use super::expiration_key::ExpirationKey;
use optionstratlib::{ExpirationDate, OptionStyle};
use orderbook_rs::OrderId;
use uuid::Uuid;

/// Version prefix of the path encoding.
const PATH_PREFIX: &[u8] = b"ocob-node/1";

/// The level of a node in the hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeLevel {
    /// An [`ExpirationOrderBook`](super::ExpirationOrderBook).
    Expiration,
    /// An [`OptionChainOrderBook`](super::OptionChainOrderBook).
    Chain,
    /// A [`StrikeOrderBook`](super::StrikeOrderBook).
    Strike,
    /// An [`OptionOrderBook`](super::OptionOrderBook) leaf.
    Leaf,
}

impl NodeLevel {
    const fn tag(self) -> u8 {
        match self {
            Self::Expiration => 1,
            Self::Chain => 2,
            Self::Strike => 3,
            Self::Leaf => 4,
        }
    }
}

/// The path of one node: its level and the fields that level carries.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NodePath<'a> {
    level: NodeLevel,
    underlying: &'a str,
    expiration: &'a ExpirationDate,
    strike: Option<u64>,
    style: Option<OptionStyle>,
}

impl<'a> NodePath<'a> {
    /// The path of an expiration book.
    #[must_use]
    pub(crate) const fn expiration(underlying: &'a str, expiration: &'a ExpirationDate) -> Self {
        Self {
            level: NodeLevel::Expiration,
            underlying,
            expiration,
            strike: None,
            style: None,
        }
    }

    /// The path of an option chain.
    #[must_use]
    pub(crate) const fn chain(underlying: &'a str, expiration: &'a ExpirationDate) -> Self {
        Self {
            level: NodeLevel::Chain,
            underlying,
            expiration,
            strike: None,
            style: None,
        }
    }

    /// The path of a strike book.
    #[must_use]
    pub(crate) const fn strike(
        underlying: &'a str,
        expiration: &'a ExpirationDate,
        strike: u64,
    ) -> Self {
        Self {
            level: NodeLevel::Strike,
            underlying,
            expiration,
            strike: Some(strike),
            style: None,
        }
    }

    /// The path of a call or put leaf.
    #[must_use]
    pub(crate) const fn leaf(
        underlying: &'a str,
        expiration: &'a ExpirationDate,
        strike: u64,
        style: OptionStyle,
    ) -> Self {
        Self {
            level: NodeLevel::Leaf,
            underlying,
            expiration,
            strike: Some(strike),
            style: Some(style),
        }
    }

    /// The version 1 encoding of this path (see the module docs).
    #[must_use]
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(PATH_PREFIX.len() + 48 + self.underlying.len());
        bytes.extend_from_slice(PATH_PREFIX);
        bytes.push(self.level.tag());
        push_varint(&mut bytes, self.underlying.len());
        bytes.extend_from_slice(self.underlying.as_bytes());
        match ExpirationKey::from(self.expiration) {
            ExpirationKey::Days(days) => {
                bytes.push(0x01);
                bytes.extend_from_slice(&days.to_dec().normalize().serialize());
            }
            ExpirationKey::DateTime(instant) => {
                bytes.push(0x02);
                bytes.extend_from_slice(&instant.timestamp().to_le_bytes());
                bytes.extend_from_slice(&instant.timestamp_subsec_nanos().to_le_bytes());
            }
        }
        if let Some(strike) = self.strike {
            bytes.extend_from_slice(&strike.to_le_bytes());
        }
        if let Some(style) = self.style {
            bytes.push(match style {
                OptionStyle::Call => 0,
                OptionStyle::Put => 1,
            });
        }
        bytes
    }
}

/// Appends `value` as an unsigned LEB128 varint. Infallible for any `usize`,
/// and the same bytes on every target width.
fn push_varint(bytes: &mut Vec<u8>, mut value: usize) {
    loop {
        // The low seven bits, as the least significant byte (no cast).
        let [low, ..] = (value & 0x7f).to_le_bytes();
        value >>= 7;
        if value == 0 {
            bytes.push(low);
            return;
        }
        bytes.push(low | 0x80);
    }
}

/// The id of the node at `path` under `root`: `UUIDv5(root, path)`.
#[must_use]
pub(crate) fn derived_node_id(root: Uuid, path: &NodePath<'_>) -> OrderId {
    OrderId::from_uuid(Uuid::new_v5(&root, &path.encode()))
}

/// A random node id, for a node built without a trade-ID root namespace.
#[must_use]
pub(crate) fn random_node_id() -> OrderId {
    #[expect(
        clippy::disallowed_methods,
        reason = "a node built without a root namespace keeps its random id"
    )]
    let uuid = Uuid::new_v4();
    OrderId::from_uuid(uuid)
}

/// The id of the node at `path`: derived from `root` when one is
/// configured, random otherwise.
#[must_use]
pub(crate) fn node_id(root: Option<Uuid>, path: &NodePath<'_>) -> OrderId {
    root.map_or_else(random_node_id, |root| derived_node_id(root, path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use optionstratlib::prelude::Positive;
    use rust_decimal::Decimal;
    use std::collections::BTreeSet;

    fn at(secs: i64, nanos: u32) -> ExpirationDate {
        ExpirationDate::DateTime(Utc.timestamp_opt(secs, nanos).single().expect("instant"))
    }

    fn days(value: Decimal) -> ExpirationDate {
        ExpirationDate::Days(Positive::new_decimal(value).expect("positive"))
    }

    const ROOT: Uuid = Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);

    #[test]
    fn test_node_id_same_root_and_path_same_id() {
        let exp = at(1_767_225_600, 0);
        let a = derived_node_id(
            ROOT,
            &NodePath::leaf("BTC", &exp, 50_000, OptionStyle::Call),
        );
        let b = derived_node_id(
            ROOT,
            &NodePath::leaf("BTC", &exp, 50_000, OptionStyle::Call),
        );
        assert_eq!(a, b);
        let other_root = Uuid::from_u128(1);
        let c = derived_node_id(
            other_root,
            &NodePath::leaf("BTC", &exp, 50_000, OptionStyle::Call),
        );
        assert_ne!(a, c, "distinct roots separate identities");
    }

    #[test]
    fn test_node_id_distinct_paths_distinct_encodings_and_ids() {
        let midnight = at(1_767_225_600, 0);
        let same_day_later = at(1_767_225_600 + 8 * 3600, 0);
        let one_nano_later = at(1_767_225_600, 1);
        let d1 = days(Decimal::new(10, 1));
        let paths = [
            NodePath::expiration("BTC", &midnight),
            NodePath::chain("BTC", &midnight),
            NodePath::strike("BTC", &midnight, 50_000),
            NodePath::leaf("BTC", &midnight, 50_000, OptionStyle::Call),
            NodePath::leaf("BTC", &midnight, 50_000, OptionStyle::Put),
            NodePath::leaf("BTC", &same_day_later, 50_000, OptionStyle::Call),
            NodePath::leaf("BTC", &one_nano_later, 50_000, OptionStyle::Call),
            NodePath::leaf("BTC", &midnight, 50_001, OptionStyle::Call),
            NodePath::leaf("ETH", &midnight, 50_000, OptionStyle::Call),
            NodePath::leaf("BTC", &d1, 50_000, OptionStyle::Call),
            // Separator and prefix cases: the length prefix keeps them apart.
            NodePath::strike("A", &midnight, 1),
            NodePath::strike("A-", &midnight, 1),
            NodePath::strike("A-B", &midnight, 1),
            NodePath::strike("", &midnight, 1),
        ];
        let encodings: BTreeSet<Vec<u8>> = paths.iter().map(NodePath::encode).collect();
        assert_eq!(encodings.len(), paths.len(), "every path encodes uniquely");
        let ids: BTreeSet<_> = paths
            .iter()
            .map(|path| derived_node_id(ROOT, path).to_string())
            .collect();
        assert_eq!(ids.len(), paths.len(), "every path gets its own id");
    }

    #[test]
    fn test_node_id_equal_day_counts_with_different_scale_same_id() {
        let one = days(Decimal::new(10, 1));
        let one_hundredths = days(Decimal::new(100, 2));
        assert_eq!(
            NodePath::chain("BTC", &one).encode(),
            NodePath::chain("BTC", &one_hundredths).encode()
        );
    }

    #[test]
    fn test_node_id_days_encode_normalized_decimal_bytes() {
        let bytes = NodePath::expiration("X", &days(Decimal::new(100, 2))).encode();
        let mut expected = b"ocob-node/1".to_vec();
        expected.extend_from_slice(&[1, 1, b'X', 0x01]);
        expected.extend_from_slice(&Decimal::ONE.serialize());
        assert_eq!(bytes, expected, "1.00 days encodes as the scale 0 one");
        let half = NodePath::expiration("X", &days(Decimal::new(5, 1))).encode();
        assert_ne!(half, bytes);
    }

    #[test]
    fn test_node_id_varint_round_lengths_encode_exactly() {
        for (len, expected) in [
            (0_usize, vec![0x00]),
            (1, vec![0x01]),
            (127, vec![0x7f]),
            (128, vec![0x80, 0x01]),
            (300, vec![0xac, 0x02]),
        ] {
            let mut bytes = Vec::new();
            push_varint(&mut bytes, len);
            assert_eq!(bytes, expected, "len {len}");
        }
    }

    #[test]
    fn test_node_id_golden_encoding_is_stable() {
        let exp = at(1, 2);
        let bytes = NodePath::leaf("X", &exp, 3, OptionStyle::Put).encode();
        let mut expected = b"ocob-node/1".to_vec();
        expected.extend_from_slice(&[4, 1, b'X', 0x02]);
        expected.extend_from_slice(&1_i64.to_le_bytes());
        expected.extend_from_slice(&2_u32.to_le_bytes());
        expected.extend_from_slice(&3_u64.to_le_bytes());
        expected.push(1);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn test_node_id_without_root_is_random() {
        let exp = at(1, 0);
        let path = NodePath::expiration("BTC", &exp);
        assert_ne!(node_id(None, &path), node_id(None, &path));
        assert_eq!(node_id(Some(ROOT), &path), node_id(Some(ROOT), &path));
    }
}
