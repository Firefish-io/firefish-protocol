//! The prefund contract.
//!
//! This module contains the definition of the Firefish prefund contract.

use core::convert::TryInto;
use core::fmt;
use bitcoin::{Address, ScriptBuf, TxOut, Transaction, Witness};
use bitcoin::locktime::absolute::{LockTime, Height};
use bitcoin::p2p::Magic;
use bitcoin::taproot::{LeafVersion, TaprootSpendInfo};
use bitcoin::key::TweakedPublicKey;
use super::context;
use super::primitives::SpendableTxo;
//use super::multisig::MultisigSigningState;
use super::participant::{self, Participant};
use super::pub_keys::{PubKeys, PubKey};
use bitcoin::secp256k1::{Secp256k1, Verification};
use bitcoin::taproot::TapNodeHash;
use super::offer::TedSigPubKeys;
use super::{Serialize, Deserialize, StateData, constants, deserialize};

/// A refundable prepayment.
///
/// A `Prefund` is a contract that can be refunded by the sender if anythign goes wrong.
/// It serves as a way to implemenet 2-factor authentication of hot wallet funds and help with
/// consolidating UTXOs.
///
/// This step is necessary because we don't know which UTXOs will be used to fund the escrow contract.
pub struct Prefund<P: Participant> {
    /// The bitcoin network this contract operates on.
    network: bitcoin::Network,

    /// The keys used in the contract.
    pub(crate) keys: PubKeys<context::Prefund>,

    /// Hash of the backup spending script of the borrower.
    pub(crate) borrower_return_hash: TapNodeHash,

    /// The key use in the Taproot output.
    ///
    /// This is computed from other fields and stored here as a cache.
    pub(crate) output_key: TweakedPublicKey,

    pub(crate) parity: secp256k1::Parity,

    /// The participant-specific data.
    pub(crate) participant_data: P::PrefundData,
}

impl<P: Participant> Prefund<P> {
    pub fn keys(&self) -> &PubKeys<context::Prefund> {
        &self.keys
    }
}

crate::test_macros::impl_test_traits!(Prefund<P: Participant> where { P::PrefundData }, keys, borrower_return_hash, output_key, parity, participant_data, network);

#[cfg(test)]
mod helper {
    use super::*;
    struct PrefundHelper<P: Participant> {
        network: bitcoin::Network,
        pub(crate) keys: PubKeys<context::Prefund>,
        pub(crate) borrower_return_hash: TapNodeHash,
        pub(crate) participant_data: P::PrefundData,
    }

    impl<P: Participant> Clone for PrefundHelper<P> where P::PrefundData: Clone {
        fn clone(&self) -> Self {
            PrefundHelper {
                network: self.network.clone(),
                keys: self.keys.clone(),
                borrower_return_hash: self.borrower_return_hash.clone(),
                participant_data: self.participant_data.clone(),
            }
        }
    }

    crate::test_macros::impl_arbitrary!(PrefundHelper<P: Participant> where { P::PrefundData }, network, keys, borrower_return_hash, participant_data);

    impl<P: Participant + 'static> quickcheck::Arbitrary for super::Prefund<P> where P::PrefundData: quickcheck::Arbitrary + Clone {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            let data = PrefundHelper::<P>::arbitrary(gen);
            let (output_key, parity) = compute_output_key(bitcoin::secp256k1::SECP256K1, data.keys, data.borrower_return_hash);
            Prefund {
                network: data.network,
                keys: data.keys,
                borrower_return_hash: data.borrower_return_hash,
                participant_data: data.participant_data,
                output_key,
                parity,
            }
        }
    }
}

impl<P: Participant> StateData for Prefund<P> {
    const PARTICIPANT_ID: constants::ParticipantId = P::IDENTIFIER;
    const STATE_ID: constants::StateId = constants::StateId::Prefund;
}

