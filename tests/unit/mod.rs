//! Unit tests for option-chain-orderbook library.

// Examples, benchmarks and tests build orders with random ids and books
// with the default constructors; the bans in clippy.toml cover the library.
#![allow(clippy::disallowed_methods)]

mod deterministic_construction_tests;
#[cfg(feature = "clock")]
mod expiration_formatting_tests;
mod layering_tests;
mod orderbook_tests;

// Shared `proptest` strategies/fixtures and the property-based invariant suites.
// All driven through the public API only.
mod common;
mod props_best_quote;
mod props_chain_stats_aggregate;
mod props_get_or_create_idempotent;
mod props_greeks_aggregation;
mod props_mass_cancel_scope;
mod props_symbol_roundtrip;

// Feature-gated cross-module test modules. They compile and run only under
// `cargo test --all-features` (or the matching single feature) and are excluded
// from the default build, so the default test matrix stays feature-free.
#[cfg(feature = "sequencer")]
mod sequencer_tests;

#[cfg(feature = "nats")]
mod nats_tests;
