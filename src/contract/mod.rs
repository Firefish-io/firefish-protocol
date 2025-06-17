//! # Contract
//!
//! This module contains the definition of the contract of the Firefish Core.
/// The contract module contains all the information about the contract
/// that is shared between the participants and Firefish verification service.

pub mod prefund;
pub mod escrow;
pub mod primitives;
pub mod participant;
pub mod pub_keys;
pub mod offer;
pub mod constants;
pub mod deserialize;

use secp256k1::Keypair;
use secp256k1::schnorr::Signature;

use participant::{Participant, Ted};

/// The identifier of a contract.
#[derive(Copy, Clone)]
pub struct Id(u64);

/// Marker types to distinguish contracts.
///
/// This is used to distinguish between the prefund contract and the escrow contract.
pub mod context {
    /// Marker for data structures used in the context of prefund contract.
    ///
    /// The prefund contract is used to fund the escrow contract.
    pub enum Prefund {}

    /// Marker for data structures used in the context of escrow contract.
    pub enum Escrow {}
}

/// The state of the Firefish contract.
pub struct ContractState<P: Participant> {
    /// The state of prefund.
    pub prefund: prefund::State<P>,

    /// The state of escrow.
    pub escrow: escrow::State<P>,
}

impl<P: Participant> ContractState<P> {
    pub fn new(offer: offer::Offer) -> Self where P::PrefundData: Default, P::PreEscrowData: Default {
        ContractState {
            prefund: prefund::State::new(offer.prefund_keys, offer.escrow.network),
            escrow: escrow::State::new(offer.escrow, offer.escrow_keys),
        }
    }
}

pub trait StateData {
    const STATE_ID: constants::StateId;
    const PARTICIPANT_ID: constants::ParticipantId;
}

pub trait Serialize {
    fn serialize(&self, out: &mut Vec<u8>);
    fn serialize_with_header(&self, out: &mut Vec<u8>) where Self: StateData {
        deserialize::StateVersion::CURRENT.serialize(out);
        self.serialize_with_header_unversioned(out);
    }

    /// This is used in sub-structs where state ID needs to be known but the version is the same,
    /// so storing it would be duplication. We would like to also avoid storing participant ID but
    /// sadly, that was forgotten in the initial version and changing it would break things.
    fn serialize_with_header_unversioned(&self, out: &mut Vec<u8>) where Self: StateData {
        out.push(Self::PARTICIPANT_ID as u8);
        out.push(Self::STATE_ID as u8);
        self.serialize(out);
    }
}

pub trait Deserialize: Sized {
    type Error: core::fmt::Debug;

    fn deserialize(bytes: &mut &[u8], version: deserialize::StateVersion) -> Result<Self, Self::Error>;
    fn deserialize_with_header(bytes: &mut &[u8]) -> Result<Self, StateDeserError<Self::Error>> where Self: StateData {
        let version = deserialize::StateVersion::deserialize(bytes)?;
        if bytes.len() < 2 {
            return Err(StateDeserError::UnexpectedEnd);
        }
        if bytes[0] != Self::PARTICIPANT_ID as u8 {
            return Err(StateDeserError::InvalidParticipant(bytes[0]));
        }
        if bytes[1] != Self::STATE_ID as u8 {
            return Err(StateDeserError::InvalidParticipant(bytes[0]));
        }
        *bytes = &bytes[2..];
        Self::deserialize(bytes, version).map_err(StateDeserError::InvalidData)
    }
}

#[derive(Debug)]
pub enum StateDeserError<E> {
    UnexpectedEnd,
    UnsupportedVersion(u32),
    InvalidState(u8),
    InvalidParticipant(u8),
    InvalidData(E),
}

impl<E> From<deserialize::StateVersionDeserError> for StateDeserError<E> {
    fn from(value: deserialize::StateVersionDeserError) -> Self {
        match value {
            deserialize::StateVersionDeserError::UnexpectedEnd => StateDeserError::UnexpectedEnd,
            deserialize::StateVersionDeserError::UnsupportedVersion(version) => StateDeserError::UnsupportedVersion(version),
        }
    }
}