impl<P: Participant> Serialize for Prefund<P> where P::PrefundData: super::Serialize {
    fn serialize(&self, out: &mut Vec<u8>) {
        out.reserve(4 + 3 * 32 + 32);
        out.extend_from_slice(&self.network.magic().to_bytes());
        self.keys.serialize_raw(out);
        out.extend_from_slice(self.borrower_return_hash.as_ref());
        // no need to store output key since it's a cache
        self.participant_data.serialize(out);
    }
}

impl<P: Participant> Deserialize for Prefund<P> where P::PrefundData: super::Deserialize {
    type Error = PrefundDeserializationError<<P::PrefundData as super::Deserialize>::Error>;

    fn deserialize(bytes: &mut &[u8], version: deserialize::StateVersion) -> Result<Self, Self::Error> {
        let magic = deserialize::magic(bytes)?;
        let network = bitcoin::Network::from_magic(magic)
            .ok_or(PrefundDeserializationErrorInner::UnknownNetwork(magic))?;
        let keys = PubKeys::deserialize_raw(bytes).map_err(PrefundDeserializationErrorInner::from)?;
        if bytes.len() < 32 {
            return Err(PrefundDeserializationErrorInner::UnexpectedEnd.into());
        }
        let borrower_return_hash = TapNodeHash::assume_hidden(bytes[..32].try_into().expect("checked above"));

        let (output_key, parity) = compute_output_key(bitcoin::secp256k1::SECP256K1, keys, borrower_return_hash);
        *bytes = &bytes[32..];
        let participant_data = P::PrefundData::deserialize(bytes, version).map_err(PrefundDeserializationErrorInner::Participant)?;

        let prefund = Prefund {
            network,
            keys,
            borrower_return_hash,
            output_key,
            parity,
            participant_data,
        };
        Ok(prefund)
    }
}

#[derive(Debug)]
pub struct PrefundDeserializationError<E>(PrefundDeserializationErrorInner<E>);

impl<E> From<deserialize::UnexpectedEnd> for PrefundDeserializationError<E> {
    fn from(_: deserialize::UnexpectedEnd) -> Self {
        PrefundDeserializationError(PrefundDeserializationErrorInner::UnexpectedEnd)
    }
}

impl<E> From<PrefundDeserializationErrorInner<E>> for PrefundDeserializationError<E> {
    fn from(error: PrefundDeserializationErrorInner<E>) -> Self {
        PrefundDeserializationError(error)
    }
}

#[derive(Debug)]
enum PrefundDeserializationErrorInner<E> {
    UnexpectedEnd,
    InvalidKey(bitcoin::secp256k1::Error),
    DuplicateKeys(super::pub_keys::Error),
    UnknownNetwork(Magic),
    Participant(E),
}

impl<E> From<super::pub_keys::RawDeserError> for PrefundDeserializationErrorInner<E> {
    fn from(error: super::pub_keys::RawDeserError) -> Self {
        use super::pub_keys::RawDeserError;
        match error {
            RawDeserError::InvalidKey(error) => PrefundDeserializationErrorInner::InvalidKey(error),
            RawDeserError::DuplicateKeys(error) => PrefundDeserializationErrorInner::DuplicateKeys(error),
        }
    }
}

impl<P: Participant> Prefund<P> {
    /// Returns the address to which satoshis need to be sent by the borrower.
    pub fn funding_address(&self) -> Address {
        Address::p2tr_tweaked(self.output_key, self.network)
    }

    /// Returns the script to which satoshis need to be sent by the borrower.
    pub fn funding_script(&self) -> ScriptBuf {
        ScriptBuf::new_p2tr_tweaked(self.output_key)
    }

    pub fn borrower_info(&self) -> BorrowerSpendInfo {
        BorrowerSpendInfo {
            key: self.keys.borrower_eph,
            return_hash: self.borrower_return_hash,
        }
    }

    pub fn network(&self) -> bitcoin::Network {
        self.network
    }
}

