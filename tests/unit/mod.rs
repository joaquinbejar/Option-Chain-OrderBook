//! Unit tests for option-chain-orderbook library.

mod layering_tests;
mod orderbook_tests;

// Feature-gated cross-module test modules. They compile and run only under
// `cargo test --all-features` (or the matching single feature) and are excluded
// from the default build, so the default test matrix stays feature-free.
#[cfg(feature = "sequencer")]
mod sequencer_tests;

#[cfg(feature = "nats")]
mod nats_tests;
