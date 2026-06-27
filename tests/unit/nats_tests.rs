//! Cross-module integration tests for the NATS subject scheme (feature
//! `nats`).
//!
//! These tests exercise the subject *construction and parse* end-to-end — no
//! live broker is involved. The headline check is a round-trip: build a
//! hierarchical NATS subject from a contract's coordinates via
//! [`OptionChainSubjectBuilder`], then parse the subject tokens back into a
//! contract symbol and rebuild the subject, asserting it is identical. This
//! guards the documented, collision-free
//! `{prefix}.{kind}.{underlying}.{expiry}.{strike}.{type}` scheme against drift.

use option_chain_orderbook::SymbolParser;
use option_chain_orderbook::orderbook::nats::OptionChainSubjectBuilder;
use optionstratlib::OptionStyle;

const PREFIX: &str = "optionchain";

/// Parses one of this crate's hierarchical subjects back into its tokens.
///
/// Expected shape: `{prefix}.{kind}.{underlying}.{expiry}.{strike}.{type}`,
/// where `kind` is `trades` or `book`. Returns
/// `(kind, underlying, expiry, strike, option_type)`.
fn parse_subject(subject: &str) -> (String, String, String, String, String) {
    let tokens: Vec<&str> = subject.split('.').collect();
    assert_eq!(
        tokens.len(),
        6,
        "subject must have exactly 6 tokens: {subject}"
    );
    assert_eq!(tokens[0], PREFIX, "prefix token mismatch");
    (
        tokens[1].to_string(),
        tokens[2].to_string(),
        tokens[3].to_string(),
        tokens[4].to_string(),
        tokens[5].to_string(),
    )
}

#[test]
fn test_subject_round_trips_to_canonical_form_and_back() {
    // Drive both a call and a put through the full build -> parse -> rebuild
    // loop. The reconstructed contract symbol must re-parse to the same
    // coordinates and rebuild byte-identical subjects.
    for (symbol, expected_type) in [("BTC-20240329-50000-C", "C"), ("ETH-20240628-3000-P", "P")] {
        let parsed = SymbolParser::parse(symbol).expect("symbol must parse");
        let builder = OptionChainSubjectBuilder::from_symbol(symbol).expect("builder from symbol");

        // The builder's components match the canonical symbol parse.
        assert_eq!(builder.underlying(), parsed.underlying());
        assert_eq!(builder.expiry(), parsed.expiration_str());
        assert_eq!(builder.strike(), parsed.strike().to_string());
        assert_eq!(builder.option_type(), expected_type);

        let trade_subject = builder.trade_subject(PREFIX);
        let book_subject = builder.book_subject(PREFIX);

        // Canonical, documented form.
        assert_eq!(
            trade_subject,
            format!(
                "{PREFIX}.trades.{}.{}.{}.{}",
                parsed.underlying(),
                parsed.expiration_str(),
                parsed.strike(),
                expected_type
            )
        );
        assert_eq!(
            book_subject,
            format!(
                "{PREFIX}.book.{}.{}.{}.{}",
                parsed.underlying(),
                parsed.expiration_str(),
                parsed.strike(),
                expected_type
            )
        );

        // ── Round-trip: parse the subject back to a contract symbol ──
        let (trade_kind, t_underlying, t_expiry, t_strike, t_type) = parse_subject(&trade_subject);
        assert_eq!(trade_kind, "trades");
        let (book_kind, b_underlying, b_expiry, b_strike, b_type) = parse_subject(&book_subject);
        assert_eq!(book_kind, "book");

        // Trade and book subjects encode the same coordinates.
        assert_eq!(
            (&t_underlying, &t_expiry, &t_strike, &t_type),
            (&b_underlying, &b_expiry, &b_strike, &b_type)
        );

        let reconstructed = format!("{t_underlying}-{t_expiry}-{t_strike}-{t_type}");
        assert_eq!(
            reconstructed, symbol,
            "round-trip must yield the original symbol"
        );

        // Rebuilding a builder from the reconstructed symbol yields identical
        // subjects — the loop closes.
        let rebuilt = OptionChainSubjectBuilder::from_symbol(&reconstructed)
            .expect("reconstructed symbol must rebuild");
        assert_eq!(rebuilt.trade_subject(PREFIX), trade_subject);
        assert_eq!(rebuilt.book_subject(PREFIX), book_subject);
        assert_eq!(rebuilt.option_style(), parsed.option_style());
    }
}

#[test]
fn test_subject_builder_try_new_round_trips_explicit_components() {
    // The explicit-component constructor must produce the same canonical subject
    // and round-trip identically to the from-symbol path.
    let builder = OptionChainSubjectBuilder::try_new("BTC", "20240329", "50000", "call")
        .expect("valid components");

    let trade_subject = builder.trade_subject(PREFIX);
    let (kind, underlying, expiry, strike, option_type) = parse_subject(&trade_subject);

    assert_eq!(kind, "trades");
    assert_eq!(underlying, "BTC");
    assert_eq!(expiry, "20240329");
    assert_eq!(strike, "50000");
    assert_eq!(option_type, "C");
    assert_eq!(builder.option_style(), OptionStyle::Call);

    // The from-symbol path over the reconstructed symbol agrees.
    let reconstructed = format!("{underlying}-{expiry}-{strike}-{option_type}");
    let from_symbol =
        OptionChainSubjectBuilder::from_symbol(&reconstructed).expect("reconstructed parses");
    assert_eq!(from_symbol.trade_subject(PREFIX), trade_subject);
    assert_eq!(
        from_symbol.book_subject(PREFIX),
        builder.book_subject(PREFIX)
    );
}

#[test]
fn test_subject_tokens_are_collision_free_across_kinds() {
    // The `trades` vs `book` kind token keeps the two streams in disjoint
    // subject spaces for the same contract — a subscriber on one never receives
    // the other.
    let builder = OptionChainSubjectBuilder::from_symbol("BTC-20240329-50000-C").expect("builder");
    let (trade, book) = builder.subjects(PREFIX);
    assert_ne!(trade, book);
    assert!(trade.contains(".trades."));
    assert!(book.contains(".book."));
}