impl Prefund<participant::Borrower> {
    /// Used when the borrower decides to cancel the contract in the prefund stage.
    pub fn spend_borrower(&self, inputs: Vec<SpendableTxo>, outputs: Vec<TxOut>, current_height: Height) -> Transaction {
        use bitcoin::sighash::{SighashCache, Prevouts, TapSighashType};
        use bitcoin::taproot::ControlBlock;
        use super::HotKey;

        let (prevouts, inputs): (Vec<_>, Vec<_>) = inputs
            .into_iter()
            .map(SpendableTxo::unpack_with_empty_sig)
            .unzip();

        let lock_time = LockTime::Blocks(current_height);
        let output_script = self.funding_script();
        let internal_key = self.keys.generate_internal_key();
        let multisig_script = self.keys.generate_multisig_script();
        let multisig_script_hash = multisig_script.tapscript_leaf_hash();
        let multisig_script_hash = TapNodeHash::from(multisig_script_hash);
        let (_, tapscript) = self.participant_data.borrower_key_and_leaf_script();
        let merkle_branch = [multisig_script_hash].into();
        let control_block = ControlBlock {
            leaf_version: LeafVersion::TapScript,
            internal_key,
            output_key_parity: self.parity,
            merkle_branch,
        };
        let control_block = control_block.serialize();
        let leaf_hash = tapscript.tapscript_leaf_hash();

        let mut transaction = Transaction {
            version: bitcoin::transaction::Version(2),
            input: inputs,
            output: outputs,
            lock_time,
        };
        let mut cache = SighashCache::new(&transaction);
        let prevouts_all = Prevouts::All(&prevouts);
        // We have to collect witnesses first and modify later due to `bitcoin` library limitation
        // See https://github.com/rust-bitcoin/rust-bitcoin/issues/1423
        let witnesses = prevouts.iter()
            .enumerate()
            .map(|(i, txout)| {
                if txout.script_pubkey == output_script {
                    let sighash = cache.taproot_script_spend_signature_hash(i, &prevouts_all, leaf_hash, TapSighashType::Default)
                        .expect("we've provided correct data");
                    let sig = secp256k1::SECP256K1.sign_schnorr(&sighash.into(), self.participant_data.participant_key_pair());
                    let mut witness = Witness::new();
                    witness.push(sig.as_ref());
                    witness.push(&tapscript);
                    witness.push(&control_block);
                    witness
                } else {
                    Witness::new()
                }
            })
            .collect::<Vec<_>>();
        for (input, witness) in transaction.input.iter_mut().zip(witnesses) {
            input.witness = witness;
        }
        transaction
    }
}

/// The state of the prefund contract when the borrower information is not yet known.
pub struct ReceivingBorrowerInfo<P: Participant> {
    network: bitcoin::Network,

    keys: TedSigPubKeys<context::Prefund>,

    /// Participant-specific data.
    participant_data: P::PrefundData,
}

crate::test_macros::impl_test_traits!(ReceivingBorrowerInfo<P: Participant> where { P::PrefundData }, network, keys, participant_data);
crate::test_macros::impl_arbitrary!(ReceivingBorrowerInfo<P: Participant> where { P::PrefundData }, network, keys, participant_data);

fn compute_output_key(ctx: &Secp256k1<impl Verification>, keys: PubKeys<context::Prefund>, borrower_hash: TapNodeHash) -> (TweakedPublicKey, secp256k1::Parity) {
    let multisig_script = keys.generate_multisig_script();
    let multisig_hash = multisig_script.tapscript_leaf_hash();
    let root = TapNodeHash::from_node_hashes(borrower_hash, multisig_hash.into());
    let internal_key = keys.generate_internal_key();
    let spend_info = TaprootSpendInfo::new_key_spend(&ctx, internal_key, Some(root));
    (spend_info.output_key(), spend_info.output_key_parity())
}

