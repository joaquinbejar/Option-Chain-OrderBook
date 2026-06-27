//! Property: a valid symbol round-trips through the parser.
//!
//! Invariant asserted: for a canonically-formatted, valid symbol `s`,
//! `SymbolParser::parse(s)?.to_symbol() == s`, the parsed components echo the
//! generated inputs, the derived expiration is an absolute
//! [`ExpirationDate::DateTime`] (never the wall-clock-relative `Days`), and
//! re-parsing the reconstructed string yields an equal `ParsedSymbol`.
//!
//! The grammar `{UNDERLYING}-{YYYYMMDD}-{STRIKE}-{C|P}` is generated in
//! canonical form (uppercase underlying with no `-`, zero-padded 8-digit date,
//! decimal strike with no leading zeros, uppercase option letter) so the exact
//! round-trip is the genuine invariant rather than an artifact of formatting.
//! Days are capped at 28 so every `(year, month, day)` is a real calendar date.

use option_chain_orderbook::SymbolParser;
use optionstratlib::{ExpirationDate, OptionStyle};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 50_000,
        ..ProptestConfig::default()
    })]

    #[test]
    fn parse_then_to_symbol_is_identity(
        underlying in "[A-Z]{1,4}",
        year in 2000u32..=2099,
        month in 1u32..=12,
        day in 1u32..=28,
        strike in 1u64..=10_000_000,
        is_call in any::<bool>(),
    ) {
        let opt = if is_call { "C" } else { "P" };
        let date = format!("{year:04}{month:02}{day:02}");
        let symbol = format!("{underlying}-{date}-{strike}-{opt}");

        let parsed = SymbolParser::parse(&symbol).expect("canonical symbol must parse");

        // The headline round-trip.
        prop_assert_eq!(parsed.to_symbol(), symbol.clone());

        // Parsed components echo the generated inputs.
        prop_assert_eq!(parsed.underlying(), underlying.as_str());
        prop_assert_eq!(parsed.expiration_str(), date.as_str());
        prop_assert_eq!(parsed.strike(), strike);
        prop_assert_eq!(
            parsed.option_style(),
            if is_call { OptionStyle::Call } else { OptionStyle::Put }
        );

        // Expiry is absolute (replay-stable), never the relative `Days` variant.
        prop_assert!(matches!(parsed.expiration(), ExpirationDate::DateTime(_)));

        // Reconstruct → re-parse is stable (the type invariant).
        let reparsed = SymbolParser::parse(&parsed.to_symbol()).expect("reconstructed must re-parse");
        prop_assert_eq!(parsed, reparsed);
    }
}
