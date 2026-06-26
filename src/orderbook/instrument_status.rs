//! Instrument status module.
//!
//! This module provides the [`InstrumentStatus`] enum for tracking the lifecycle
//! state of an option instrument. Only instruments with [`InstrumentStatus::Active`]
//! status accept new orders.

use serde::{Deserialize, Serialize};

/// Lifecycle status of an option instrument.
///
/// Tracks the current state of an instrument in the trading system.
/// Only [`Active`](InstrumentStatus::Active) instruments accept new orders.
///
/// ## State Transitions
///
/// The lifecycle is enforced: only the edges below are legal, checked by
/// [`can_transition`](InstrumentStatus::can_transition). Any other transition is
/// rejected with
/// [`Error::IllegalStatusTransition`](crate::error::Error::IllegalStatusTransition).
/// A self-transition (`X -> X`) is always a legal no-op so repeated lifecycle
/// ticks stay idempotent.
///
/// ```text
///                  ┌──── resume ────┐
///                  ▼                │
/// Pending ──────► Active ───────► Halted
///   │  │            │  │            │
///   │  │ halt/      │  │ settle     │ settle
///   │  └────────────┘  ▼            ▼
///   │                Settling ──► Expired (terminal)
///   └───────────────────────────► Expired
/// ```
///
/// Legal edges:
/// - `Pending -> Active` (activation)
/// - `Pending -> Settling`, `Pending -> Expired` (lifecycle catch-up / expiry)
/// - `Active -> Halted` (halt), `Active -> Settling`, `Active -> Expired`
/// - `Halted -> Active` (resume), `Halted -> Settling`, `Halted -> Expired`
/// - `Settling -> Expired`
/// - `X -> X` (idempotent no-op)
///
/// [`Expired`](InstrumentStatus::Expired) is terminal: it has no outgoing edges
/// other than the `Expired -> Expired` no-op. The transitions rejected by design
/// include every `Expired -> *` move, `Settling -> Halted`, `Settling -> Active`,
/// and any move back to `Pending`.
///
/// The forward edges (everything except `Halted -> Active`) advance the
/// discriminant monotonically, which keeps them in lock-step with the expiry
/// lifecycle's CAS-forward setter and the `min`-based chain-status read.
///
/// ## Thread Safety
///
/// This enum is stored as an [`AtomicU8`](std::sync::atomic::AtomicU8) inside
/// [`OptionOrderBook`](super::book::OptionOrderBook) for lock-free concurrent access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum InstrumentStatus {
    /// Instrument is pending activation (not yet trading).
    Pending = 0,
    /// Instrument is active and accepting orders.
    Active = 1,
    /// Instrument is temporarily halted (no new orders accepted).
    Halted = 2,
    /// Instrument is in settlement process (no new orders accepted).
    Settling = 3,
    /// Instrument has expired (no new orders accepted, all orders cancelled).
    Expired = 4,
}