impl<P: Participant> ReceivingBorrowerInfo<P> {
    pub fn new(keys: TedSigPubKeys<context::Prefund>, network: bitcoin::Network) -> Self where P::PrefundData: Default {
        Self::with_participant_data(keys, network, Default::default())
    }

    pub fn with_participant_data(keys: TedSigPubKeys<context::Prefund>, network: bitcoin::Network, participant_data: P::PrefundData) -> Self {
        ReceivingBorrowerInfo {
            network,
            keys,
            participant_data,
        }
    }

    /// Processes the borrower's information.
    ///
    /// This function is called by other parties when the borrower's information is received.
    pub fn borrower_info_received(self, ctx: &Secp256k1<impl Verification>, borrower_info: BorrowerSpendInfo) -> Prefund<P>  {
        let keys = self.keys.add_borrower_eph(borrower_info.key);
        let (output_key, parity) = compute_output_key(ctx, keys, borrower_info.return_hash);

        let prefund = Prefund {
            network: self.network,
            keys,
            borrower_return_hash: borrower_info.return_hash,
            participant_data: self.participant_data,
            output_key,
            parity,
        };
        prefund
    }
}

impl<P: Participant> StateData for ReceivingBorrowerInfo<P> {
    const PARTICIPANT_ID: constants::ParticipantId = P::IDENTIFIER;
    const STATE_ID: constants::StateId = constants::StateId::PrefundReceivingBorrowerData;
}

impl<P: Participant> Serialize for ReceivingBorrowerInfo<P> where P::PrefundData: Serialize {
    fn serialize(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.network.magic().to_bytes());
        self.keys.serialize(out);
        self.participant_data.serialize(out);
    }
}

impl<P: Participant> Deserialize for ReceivingBorrowerInfo<P> where P::PrefundData: Deserialize {
    type Error = ReceivingBorrowerInfoDeserError<<P::PrefundData as Deserialize>::Error>;
    fn deserialize(bytes: &mut &[u8], version: deserialize::StateVersion) -> Result<Self, Self::Error> {
        if bytes.len() < 68 {
            return Err(ReceivingBorrowerInfoDeserError(ReceivingBorrowerInfoDeserErrorInner::UnexpectedEnd));
        }
        let magic = deserialize::magic(bytes)?;
        let network = bitcoin::Network::from_magic(magic)
            .ok_or(ReceivingBorrowerInfoDeserError(ReceivingBorrowerInfoDeserErrorInner::InvalidNetwork(magic)))?;
        let keys = TedSigPubKeys::deserialize(bytes)
            .map_err(ReceivingBorrowerInfoDeserErrorInner::Keys)
            .map_err(ReceivingBorrowerInfoDeserError)?;
        let participant_data = P::PrefundData::deserialize(bytes, version)
            .map_err(ReceivingBorrowerInfoDeserErrorInner::Participant)
            .map_err(ReceivingBorrowerInfoDeserError)?;

        Ok(ReceivingBorrowerInfo { network ,keys, participant_data, })
    }
}

#[derive(Debug)]
pub struct ReceivingBorrowerInfoDeserError<E>(ReceivingBorrowerInfoDeserErrorInner<E>);

impl<E> From<deserialize::UnexpectedEnd> for ReceivingBorrowerInfoDeserError<E> {
    fn from(_: deserialize::UnexpectedEnd) -> Self {
        ReceivingBorrowerInfoDeserError(ReceivingBorrowerInfoDeserErrorInner::UnexpectedEnd)
    }
}

#[derive(Debug)]
enum ReceivingBorrowerInfoDeserErrorInner<E> {
    UnexpectedEnd,
    InvalidNetwork(Magic),
    Keys(super::offer::DeserializationError),
    Participant(E),
}

/// The state of the prefund contract.
pub enum State<P: Participant> {
    /// The prefund contract is being created.
    ReceivingBorrowerInfo(ReceivingBorrowerInfo<P>),

