//! Property tests for FunnelState transitions.
//!
//! Encodes the legal-transition graph as data and proptests against
//! the `can_transition_to` implementation.

use crate::model::FunnelState;
use proptest::prelude::*;

fn arb_state() -> impl Strategy<Value = FunnelState> {
    prop_oneof![
        Just(FunnelState::New),
        Just(FunnelState::Qualified),
        Just(FunnelState::Contacted),
        Just(FunnelState::Engaged),
        Just(FunnelState::Meeting),
        Just(FunnelState::Proposal),
        Just(FunnelState::Won),
        Just(FunnelState::Lost),
        Just(FunnelState::Suppressed),
    ]
}

const TERMINALS: [FunnelState; 3] = [FunnelState::Won, FunnelState::Lost, FunnelState::Suppressed];
const FORWARD_CHAIN: [FunnelState; 7] = [
    FunnelState::New,
    FunnelState::Qualified,
    FunnelState::Contacted,
    FunnelState::Engaged,
    FunnelState::Meeting,
    FunnelState::Proposal,
    FunnelState::Won,
];

fn position_in_chain(s: FunnelState) -> Option<usize> {
    FORWARD_CHAIN.iter().position(|x| *x == s)
}

proptest! {
    #[test]
    fn self_transition_always_allowed(s in arb_state()) {
        prop_assert!(s.can_transition_to(s));
    }

    #[test]
    fn terminal_states_never_transition_out(t in arb_state(), s in arb_state()) {
        if TERMINALS.contains(&t) && t != s {
            prop_assert!(!t.can_transition_to(s));
        }
    }

    #[test]
    fn suppressed_reachable_from_any_non_terminal(s in arb_state()) {
        if !TERMINALS.contains(&s) {
            prop_assert!(s.can_transition_to(FunnelState::Suppressed));
        }
    }

    #[test]
    fn lost_reachable_from_any_non_terminal(s in arb_state()) {
        if !TERMINALS.contains(&s) {
            prop_assert!(s.can_transition_to(FunnelState::Lost));
        }
    }

    #[test]
    fn forward_chain_allowed(from in arb_state(), to in arb_state()) {
        if let (Some(i), Some(j)) = (position_in_chain(from), position_in_chain(to))
            && !TERMINALS.contains(&from)
        {
            let allowed = from.can_transition_to(to);
            prop_assert_eq!(allowed, j >= i);
        }
    }

    #[test]
    fn backward_chain_disallowed(from in arb_state(), to in arb_state()) {
        if let (Some(i), Some(j)) = (position_in_chain(from), position_in_chain(to))
            && !TERMINALS.contains(&from)
            && j < i
            && to != FunnelState::Lost
            && to != FunnelState::Suppressed
        {
            prop_assert!(!from.can_transition_to(to));
        }
    }
}