impl InstrumentStatus {
    /// Converts a `u8` value to an `InstrumentStatus`.
    ///
    /// Returns `None` if the value does not correspond to a valid status.
    ///
    /// # Arguments
    ///
    /// * `value` - The raw `u8` value to convert
    #[must_use]
    #[inline]
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Pending),
            1 => Some(Self::Active),
            2 => Some(Self::Halted),
            3 => Some(Self::Settling),
            4 => Some(Self::Expired),
            _ => None,
        }
    }

    /// Returns `true` if this status allows new orders to be placed.
    ///
    /// Only [`Active`](InstrumentStatus::Active) instruments accept orders.
    #[must_use]
    #[inline]
    pub const fn is_accepting_orders(&self) -> bool {
        matches!(self, Self::Active)
    }

    /// Returns `true` if transitioning from `self` to `to` is a legal edge in
    /// the enforced lifecycle state machine.
    ///
    /// A self-transition (`X -> X`) is always permitted as an idempotent no-op,
    /// so repeated lifecycle ticks remain idempotent.
    /// [`Expired`](Self::Expired) is terminal and has no outgoing edges other
    /// than the `Expired -> Expired` no-op.
    ///
    /// The full set of legal edges is documented on the [`InstrumentStatus`]
    /// type. This is the single source of truth that
    /// [`OptionOrderBook::set_status`](super::book::OptionOrderBook::set_status),
    /// `halt`, `resume`, `expire`, and `compare_and_set_status` validate
    /// against. Every transition the expiry lifecycle performs through its
    /// CAS-forward setter — `{Pending, Active, Halted} -> Settling` and
    /// `{Pending, Active, Halted, Settling} -> Expired` — is included here, so
    /// enforcing the state machine never blocks the lifecycle.
    ///
    /// # Arguments
    ///
    /// * `to` - The target status to transition to
    #[must_use]
    #[inline]
    pub const fn can_transition(self, to: Self) -> bool {
        // Idempotent self-transition is always a legal no-op.
        if self as u8 == to as u8 {
            return true;
        }
        matches!(
            (self, to),
            // Activation / resume.
            (Self::Pending, Self::Active) | (Self::Halted, Self::Active)
            // Halt of a live instrument.
            | (Self::Active, Self::Halted)
            // Settlement entry (lifecycle forward-advance).
            | (Self::Pending, Self::Settling)
            | (Self::Active, Self::Settling)
            | (Self::Halted, Self::Settling)
            // Expiry (terminal sweep, reachable from any live state).
            | (Self::Pending, Self::Expired)
            | (Self::Active, Self::Expired)
            | (Self::Halted, Self::Expired)
            | (Self::Settling, Self::Expired)
        )
    }
}

impl std::fmt::Display for InstrumentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::Active => write!(f, "Active"),
            Self::Halted => write!(f, "Halted"),
            Self::Settling => write!(f, "Settling"),
            Self::Expired => write!(f, "Expired"),
        }
    }
}