    /// The prefund contract is ready to be funded.
    Ready(Prefund<P>),
}

impl<P: Participant> State<P> {
    pub fn new(keys: TedSigPubKeys<context::Prefund>, network: bitcoin::Network) -> Self where P::PrefundData: Default {
        State::ReceivingBorrowerInfo(ReceivingBorrowerInfo::new(keys, network))
    }

    pub fn with_participant_data(keys: TedSigPubKeys<context::Prefund>, network: bitcoin::Network, participant_data: P::PrefundData) -> Self {
        State::ReceivingBorrowerInfo(ReceivingBorrowerInfo::with_participant_data(keys, network, participant_data))
    }

    pub fn serialize(&self, out: &mut Vec<u8>) where P::PrefundData: super::Serialize {
        // The individual variants are self-tagged
        match self {
            State::ReceivingBorrowerInfo(state) => state.serialize_with_header(out),
            State::Ready(state) => state.serialize_with_header(out),
        }
    }

    pub(crate) fn serialize_unversioned(&self, out: &mut Vec<u8>) where P::PrefundData: super::Serialize {
        // The individual variants are self-tagged
        match self {
            State::ReceivingBorrowerInfo(state) => state.serialize_with_header_unversioned(out),
            State::Ready(state) => state.serialize_with_header_unversioned(out),
        }
    }

    pub fn deserialize(bytes: &mut &[u8]) -> Result<Self, StateDeserError<<P::PrefundData as Deserialize>::Error>> where P::PrefundData: super::Deserialize {
        let version = deserialize::StateVersion::deserialize(bytes)?;
        Self::deserialize_fixed_version(bytes, version)
    }

    /// Deserializes specific version of the state (not reading it from the data).
    ///
    /// This is used in sub-structs where state ID needs to be known but the version is the same,
    /// so storing it would be duplication. We would like to also avoid storing participant ID but
    /// sadly, that was forgotten in the initial version and changing it would break things.
    pub(crate) fn deserialize_fixed_version(bytes: &mut &[u8], version: deserialize::StateVersion) -> Result<Self, StateDeserError<<P::PrefundData as Deserialize>::Error>> where P::PrefundData: super::Deserialize {
        if bytes.len() < 2 {
            return Err(StateDeserError::UnexpectedEnd);
        }
        if bytes[0] == P::IDENTIFIER as u8 {
            if bytes[1] == ReceivingBorrowerInfo::<P>::STATE_ID as u8 {
                *bytes = &bytes[2..];
                ReceivingBorrowerInfo::deserialize(bytes, version)
                    .map(State::ReceivingBorrowerInfo)
                    .map_err(StateDeserError::InvalidRbiData)
            } else if bytes[1] == Prefund::<P>::STATE_ID as u8 {
                *bytes = &bytes[2..];
                Prefund::deserialize(bytes, version)
                    .map(State::Ready)
                    .map_err(StateDeserError::InvalidPrefundData)
            } else {
                Err(StateDeserError::InvalidState(bytes[1]))
            }
        } else {
            Err(StateDeserError::InvalidParticipant(bytes[0]))
        }
    }
}

impl<P: Participant> fmt::Debug for State<P> where P::PrefundData: fmt::Debug {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Both structs tell their name so repeating it is not needed.
        match self {
            State::ReceivingBorrowerInfo(rbi) => fmt::Debug::fmt(rbi, f),
            State::Ready(prefund) => fmt::Debug::fmt(prefund, f),
        }
    }
}

impl<P: Participant> Clone for State<P> where P::PrefundData: Clone {
    fn clone(&self) -> Self {
        match self {
            State::ReceivingBorrowerInfo(rbi) => State::ReceivingBorrowerInfo(rbi.clone()),
            State::Ready(prefund) => State::Ready(prefund.clone()),
        }
    }
}

