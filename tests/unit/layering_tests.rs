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
//!
//! The guarded file set is enumerated **dynamically** from `src/orderbook/`
//! (minus an explicit exclude list), so a newly added core / shared module is
//! covered automatically — there is no hand-maintained allowlist to forget to
//! update. The check rejects both pricing *module* names and the distinctive
//! pricing *type* names, so a core file cannot smuggle in a pricing type via the
//! crate-root flat re-export (`use crate::orderbook::MarkPriceCalculator;`)
//! without naming the module.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Files under `src/orderbook/` that are deliberately EXCLUDED from the guard,
/// in three buckets:
///
/// 1. The pricing subsystem itself (`greeks_engine`, `greeks_aggregator`,
///    `mark_price`, `index_price_feed`) — it legitimately defines and wires
///    these symbols, so of course it references them.
/// 2. `mod.rs`, the re-export hub, which names every module including pricing.
/// 3. The top-of-stack eventing / expiry-lifecycle layer (`sequencer`, `nats`,
///    `expiry_cycle`, `expiry_lifecycle`, `expiry_scheduler`). That layer sits
///    *above* the hierarchy and is a separate concern from the core hierarchy +
///    shared services this guard protects (the scope of issue #89).
///
/// Everything else in `src/orderbook/*.rs`, plus `src/utils.rs`, is guarded.
const EXCLUDED_FILES: &[&str] = &[
    // pricing subsystem
    "greeks_engine.rs",
    "greeks_aggregator.rs",
    "mark_price.rs",
    "index_price_feed.rs",
    // re-export hub
    "mod.rs",
    // top-of-stack eventing / expiry-lifecycle layer (separate layer)
    "sequencer.rs",
    "nats.rs",
    "expiry_cycle.rs",
    "expiry_lifecycle.rs",
    "expiry_scheduler.rs",
];

/// Pricing-subsystem module names that a guarded file must never import.
const PRICING_MODULES: &[&str] = &[
    "greeks_engine",
    "greeks_aggregator",
    "mark_price",
    "index_price_feed",
];

/// Distinctive pricing-subsystem *type* names that a guarded file must never
/// reference. Checking these closes the hole where a core file could pull a
/// pricing type via the crate-root flat re-export (e.g.
/// `use crate::orderbook::MarkPriceCalculator;`) without naming the pricing
/// module. Each name is specific enough not to collide with a legitimate
/// core-hierarchy identifier. (`VolSurface` also matches `FlatVolSurface`;
/// `MarkPriceConfig` also matches `MarkPriceConfigBuilder`.)
const PRICING_TYPES: &[&str] = &[
    "GreeksEngine",
    "GreeksAggregator",
    "AggregatedGreeks",
    "GreeksRecalcTrigger",
    "VolSurface",
    "MarkPriceCalculator",
    "MarkPriceConfig",
];

/// Eventing-layer module-import paths a guarded core / shared file must never
/// use. The hierarchy receives any NATS wiring as an `orderbook_rs`-native
/// listener factory threaded through the leaf (`book.rs`), never by importing
/// the crate's `nats` module (issue #102). These needles are *path-scoped*
/// (`module::`), so `book.rs` — which legitimately defines the `Nats*` listener
/// types and the factory alias but does not import the `nats` module — is not
/// tripped, while a stray `use super::nats::…` in `strike.rs` / `chain.rs` /
/// `expiration.rs` / `underlying.rs` is.
///
/// The needles match as **substrings**, so `orderbook::nats` also rejects every
/// fully-qualified form (`crate::orderbook::nats::…`, `self::orderbook::nats::…`,
/// `use crate :: orderbook::nats`), and `super::nats` covers the relative form —
/// there is no qualified import style that bypasses the guard.
const FORBIDDEN_MODULE_IMPORTS: &[&str] = &["super::nats", "orderbook::nats"];

/// Strips full-line comments (doc comments and `//` comments) from source so the
/// structural check only inspects actual code. Documentation legitimately
/// references the pricing modules by name (e.g. to explain the layering), so it
/// must be excluded from the import check.
///
/// This fails *safe*: it does not strip block comments (`/* … */`) or trailing
/// inline comments, so a pricing name in one would trip the assert as a false
/// positive — it can only make the guard stricter, never let an inversion slip
/// through.
fn code_only(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Dynamically enumerate the guarded files: every `src/orderbook/*.rs` not in
/// [`EXCLUDED_FILES`], plus `src/utils.rs`.
fn guarded_files(manifest_dir: &Path) -> Vec<PathBuf> {
    let orderbook_dir = manifest_dir.join("src/orderbook");
    let entries = std::fs::read_dir(&orderbook_dir)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", orderbook_dir.display()));

    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let path = entry.expect("read dir entry").path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.ends_with(".rs") && !EXCLUDED_FILES.contains(&name) {
            files.push(path);
        }
    }
    files.push(manifest_dir.join("src/utils.rs"));
    files.sort();
    files
}

#[test]
fn core_hierarchy_does_not_import_pricing_subsystem() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let files = guarded_files(&manifest_dir);

    // Sanity: the enumeration must actually find the core files. Guards against a
    // globbing mistake silently checking nothing (a passing-but-vacuous test).
    assert!(
        files.len() >= 15,
        "layering guard enumerated only {} files — expected the full core \
         hierarchy + shared services; the directory glob likely broke",
        files.len(),
    );

    // The dynamic enumeration must cover the known core / shared files —
    // including `expiration_key.rs`, which a hand-maintained allowlist had
    // previously omitted.
    let names: BTreeSet<&str> = files
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
        .collect();
    for must in [
        "book.rs",
        "quote.rs",
        "strike.rs",
        "chain.rs",
        "expiration.rs",
        "expiration_key.rs",
        "underlying.rs",
        "index_feed.rs",
        "symbol_index.rs",
    ] {
        assert!(
            names.contains(must),
            "core / shared file `{must}` is missing from the layering guard set",
        );
    }

    for path in &files {
        let rel = path.strip_prefix(&manifest_dir).unwrap_or(path);
        let rel = rel.display();
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        let code = code_only(&source);

        for needle in PRICING_MODULES.iter().chain(PRICING_TYPES) {
            assert!(
                !code.contains(needle),
                "layering inversion: core-hierarchy / shared-service file `{rel}` \
                 references pricing symbol `{needle}`. The pricing subsystem reads \
                 the hierarchy, not the reverse (see issue #89).",
            );
        }

        for needle in FORBIDDEN_MODULE_IMPORTS {
            assert!(
                !code.contains(needle),
                "layering inversion: core-hierarchy / shared-service file `{rel}` \
                 imports the eventing `nats` module via `{needle}`. NATS wiring must \
                 reach the hierarchy as an orderbook_rs-native listener factory \
                 threaded through `book.rs`, never by importing the `nats` module \
                 (see issue #102).",
            );
        }
    }
}
