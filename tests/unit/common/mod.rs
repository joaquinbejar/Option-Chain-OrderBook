//! Shared test support for the property-based (`proptest`) suites.
//!
//! Only the strategy/fixture helpers live here; every invariant assertion lives
//! in its own `props_<invariant>` sibling module. Driven exclusively through the
//! crate's public API.

pub mod strategies;