pub trait HotKey {
    fn participant_key_pair(&self) -> &Keypair;
}

pub trait SetBorrowerSpendInfo: Sized {
    fn set_borrower_spend_info(self, info: prefund::BorrowerSpendInfo) -> Result<Self, (Self, BorrowerInfoError)>;
}

impl Ted<escrow::ReceivingBorrowerInfo<participant::TedO>, escrow::ReceivingBorrowerInfo<participant::TedP>> {
    /// Initializes the contract.
    ///
    /// Matches the supplied keys with those in the offer. Returns `None` if the don't match.
    pub fn init(prefund_key: Keypair, escrow_key: Keypair, offer: offer::Offer) -> Option<Self> {
        if prefund_key.x_only_public_key().0 == *offer.prefund_keys.ted_o.as_x_only() && escrow_key.x_only_public_key().0 == *offer.escrow_keys.ted_o.as_x_only() {
            Some(Ted::O(participant::ted_o::init(prefund_key, escrow_key, offer)))
        } else if prefund_key.x_only_public_key().0 == *offer.prefund_keys.ted_p.as_x_only() && escrow_key.x_only_public_key().0 == *offer.escrow_keys.ted_p.as_x_only() {
            Some(Ted::P(participant::ted_p::init(prefund_key, escrow_key, offer)))
        } else {
            None
        }
    }

    pub fn prefund_borrower_info(self, borrower_info: prefund::BorrowerSpendInfo) -> Result<Self, (Self, BorrowerInfoError)> {
        match self {
            Ted::O(state) => state.prefund_borrower_info(borrower_info).map(Ted::O).map_err(|(state, error)| (Ted::O(state), error)),
            Ted::P(state) => state.prefund_borrower_info(borrower_info).map(Ted::P).map_err(|(state, error)| (Ted::P(state), error)),
        }
    }

    pub fn borrower_info(&self, borrower_info: escrow::BorrowerInfo<escrow::validation::Validated>) -> escrow::UnsignedTransactions {
        match self {
            Ted::O(state) => state.borrower_info(borrower_info),
            Ted::P(state) => state.borrower_info(borrower_info),
        }
    }

    pub fn set_and_sign_transactions(self, transactions: escrow::UnsignedTransactions, borrower: escrow::BorrowerSignatures, out: &mut Vec<u8>) -> Ted<escrow::WaitingForEscrowConfirmation<participant::TedO>, escrow::WaitingForEscrowConfirmation<participant::TedP>> {
        match self {
            Ted::O(state) => {
                let (state, sigs) = state.ted_o_set_and_sign_transactions(transactions, borrower);
                sigs.serialize(out);
                Ted::O(state)
            },
            Ted::P(state) => {
                let (state, sigs) = state.ted_p_set_and_sign_transactions(transactions, borrower);
                sigs.serialize(out);
                Ted::P(state)
            },
        }
    }
}

impl<O: Serialize + StateData, P: Serialize + StateData> Ted<O, P> {
    pub fn serialize(&self, out: &mut Vec<u8>) {
        match self {
            Ted::O(state) => state.serialize_with_header(out),
            Ted::P(state) => state.serialize_with_header(out),
        }
    }
}

