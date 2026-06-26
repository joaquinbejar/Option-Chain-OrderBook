//! Structural layering tests.
//!
//! These tests guard the one-way layering rule: the core order-book hierarchy
//! and the shared lookup/state services must NOT depend on the Greeks /
//! mark-price pricing subsystem (`greeks_engine`, `greeks_aggregator`,
//! `mark_price`, `index_price_feed`). Pricing reads the hierarchy, never the
//! reverse.
//!
//! See issue #89: the `IndexPriceFeed` trait was relocated to the neutral
//! `index_feed` module so `UnderlyingOrderBook` (top of the core hierarchy) can
//! hold an `Arc<dyn IndexPriceFeed>` without importing the pricing subsystem.

use std::path::PathBuf;

/// Core-hierarchy and shared-service modules that must stay independent of the
/// pricing subsystem. `index_feed` is the neutral trait module and is included
/// because it too must not pull in pricing.
const CORE_HIERARCHY_FILES: &[&str] = &[
    "src/orderbook/book.rs",
    "src/orderbook/quote.rs",
    "src/orderbook/strike.rs",
    "src/orderbook/chain.rs",
    "src/orderbook/expiration.rs",
    "src/orderbook/underlying.rs",
    "src/orderbook/fees.rs",
    "src/orderbook/stp.rs",
    "src/orderbook/validation.rs",
    "src/orderbook/contract_specs.rs",
    "src/orderbook/instrument_registry.rs",
    "src/orderbook/instrument_status.rs",
    "src/orderbook/symbol_index.rs",
    "src/orderbook/strike_generator.rs",
    "src/orderbook/strike_range.rs",
    "src/orderbook/index_feed.rs",
    "src/utils.rs",
];

/// Pricing-subsystem module names that the core hierarchy must never import.
const PRICING_MODULES: &[&str] = &[
    "greeks_engine",
    "greeks_aggregator",
    "mark_price",
    "index_price_feed",
];

/// Strips full-line comments (doc comments and `//` comments) from source so the
/// structural check only inspects actual code. Documentation legitimately
/// references the pricing modules by name (e.g. to explain the layering), so it
/// must be excluded from the import check.
///
/// Limitations (both fail *safe* — they can only make the guard stricter, never
/// let an inversion slip through): block comments (`/* … */`) and trailing inline
/// comments are not stripped, so a pricing-module name in one would trip the
/// assert as a false positive. The check also matches module *names* as
/// substrings, so it catches the `super::<module>::Type` convention used between
/// `src/orderbook/` siblings but not a pricing type pulled via the crate-root
/// flat re-export (`use crate::orderbook::MarkPriceCalculator;`). The latter is
/// not the idiom used inside the crate, so the substring check is sufficient.
fn code_only(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn core_hierarchy_does_not_import_pricing_subsystem() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    for rel in CORE_HIERARCHY_FILES {
        let path = manifest_dir.join(rel);
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) => panic!("failed to read {}: {err}", path.display()),
        };
        let code = code_only(&source);

        for module in PRICING_MODULES {
            assert!(
                !code.contains(module),
                "layering inversion: core-hierarchy file `{rel}` references pricing \
                 module `{module}`. The pricing subsystem reads the hierarchy, not \
                 the reverse (see issue #89).",
            );
        }
    }
}