impl<P: Participant> PartialEq for State<P> where P::PrefundData: PartialEq {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (State::ReceivingBorrowerInfo(left), State::ReceivingBorrowerInfo(right)) => left == right,
            (State::Ready(left), State::Ready(right)) => left == right,
            (State::ReceivingBorrowerInfo(_), State::Ready(_)) | (State::Ready(_), State::ReceivingBorrowerInfo(_)) => false,
        }
    }
}

#[cfg(test)]
impl<P: Participant + 'static> quickcheck::Arbitrary for State<P> where P::PrefundData: quickcheck::Arbitrary {
    fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
        if *gen.choose(&[true, false]).unwrap() {
            State::Ready(quickcheck::Arbitrary::arbitrary(gen))
        } else {
            State::ReceivingBorrowerInfo(quickcheck::Arbitrary::arbitrary(gen))
        }
    }
}

#[derive(Debug)]
pub enum StateDeserError<E> {
    UnexpectedEnd,
    UnsupportedVersion(u32),
    InvalidState(u8),
    InvalidParticipant(u8),
    InvalidRbiData(ReceivingBorrowerInfoDeserError<E>),
    InvalidPrefundData(PrefundDeserializationError<E>),
}

impl<E> From<deserialize::StateVersionDeserError> for StateDeserError<E> {
    fn from(value: deserialize::StateVersionDeserError) -> Self {
        match value {
            deserialize::StateVersionDeserError::UnexpectedEnd => Self::UnexpectedEnd,
            deserialize::StateVersionDeserError::UnsupportedVersion(version) => Self::UnsupportedVersion(version),
        }
    }
}

/// Information about the borrower's spending conditions.
#[derive(Clone)]
pub struct BorrowerSpendInfo {
    pub key: PubKey<participant::Borrower, context::Prefund>,
    // Hash of Taproot node representing spending conditions for return transaction
    pub return_hash: TapNodeHash,
}

impl BorrowerSpendInfo {
    pub fn serialize(&self, out: &mut Vec<u8>) {
        out.reserve(1 + 32 + 32);
        out.push(super::constants::MessageId::PrefundBorrowerInfo as u8);
        self.key.serialize_raw(out);
        out.extend_from_slice(self.return_hash.as_ref());
    }

    pub fn deserialize(bytes: &mut &[u8]) -> Result<Self, BorrowerSpendInfoDeserError> {
        if bytes.len() < 1 + 32 + 32 {
            return Err(BorrowerSpendInfoDeserError(BorrowerSpendInfoDeserErrorInner::UnexpectedEnd));
        }
        if bytes[0] != super::constants::MessageId::PrefundBorrowerInfo as u8 {
            return Err(BorrowerSpendInfoDeserError(BorrowerSpendInfoDeserErrorInner::InvalidMessage(bytes[0])));
        }
        *bytes = &bytes[1..];
        let key = PubKey::deserialize_raw(bytes)
            .map_err(BorrowerSpendInfoDeserErrorInner::Secp256k1)
            .map_err(BorrowerSpendInfoDeserError)?;
        let return_hash = TapNodeHash::assume_hidden(bytes[..32].try_into().expect("checked above"));
        *bytes = &bytes[32..];
        Ok(BorrowerSpendInfo {key, return_hash })
    }
}

#[derive(Debug)]
pub struct BorrowerSpendInfoDeserError(BorrowerSpendInfoDeserErrorInner);

#[derive(Debug)]
enum BorrowerSpendInfoDeserErrorInner {
    UnexpectedEnd,
    InvalidMessage(u8),
    Secp256k1(secp256k1::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    crate::test_macros::check_roundtrip_with_version!(roundtrip_prefund, Prefund<participant::Borrower>);
    crate::test_macros::check_roundtrip_with_version!(roundtrip_receiving_borrower_info, ReceivingBorrowerInfo<participant::Borrower>);
    crate::test_macros::check_roundtrip!(roundtrip_state, State<participant::Borrower>);
}
