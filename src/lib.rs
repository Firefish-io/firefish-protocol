//! The raw Firefish smart contract.
//!
//! This crate contains the implementation of the smart contract with no communication, interaction
//! with the chain, etc. It only creates objects that need to passed around and transactions to
//! broadcast.
//!
//! [`Prefund::new`] is the entry point to the contract API. The contract is modeled as a type-level
//! state machine to prevent mistakes.

mod test_macros;
pub mod contract;

// Why is everything in `contract` and nothing here?
//
// Because contract contains quite low-level primitives and I wanted to create a higher layer
// which was meant to go here. However I later decided to do a separate crate instead. I did
// flatten it but in a different branch which contains many drastic changes that are not that
// well-tested.
//
// This is old code that will get replaced and flattened.