impl<O: Deserialize + StateData, P: Deserialize + StateData> Ted<O, P> {
    pub fn deserialize(bytes: &mut &[u8]) -> Result<Self, StateDeserError<Ted<O::Error, P::Error>>> {
        let version = deserialize::StateVersion::deserialize(bytes)?;
        if bytes.len() < 2 {
            return Err(StateDeserError::UnexpectedEnd);
        }
        if bytes[0] == O::PARTICIPANT_ID as u8 {
            if bytes[1] != O::STATE_ID as u8 {
                return Err(StateDeserError::InvalidState(bytes[1]));
            }
            *bytes = &bytes[2..];
            O::deserialize(bytes, version)
                .map(Ted::O)
                .map_err(Ted::O)
                .map_err(StateDeserError::InvalidData)
        } else if bytes[0] == P::PARTICIPANT_ID as u8 {
            if bytes[1] != P::STATE_ID as u8 {
                return Err(StateDeserError::InvalidState(bytes[1]));
            }
            *bytes = &bytes[2..];
            P::deserialize(bytes, version)
                .map(Ted::P)
                .map_err(Ted::P)
                .map_err(StateDeserError::InvalidData)
        } else {
            Err(StateDeserError::InvalidParticipant(bytes[0]))
        }
    }
}

#[cfg(test)]
impl<O: quickcheck::Arbitrary, P: quickcheck::Arbitrary> quickcheck::Arbitrary for Ted<O, P> {
    fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
        if *gen.choose(&[true, false]).unwrap() {
            Ted::O(O::arbitrary(gen))
        } else {
            Ted::P(P::arbitrary(gen))
        }
    }
}

#[non_exhaustive]
#[derive(Debug)]
pub enum BorrowerInfoError {
    AlreadyReceived,
}

fn assemble_witness(borrower: &Signature, ted_o: &Signature, ted_p: &Signature, permutation: primitives::Permutation, script: &bitcoin::Script, control_block: &[u8]) -> bitcoin::Witness {
    let mut witness = bitcoin::Witness::new();
    let sigs = permutation.permute([borrower, ted_o, ted_p]);
    // These need to be pushed in reverse order because witness represents a stack so it's read
    // from the most-recently-pushed to the first-pushed element (if you consider keys to be in
    // forward order)
    witness.push(sigs[2].as_ref());
    witness.push(sigs[1].as_ref());
    witness.push(sigs[0].as_ref());
    witness.push(script.as_bytes());
    witness.push(control_block);
    witness
}

#[cfg(test)]
mod tests {
    use core::fmt;
    use super::{constants, participant};

    struct Empty<P>(core::marker::PhantomData<P>);

    impl<P> fmt::Debug for Empty<P> {
        fn fmt(&self, _: &mut fmt::Formatter) -> fmt::Result { Ok(()) }
    }

    impl<P> Clone for Empty<P> {
        fn clone(&self) -> Self {
            Empty(Default::default())
        }
    }

    impl<P: 'static> quickcheck::Arbitrary for Empty<P> {
        fn arbitrary(_: &mut quickcheck::Gen) -> Self {
            Empty(Default::default())
        }
    }

    impl<P> super::Serialize for Empty<P> {
        fn serialize(&self, _: &mut Vec<u8>) {}
    }

    impl<P> super::Deserialize for Empty<P> {
        type Error = core::convert::Infallible;

        fn deserialize(_: &mut &[u8], _: super::deserialize::StateVersion) -> Result<Self, Self::Error> {
            Ok(Empty(Default::default()))
        }
    }

    impl<P: super::Participant> super::StateData for Empty<P> {
        const STATE_ID: constants::StateId = constants::StateId::Prefund;
        const PARTICIPANT_ID: constants::ParticipantId = P::IDENTIFIER;
    }

    quickcheck::quickcheck! {
        fn ted_deserializes_the_same(ted: super::Ted<Empty<participant::TedO>, Empty<participant::TedP>>) -> bool {
            use super::Ted;

            let mut bytes = Vec::new();
            ted.serialize(&mut bytes);
            let ted2 = Ted::<Empty<participant::TedO>, Empty<participant::TedP>>::deserialize(&mut &*bytes).unwrap();
            match (ted2, ted) {
                (Ted::O(_), Ted::O(_)) | (Ted::P(_), Ted::P(_)) => true,
                (Ted::O(_), Ted::P(_)) | (Ted::P(_), Ted::O(_)) => false,
            }
        }
    }
}