impl Default for InstrumentStatus {
    /// Default status is [`Active`](InstrumentStatus::Active).
    ///
    /// Newly created order books are immediately ready to accept orders.
    fn default() -> Self {
        Self::Active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instrument_status_display() {
        assert_eq!(InstrumentStatus::Pending.to_string(), "Pending");
        assert_eq!(InstrumentStatus::Active.to_string(), "Active");
        assert_eq!(InstrumentStatus::Halted.to_string(), "Halted");
        assert_eq!(InstrumentStatus::Settling.to_string(), "Settling");
        assert_eq!(InstrumentStatus::Expired.to_string(), "Expired");
    }

    #[test]
    fn test_instrument_status_from_u8() {
        assert_eq!(
            InstrumentStatus::from_u8(0),
            Some(InstrumentStatus::Pending)
        );
        assert_eq!(InstrumentStatus::from_u8(1), Some(InstrumentStatus::Active));
        assert_eq!(InstrumentStatus::from_u8(2), Some(InstrumentStatus::Halted));
        assert_eq!(
            InstrumentStatus::from_u8(3),
            Some(InstrumentStatus::Settling)
        );
        assert_eq!(
            InstrumentStatus::from_u8(4),
            Some(InstrumentStatus::Expired)
        );
        assert_eq!(InstrumentStatus::from_u8(5), None);
        assert_eq!(InstrumentStatus::from_u8(255), None);
    }

    #[test]
    fn test_instrument_status_from_u8_roundtrip() {
        for &status in &[
            InstrumentStatus::Pending,
            InstrumentStatus::Active,
            InstrumentStatus::Halted,
            InstrumentStatus::Settling,
            InstrumentStatus::Expired,
        ] {
            let raw = status as u8;
            assert_eq!(InstrumentStatus::from_u8(raw), Some(status));
        }
    }

    #[test]
    fn test_instrument_status_is_accepting_orders() {
        assert!(!InstrumentStatus::Pending.is_accepting_orders());
        assert!(InstrumentStatus::Active.is_accepting_orders());
        assert!(!InstrumentStatus::Halted.is_accepting_orders());
        assert!(!InstrumentStatus::Settling.is_accepting_orders());
        assert!(!InstrumentStatus::Expired.is_accepting_orders());
    }

    #[test]
    fn test_instrument_status_default() {
        assert_eq!(InstrumentStatus::default(), InstrumentStatus::Active);
    }

    #[test]
    fn test_instrument_status_clone_copy() {
        let status = InstrumentStatus::Active;
        let cloned = status;
        assert_eq!(status, cloned);
    }

    #[test]
    fn test_instrument_status_eq() {
        assert_eq!(InstrumentStatus::Active, InstrumentStatus::Active);
        assert_ne!(InstrumentStatus::Active, InstrumentStatus::Halted);
    }

    #[test]
    fn test_instrument_status_debug() {
        let debug = format!("{:?}", InstrumentStatus::Settling);
        assert_eq!(debug, "Settling");
    }

    #[test]
    fn test_instrument_status_serde_roundtrip() {
        for &status in &[
            InstrumentStatus::Pending,
            InstrumentStatus::Active,
            InstrumentStatus::Halted,
            InstrumentStatus::Settling,
            InstrumentStatus::Expired,
        ] {
            let json = match serde_json::to_string(&status) {
                Ok(j) => j,
                Err(err) => panic!("serialization failed: {}", err),
            };
            let deserialized: InstrumentStatus = match serde_json::from_str(&json) {
                Ok(d) => d,
                Err(err) => panic!("deserialization failed: {}", err),
            };
            assert_eq!(status, deserialized);
        }
    }

    #[test]
    fn test_instrument_status_hash() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(InstrumentStatus::Active);
        set.insert(InstrumentStatus::Halted);
        set.insert(InstrumentStatus::Active); // duplicate

        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_instrument_status_repr_values() {
        assert_eq!(InstrumentStatus::Pending as u8, 0);
        assert_eq!(InstrumentStatus::Active as u8, 1);
        assert_eq!(InstrumentStatus::Halted as u8, 2);
        assert_eq!(InstrumentStatus::Settling as u8, 3);
        assert_eq!(InstrumentStatus::Expired as u8, 4);
    }

    #[test]
    fn test_can_transition_self_is_legal_noop() {
        for &status in &[
            InstrumentStatus::Pending,
            InstrumentStatus::Active,
            InstrumentStatus::Halted,
            InstrumentStatus::Settling,
            InstrumentStatus::Expired,
        ] {
            assert!(
                status.can_transition(status),
                "self-transition {status} -> {status} must be a legal no-op",
            );
        }
    }

    #[test]
    fn test_can_transition_legal_edges_allowed() {
        use InstrumentStatus::*;
        let legal = [
            (Pending, Active),
            (Pending, Settling),
            (Pending, Expired),
            (Active, Halted),
            (Active, Settling),
            (Active, Expired),
            (Halted, Active),
            (Halted, Settling),
            (Halted, Expired),
            (Settling, Expired),
        ];
        for (from, to) in legal {
            assert!(from.can_transition(to), "{from} -> {to} must be legal",);
        }
    }

    #[test]
    fn test_can_transition_expired_is_terminal() {
        use InstrumentStatus::*;
        for to in [Pending, Active, Halted, Settling] {
            assert!(
                !Expired.can_transition(to),
                "Expired -> {to} must be rejected (terminal state)",
            );
        }
    }

    #[test]
    fn test_can_transition_settling_backward_rejected() {
        use InstrumentStatus::*;
        assert!(!Settling.can_transition(Halted));
        assert!(!Settling.can_transition(Active));
        assert!(!Settling.can_transition(Pending));
    }

    #[test]
    fn test_can_transition_back_to_pending_rejected() {
        use InstrumentStatus::*;
        for from in [Active, Halted, Settling, Expired] {
            assert!(
                !from.can_transition(Pending),
                "{from} -> Pending must be rejected",
            );
        }
    }

    #[test]
    fn test_can_transition_lifecycle_forward_edges_all_legal() {
        // Every per-book edge the expiry lifecycle's CAS-forward setter can
        // request must be legal so enforcing the state machine never blocks it.
        use InstrumentStatus::*;
        for from in [Pending, Active, Halted] {
            assert!(from.can_transition(Settling), "{from} -> Settling");
        }
        for from in [Pending, Active, Halted, Settling] {
            assert!(from.can_transition(Expired), "{from} -> Expired");
        }
    }
}
