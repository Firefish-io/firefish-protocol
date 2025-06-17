//! The escrow contract.
//!
//! This module contains the definition of the Firefish escrow contract.
//! 

use core::convert::TryInto;
use bitcoin::{Transaction, TxIn, TxOut, ScriptBuf, OutPoint, Sequence, Witness, key::XOnlyPublicKey};
use bitcoin::secp256k1::schnorr::Signature;
use bitcoin::locktime::absolute::{Height, LockTime};
use bitcoin::taproot::{LeafVersion, TapLeafHash, TapNodeHash, TaprootSpendInfo};
use bitcoin::key::Keypair;

use super::deserialize;
use super::{Serialize, Deserialize, context, participant, offer, constants};
use super::pub_keys::{PubKey, PubKeys};
use super::participant::Participant;
use super::primitives::{SpendableTxo, Permutation};

/// Only accept this many inputs in transaction.
///
/// The value of the constant is block_size / min_txin_size.
///
/// More inputs than this definitely wouldn't fit the block, so this constant is a maximum sensible
/// number. In practice, it is likely much lower but we don't care.
const MAX_INPUT_COUNT: u32 = 4_000_000 / (32 + 4 + 4 + 1);

pub(crate) type EscrowKeys = offer::TedSigPubKeys<context::Escrow>;

pub mod validation {
    pub enum Unvalidated {}
    pub enum Validated {}
}

/// State of the escrow contract.
pub enum State<P: Participant> {
    /// Information from borrower is not known yet.
    ///
    /// The list of unknown parts is defined by [`BorrowerInfo`]
    ///
    ReceivingBorrowerInfo(ReceivingBorrowerInfo<P>),

    /// The escrow signature is not known yet.
    ///
    /// This signing the escrow transaction is required to finalize the contract.
    ReceivingEscrowSignature(ReceivingEscrowSignature<P>),


    /// The contract is finalized, after the transaction confirms it is safe to send fiat to the
    /// borrower.
    EscrowSigned(EscrowSigned<P>),
}

impl<P: Participant> State<P> {
    pub fn new(params: offer::EscrowParams, keys: EscrowKeys) -> Self where P::PreEscrowData: Default {
        State::ReceivingBorrowerInfo(ReceivingBorrowerInfo::new(params, keys))
    }

    pub fn with_participant_data(params: offer::EscrowParams, keys: EscrowKeys, participant_data: P::PreEscrowData) -> Self {
        State::ReceivingBorrowerInfo(ReceivingBorrowerInfo::with_participant_data(params, keys, participant_data))
    }

    pub fn participant_data(&self) -> &P::PreEscrowData {
        match self {
            State::ReceivingBorrowerInfo(state) => &state.participant_data,
            State::ReceivingEscrowSignature(state) => &state.participant_data,
            State::EscrowSigned(state) => &state.participant_data,
        }
    }
}

const TX_VERSION: bitcoin::transaction::Version = bitcoin::transaction::Version(2);

/// The participant is waiting for required infromation from borrower.
///
/// This is the first state of the escrow contract.
///
/// The borrower is expected to provide the following information:
///
/// * Ephemeral public key
/// * Information about the borrower's spending conditions
/// * Inputs to spend
/// * Signatures for state transition transactions
pub struct ReceivingBorrowerInfo<P: Participant> {
    pub params: offer::EscrowParams,
    keys: EscrowKeys,
    pub participant_data: P::PreEscrowData,
}

crate::test_macros::impl_test_traits!(ReceivingBorrowerInfo<P: Participant> where { P::PreEscrowData }, params, keys, participant_data);
crate::test_macros::impl_arbitrary!(ReceivingBorrowerInfo<P: Participant> where { P::PreEscrowData }, params, keys, participant_data);

impl<P: Participant> ReceivingBorrowerInfo<P> {
    /// Initializes the receiver.
    pub fn new(params: offer::EscrowParams, keys: EscrowKeys) -> Self where P::PreEscrowData: Default {
        Self::with_participant_data(params, keys, Default::default())
    }

    /// Initializes the receiver.
    pub fn with_participant_data(params: offer::EscrowParams, keys: EscrowKeys, participant_data: P::PreEscrowData) -> Self {
        ReceivingBorrowerInfo {
            params,
            keys,
            participant_data,
        }
    }

    pub fn prefund_borrower_info(self, borrower_info: super::prefund::BorrowerSpendInfo) -> Result<Self, (Self, super::BorrowerInfoError)> where P::PreEscrowData: super::SetBorrowerSpendInfo {
        use super::SetBorrowerSpendInfo;

        let (participant_data, error) = match self.participant_data.set_borrower_spend_info(borrower_info) {
            Ok(state) => (state, None),
            Err((state, error)) => (state, Some(error)),
        };

        let new_state = ReceivingBorrowerInfo {
            keys: self.keys,
            params: self.params,
            participant_data,
        };
        match error {
            None => Ok(new_state),
            Some(error) => Err((new_state, error)),
        }
    }

    /// Called when borrower information is received.
    ///
    /// This constructs `UnsignedTransactions` which can be used to verify the signatures.
    pub fn borrower_info(&self, borrower_info: BorrowerInfo<validation::Validated>) -> UnsignedTransactions {
        let keys = self.keys.add_borrower_eph(borrower_info.escrow_eph_key);
        let (escrow_out_script, multisig_leaf_hash, _) = output_script(&keys);

        let escrow_txout = TxOut {
            value: borrower_info.escrow_amount,
            script_pubkey: escrow_out_script,
        };
        let escrow_output_index = borrower_info.escrow_contract_output_position as usize;
        let mut escrow_txouts = borrower_info.escrow_extra_outputs;
        escrow_txouts.insert(escrow_output_index, escrow_txout);
        let (escrow_prevouts, escrow_txins) = borrower_info.inputs
            .into_iter()
            .map(SpendableTxo::unpack_with_empty_sig)
            .unzip();
        let escrow_tx = Transaction {
            // Enable relative time locks
            version: TX_VERSION,
            input: escrow_txins,
            output: escrow_txouts,
            lock_time: LockTime::from(borrower_info.tx_height).into(),
        };
        let escrow_txid = escrow_tx.compute_txid();
        let escrow_out_point = OutPoint {
            txid: escrow_txid,
            vout: borrower_info.escrow_contract_output_position,
        };
        let escrow_non_recover_txin = TxIn {
            previous_output: escrow_out_point,
            script_sig: ScriptBuf::new(),
            // Since non-recover transactions don't use lock time in the contract and we can't
            // predict when they will be broadcasted setting same height as the previous
            // transaction would create an identifiable footprint. There are still wallets that
            // don't implement anti-fee-sniping policy so it's better to hide among them rather
            // than implement broken anti-fee-sniping. And if we don't use lock time anyway we
            // should just disable it.
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: Witness::new(),
        };
        let escrow_non_recover_txins = vec![escrow_non_recover_txin];
        let liquidator_output_default = TxOut {
            script_pubkey: self.params.liquidator_script_default.clone(),
            value: borrower_info.collateral_amount_default,
        };
        let liquidator_output_liquidation = TxOut {
            script_pubkey: self.params.liquidator_script_liquidation.clone(),
            value: borrower_info.collateral_amount_liquidation,
        };
        fn vec_with_item_inserted<T: Clone>(base: &[T], inserted: T, index: usize) -> Vec<T> {
            let mut result = Vec::with_capacity(base.len() + 1);
            let mut iter = base.iter().cloned();
            result.extend(iter.by_ref().take(index));
            result.push(inserted);
            result.extend(iter);
            result
        }
        let termination_outputs_default = vec_with_item_inserted(&self.params.extra_termination_outputs, liquidator_output_default, self.params.liquidator_output_index);
        let termination_outputs_liquidation = vec_with_item_inserted(&self.params.extra_termination_outputs, liquidator_output_liquidation, self.params.liquidator_output_index);

        let repayment_tx = Transaction {
            // Enable relative time locks
            version: TX_VERSION,
            input: escrow_non_recover_txins.clone(),
            output: borrower_info.repayment_outputs,
            lock_time: LockTime::ZERO,
        };
        let default_tx = Transaction {
            // Enable relative time locks
            version: TX_VERSION,
            input: escrow_non_recover_txins.clone(),
            output: termination_outputs_default,
            lock_time: self.params.default_lock_time,
        };
        let liquidation_tx = Transaction {
            // Enable relative time locks
            version: TX_VERSION,
            input: escrow_non_recover_txins,
            output: termination_outputs_liquidation,
            lock_time: LockTime::ZERO,
        };
        let escrow_recover_txin = TxIn {
            previous_output: escrow_out_point,
            script_sig: ScriptBuf::new(),
            // Enable both RBF and lock time
            sequence: Sequence::ZERO,
            witness: Witness::new(),
        };
        let escrow_recover_txins = vec![escrow_recover_txin];
        let recover_tx = Transaction {
            version: TX_VERSION,
            input: escrow_recover_txins,
            output: borrower_info.recover_outputs,
            lock_time: self.params.recover_lock_time.into(),
        };

        UnsignedTransactions {
            borrower_eph: borrower_info.escrow_eph_key,
            multisig_leaf_hash,
            contract_index: borrower_info.escrow_contract_output_position,
            escrow_prevouts,
            escrow: escrow_tx,
            repayment: repayment_tx,
            default: default_tx,
            liquidation: liquidation_tx,
            recover: recover_tx,
        }
    }

    pub fn transactions_validated(self, unsigned_txes: UnsignedTransactions, recover: Signature, repayment: Signature) -> ReceivingEscrowSignature<P> {
        ReceivingEscrowSignature {
            params: self.params,
            keys: self.keys,
            unsigned_txes,
            participant_data: self.participant_data,
            recover_signature: recover,
            repayment_signature: repayment,
        }
    }

    pub fn transactions_presigned(self, unsigned_txes: UnsignedTransactions, borrower: BorrowerSignatures) -> WaitingForEscrowConfirmation<P> {
        WaitingForEscrowConfirmation {
            params: self.params,
            keys: self.keys,
            borrower,
            unsigned_txes,
            participant_data: self.participant_data,
        }
    }
}

impl<P: Participant> super::StateData for ReceivingBorrowerInfo<P> where P::PreEscrowData: super::Serialize {
    const STATE_ID: constants::StateId = constants::StateId::EscrowReceivingBorrowerInfo;
    const PARTICIPANT_ID: constants::ParticipantId = P::IDENTIFIER;
}
impl<P: Participant> super::Serialize for ReceivingBorrowerInfo<P> where P::PreEscrowData: super::Serialize {
    fn serialize(&self, out: &mut Vec<u8>) {
        out.reserve(self.params.reserve_suggestion() + 64);
        self.keys.serialize(out);
        self.params.serialize(out);
        self.participant_data.serialize(out);
    }
}

impl<P: Participant> super::Deserialize for ReceivingBorrowerInfo<P> where P::PreEscrowData: super::Deserialize {
    type Error = ReceivingBorrowerInfoDeserError<<P::PreEscrowData as super::Deserialize>::Error>;

    fn deserialize(bytes: &mut &[u8], version: deserialize::StateVersion) -> std::result::Result<Self, Self::Error> {
        if bytes.len() < 64 {
            return Err(ReceivingBorrowerInfoDeserErrorInner::Offer(super::offer::DeserializationError::UnexpectedEnd).into());
        }
        let keys = super::offer::TedSigPubKeys::deserialize(bytes).map_err(ReceivingBorrowerInfoDeserErrorInner::Offer)?;
        let escrow_params_version = match version {
            deserialize::StateVersion::V0 => super::offer::EscrowParamsVersion::V0,
            deserialize::StateVersion::V1 => super::offer::EscrowParamsVersion::V1,
        };
        let params = super::offer::EscrowParams::deserialize(bytes, escrow_params_version).map_err(ReceivingBorrowerInfoDeserErrorInner::Offer)?;
        let participant_data = P::PreEscrowData::deserialize(bytes, version).map_err(ReceivingBorrowerInfoDeserErrorInner::Participant)?;
        Ok(ReceivingBorrowerInfo {
            keys,
            params,
            participant_data,
        })
    }
}

/// The participant is waiting for escrow to confirm in the chain.
#[allow(unused)]
pub struct WaitingForEscrowConfirmation<P: Participant> {
    pub(crate) params: offer::EscrowParams,
    pub(crate) borrower: BorrowerSignatures,
    pub(crate) keys: EscrowKeys,
    pub(crate) unsigned_txes: UnsignedTransactions,
    pub(crate) participant_data: P::PreEscrowData,
}

crate::test_macros::impl_test_traits!(WaitingForEscrowConfirmation<P: Participant> where { P::PreEscrowData }, params, borrower, keys, unsigned_txes, participant_data);

#[cfg(test)]
impl<P: Participant + 'static> quickcheck::Arbitrary for WaitingForEscrowConfirmation<P> where P::PreEscrowData: quickcheck::Arbitrary {
    fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
        struct WaitingForEscrowConfirmationHelper<P: Participant> {
            params: offer::EscrowParams,
            borrower: BorrowerSignatures,
            keys: EscrowKeys,
            participant_data: P::PreEscrowData,
        }
        crate::test_macros::impl_test_traits!(WaitingForEscrowConfirmationHelper<P: Participant> where { P::PreEscrowData }, params, borrower, keys, participant_data);
        crate::test_macros::impl_arbitrary!(WaitingForEscrowConfirmationHelper<P: Participant> where { P::PreEscrowData }, params, borrower, keys, participant_data);

        let helper = <WaitingForEscrowConfirmationHelper<P> as quickcheck::Arbitrary>::arbitrary(gen);
        let unsigned_txes = UnsignedTransactions::arbitrary(gen, helper.keys);

        WaitingForEscrowConfirmation {
            params: helper.params,
            borrower: helper.borrower,
            keys: helper.keys,
            unsigned_txes,
            participant_data: helper.participant_data,
        }
    }
}

impl<P: Participant> super::StateData for WaitingForEscrowConfirmation<P> {
    const STATE_ID: constants::StateId = constants::StateId::WaitingForEscrowConfirmation;
    const PARTICIPANT_ID: constants::ParticipantId = P::IDENTIFIER;
}

impl<P: super::Participant> WaitingForEscrowConfirmation<P> {
    pub fn escrow_txid(&self) -> bitcoin::Txid {
        self.unsigned_txes.escrow.compute_txid()
    }
}

impl<P: Participant> Serialize for WaitingForEscrowConfirmation<P> where P::PreEscrowData: super::Serialize {
    fn serialize(&self, out: &mut Vec<u8>) {
        // TODO: state marker
        self.keys.serialize(out);
        self.borrower.serialize(out);
        self.params.serialize(out);
        self.unsigned_txes.serialize(out);
        self.participant_data.serialize(out);
    }
}

impl<P: Participant> super::Deserialize for WaitingForEscrowConfirmation<P>  where P::PreEscrowData: super::Deserialize {
    type Error = ReceivingEscrowSignatureDeserError<<P::PreEscrowData as super::Deserialize>::Error>;

    fn deserialize(bytes: &mut &[u8], version: deserialize::StateVersion) -> Result<Self, Self::Error> {
        let escrow_params_version = match version {
            deserialize::StateVersion::V0 => super::offer::EscrowParamsVersion::V0,
            deserialize::StateVersion::V1 => super::offer::EscrowParamsVersion::V1,
        };
        let keys = offer::TedSigPubKeys::deserialize(bytes)
            .map_err(ReceivingEscrowSignatureDeserErrorInner::Keys)
            .map_err(ReceivingEscrowSignatureDeserError)?;
        let borrower = BorrowerSignatures::deserialize(bytes)
            .map_err(ReceivingEscrowSignatureDeserErrorInner::Borrower)
            .map_err(ReceivingEscrowSignatureDeserError)?;
        let params = offer::EscrowParams::deserialize(bytes, escrow_params_version)
            .map_err(ReceivingEscrowSignatureDeserErrorInner::Params)
            .map_err(ReceivingEscrowSignatureDeserError)?;
        let unsigned_txes = UnsignedTransactions::deserialize(bytes, keys)
            .map_err(ReceivingEscrowSignatureDeserErrorInner::Txes)
            .map_err(ReceivingEscrowSignatureDeserError)?;
        let participant_data = P::PreEscrowData::deserialize(bytes, version)
            .map_err(ReceivingEscrowSignatureDeserErrorInner::Participant)
            .map_err(ReceivingEscrowSignatureDeserError)?;
        let state = WaitingForEscrowConfirmation {
            params,
            borrower,
            keys,
            unsigned_txes,
            participant_data,
        };
        Ok(state)
    }
}

#[derive(Debug)]
pub struct ReceivingBorrowerInfoDeserError<E>(ReceivingBorrowerInfoDeserErrorInner<E>);

impl<E> From<ReceivingBorrowerInfoDeserErrorInner<E>> for ReceivingBorrowerInfoDeserError<E> {
    fn from(error: ReceivingBorrowerInfoDeserErrorInner<E>) -> Self {
        ReceivingBorrowerInfoDeserError(error)
    }
}

#[derive(Debug)]
enum  ReceivingBorrowerInfoDeserErrorInner<E> {
    Offer(super::offer::DeserializationError),
    Participant(E)
}

#[derive(Debug)]
pub struct BorrowerInfoMessage {
    pub borrower_info: BorrowerInfo<validation::Unvalidated>,
    pub signatures: BorrowerSignatures,
}

impl BorrowerInfoMessage {
    pub fn deserialize(bytes: &mut &[u8]) -> Result<Self, BorrowerInfoMessageDeserError> {
        let borrower_info = BorrowerInfo::deserialize(bytes)?;
        let signatures = BorrowerSignatures::deserialize(bytes)?;
        Ok(BorrowerInfoMessage { borrower_info, signatures, })
    }
}

#[derive(Debug)]
pub enum BorrowerInfoMessageDeserError {
    BorrowerInfo(BorrowerInfoDeserError),
    BorrowerSignatures(BorrowerSignaturesDeserError),
}

impl From<BorrowerInfoDeserError> for BorrowerInfoMessageDeserError {
    fn from(error: BorrowerInfoDeserError) -> Self {
        BorrowerInfoMessageDeserError::BorrowerInfo(error)
    }
}

impl From<BorrowerSignaturesDeserError> for BorrowerInfoMessageDeserError {
    fn from(error: BorrowerSignaturesDeserError) -> Self {
        BorrowerInfoMessageDeserError::BorrowerSignatures(error)
    }
}

/// The information about the borrower.
#[non_exhaustive]
pub struct BorrowerInfo<Validation> {
    pub escrow_eph_key: PubKey<participant::Borrower, context::Escrow>,
    pub inputs: Vec<SpendableTxo>,
    pub tx_height: Height,
    pub escrow_extra_outputs: Vec<TxOut>,
    pub escrow_contract_output_position: u32,
    pub escrow_amount: bitcoin::Amount,
    pub collateral_amount_default: bitcoin::Amount,
    pub collateral_amount_liquidation: bitcoin::Amount,
    pub repayment_outputs: Vec<TxOut>,
    pub recover_outputs: Vec<TxOut>,
    pub(crate) _phantom: core::marker::PhantomData<Validation>,
}

crate::test_macros::impl_test_traits!(BorrowerInfo<Validation> where { }, escrow_eph_key, inputs, tx_height, escrow_extra_outputs, escrow_contract_output_position, escrow_amount, collateral_amount_default, collateral_amount_liquidation, repayment_outputs, recover_outputs, _phantom);

crate::test_macros::impl_arbitrary!(BorrowerInfo<Validation>, escrow_eph_key, inputs, tx_height, escrow_extra_outputs, escrow_contract_output_position, escrow_amount, collateral_amount_default, collateral_amount_liquidation, repayment_outputs, recover_outputs, _phantom);

impl<V> BorrowerInfo<V> {
    pub fn serialize(&self, out: &mut Vec<u8>) {
        use bitcoin::consensus::Encodable;

        out.push(constants::MessageId::EscrowBorrowerInfo as u8);
        self.escrow_eph_key.serialize_raw(out);
        // Note: LE bytes is the consensus encoding so if `Height` has consensus encode/decode in
        // the future we may use that.
        out.extend_from_slice(&self.tx_height.to_consensus_u32().to_le_bytes());
        out.extend_from_slice(&self.escrow_contract_output_position.to_be_bytes());
        // preparation for consensus_encode again
        out.extend_from_slice(&self.escrow_amount.to_sat().to_le_bytes());
        out.extend_from_slice(&self.collateral_amount_default.to_sat().to_le_bytes());
        out.extend_from_slice(&self.collateral_amount_liquidation.to_sat().to_le_bytes());

        out.extend_from_slice(&(self.inputs.len() as u32).to_be_bytes());
        for input in &self.inputs {
            input.serialize(out);
        }
        fn write_txouts(outputs: &[TxOut], out: &mut Vec<u8>) {
            out.extend_from_slice(&(outputs.len() as u32).to_be_bytes());
            for output in outputs {
                output.consensus_encode(out).expect("vec doesn't error");
            }
        }
        write_txouts(&self.escrow_extra_outputs, out);
        write_txouts(&self.repayment_outputs, out);
        write_txouts(&self.recover_outputs, out);
    }
}

impl BorrowerInfo<validation::Unvalidated> {
    pub fn deserialize(mut bytes: &mut &[u8]) -> Result<Self, BorrowerInfoDeserError> {
        use bitcoin::Amount;
        use bitcoin::consensus::Decodable;

        if bytes.len() < 61 {
            return Err(BorrowerInfoDeserErrorInner::UnexpectedEnd.into());
        }
        if bytes[0] != constants::MessageId::EscrowBorrowerInfo as u8 {
            return Err(BorrowerInfoDeserErrorInner::InvalidMessage(bytes[0]).into());
        }
        *bytes = &bytes[1..];
        let escrow_eph_key = PubKey::deserialize_raw(bytes)
            .map_err(BorrowerInfoDeserErrorInner::PubKey)?;
        let tx_height = deserialize::le::<u32>(bytes)?;
        let tx_height = Height::from_consensus(tx_height).map_err(BorrowerInfoDeserErrorInner::Height)?;
        let escrow_contract_output_position = deserialize::be::<u32>(bytes)?;
        let escrow_amount = Amount::from_sat(deserialize::le(bytes)?);
        let collateral_amount_default = Amount::from_sat(deserialize::le(bytes)?);
        let collateral_amount_liquidation = Amount::from_sat(deserialize::le(bytes)?);
        let inputs_count  = deserialize::be::<u32>(bytes)?;
        if inputs_count > MAX_INPUT_COUNT {
            return Err(BorrowerInfoDeserErrorInner::TooManyInputs(inputs_count).into());
        }
        let mut inputs = Vec::with_capacity(inputs_count as usize);
        for _ in 0..inputs_count {
            let txo = SpendableTxo::deserialize(bytes).map_err(BorrowerInfoDeserErrorInner::Consensus)?;
            inputs.push(txo);
        }

        fn read_txouts(bytes: &mut &[u8]) -> Result<Vec<TxOut>, BorrowerInfoDeserErrorInner> {
            if bytes.len() < 4 {
                return Err(BorrowerInfoDeserErrorInner::UnexpectedEnd);
            }
            let count  = u32::from_be_bytes(bytes[..4].try_into().expect("checked above"));
            *bytes = &bytes[4..];
            let mut vec = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let tx_out = TxOut::consensus_decode(bytes)?;
                vec.push(tx_out);
            }
            Ok(vec)
        }
        let escrow_extra_outputs = read_txouts(&mut bytes)?;
        let repayment_outputs = read_txouts(&mut bytes)?;
        let recover_outputs = read_txouts(&mut bytes)?;

        let info = BorrowerInfo {
            escrow_eph_key,
            escrow_contract_output_position,
            tx_height,
            collateral_amount_default,
            collateral_amount_liquidation,
            escrow_amount,
            inputs,
            escrow_extra_outputs,
            recover_outputs,
            repayment_outputs,
            _phantom: Default::default(),
        };
        Ok(info)
    }
}

#[derive(Debug)]
pub struct BorrowerInfoDeserError(BorrowerInfoDeserErrorInner);

impl From<deserialize::UnexpectedEnd> for BorrowerInfoDeserError {
    fn from(_: deserialize::UnexpectedEnd) -> Self {
        BorrowerInfoDeserError(BorrowerInfoDeserErrorInner::UnexpectedEnd)
    }
}

impl From<BorrowerInfoDeserErrorInner> for BorrowerInfoDeserError {
    fn from(error: BorrowerInfoDeserErrorInner) -> Self {
        BorrowerInfoDeserError(error)
    }
}

#[derive(Debug)]
enum BorrowerInfoDeserErrorInner {
    UnexpectedEnd,
    InvalidMessage(u8),
    PubKey(secp256k1::Error),
    Height(bitcoin::locktime::absolute::ConversionError),
    Consensus(bitcoin::consensus::encode::Error),
    TooManyInputs(u32),
}

impl From<bitcoin::consensus::encode::Error> for BorrowerInfoDeserErrorInner {
    fn from(error: bitcoin::consensus::encode::Error) -> Self {
        BorrowerInfoDeserErrorInner::Consensus(error)
    }
}

impl BorrowerInfo<validation::Unvalidated> {
    pub fn validate(self, escrow_params: &offer::EscrowParams) -> Result<BorrowerInfo<validation::Validated>, BorrowerInfoError> {
        // if this overflows it's also OOB
        // Not that I'd expect anyone to run this on (unsupported) 16-bit MCUs...
        let contract_pos: usize = self.escrow_contract_output_position
            .try_into()
            .map_err(|_| BorrowerInfoError::ContractPositionOob)?;
        if contract_pos > self.escrow_extra_outputs.len() {
            return Err(BorrowerInfoError::ContractPositionOob);
        }
        if self.collateral_amount_default < escrow_params.min_collateral || self.collateral_amount_liquidation < escrow_params.min_collateral {
            return Err(BorrowerInfoError::Undercollateralized);
        }
        // Note: some checks here are "missing", e.g. collateral <= escrow_amount
        // However, that doesn't matter because borrower would just get invalid transaction(s).
        // Also because of how the transactions are constructed borrower can't cause default or
        // liquidation to be invalid.
        Ok(BorrowerInfo {
            escrow_eph_key: self.escrow_eph_key,
            inputs: self.inputs,
            collateral_amount_default: self.collateral_amount_default,
            collateral_amount_liquidation: self.collateral_amount_liquidation,
            escrow_amount: self.escrow_amount,
            escrow_contract_output_position: self.escrow_contract_output_position,
            escrow_extra_outputs: self.escrow_extra_outputs,
            recover_outputs: self.recover_outputs,
            repayment_outputs: self.repayment_outputs,
            tx_height: self.tx_height,
            _phantom: Default::default(),
        })
    }
}

/// Contains all data required to compute unwrap_or_else data.
#[derive(Debug, Clone, PartialEq)]
pub struct UnsignedTransactions {
    pub(crate) borrower_eph: PubKey<participant::Borrower, context::Escrow>,
    pub(crate) multisig_leaf_hash: bitcoin::taproot::TapLeafHash,
    contract_index: u32,
    // Invariant: self.escrow_prevouts.len() == escrow.input.len()
    escrow_prevouts: Vec<TxOut>,
    pub(crate) escrow: Transaction,
    pub(crate) repayment: Transaction,
    pub(crate) default: Transaction,
    pub(crate) liquidation: Transaction,
    pub(crate) recover: Transaction,
}


impl UnsignedTransactions {
    /// For debugging 
    pub fn explain(&self) -> String {
        use core::fmt::Write;

        let mut string = String::new();
        string.push_str("The borrower is spending these inputs:\n");
        for (txin, txo) in self.escrow.input.iter().zip(&self.escrow_prevouts) {
            writeln!(string, " * {} sats from {}:{} with sequence {} and script: {}", txo.value, txin.previous_output.txid, txin.previous_output.vout, txin.sequence, txo.script_pubkey).unwrap();
        }
        string.push_str("to create these outputs:\n");
        for (i, txo) in self.escrow.output.iter().enumerate() {
            write!(string, " * {} sats to {}", txo.value, txo.script_pubkey).unwrap();
            if i == self.contract_index as usize {
                writeln!(string, " <- this is the multisig contract").unwrap();
            } else {
                string.push('\n');
            }
        }
        string.push_str("consumed by one of these:\n");
        writeln!(string, " * recover with time lock {}:", self.recover.lock_time).unwrap();
        for txo in &self.recover.output {
            writeln!(string, "    - {} sats to {}", txo.value, txo.script_pubkey).unwrap();
        }
        writeln!(string, " * repayment:").unwrap();
        for txo in &self.repayment.output {
            writeln!(string, "    - {} sats to {}", txo.value, txo.script_pubkey).unwrap();
        }
        writeln!(string, " * default:").unwrap();
        for txo in &self.default.output {
            writeln!(string, "    - {} sats to {}", txo.value, txo.script_pubkey).unwrap();
        }
        writeln!(string, " * liquidation:").unwrap();
        for txo in &self.liquidation.output {
            writeln!(string, "    - {} sats to {}", txo.value, txo.script_pubkey).unwrap();
        }
        string
    }

    pub(crate) fn serialize(&self, out: &mut Vec<u8>) {
        use bitcoin::consensus::Encodable;

        self.borrower_eph.serialize_raw(out);
        out.extend_from_slice(&self.contract_index.to_be_bytes());
        out.extend_from_slice(&(self.escrow_prevouts.len() as u32).to_be_bytes());
        for prevout in &self.escrow_prevouts {
            prevout.consensus_encode(out).expect("vec doesn't error");
        }
        self.escrow.consensus_encode(out).expect("vec doesn't error");
        self.repayment.consensus_encode(out).expect("vec doesn't error");
        self.default.consensus_encode(out).expect("vec doesn't error");
        self.liquidation.consensus_encode(out).expect("vec doesn't error");
        self.recover.consensus_encode(out).expect("vec doesn't error");
    }

    pub(crate) fn deserialize(bytes: &mut &[u8], keys: offer::TedSigPubKeys<context::Escrow>) -> Result<Self, UnsignedTransactionsDeserError> {
        use bitcoin::consensus::Decodable;

        let borrower_eph = PubKey::deserialize_raw(bytes)
            .map_err(UnsignedTransactionsDeserError::Secp256k1)?;
        let contract_index = deserialize::be::<u32>(bytes)?;
        let prevouts_len = deserialize::be::<u32>(bytes)?;
        let mut escrow_prevouts = Vec::with_capacity(prevouts_len as usize);
        for _ in 0..prevouts_len {
            let prevout = TxOut::consensus_decode(bytes)?;
            escrow_prevouts.push(prevout);
        }
        let escrow = Transaction::consensus_decode(bytes)?;
        let repayment = Transaction::consensus_decode(bytes)?;
        let default = Transaction::consensus_decode(bytes)?;
        let liquidation = Transaction::consensus_decode(bytes)?;
        let recover = Transaction::consensus_decode(bytes)?;
        let keys = keys.add_borrower_eph(borrower_eph);
        let multisig_script = keys.generate_multisig_script();
        let multisig_leaf_hash = multisig_script.tapscript_leaf_hash();
        let transactions = UnsignedTransactions {
            borrower_eph,
            contract_index,
            multisig_leaf_hash,
            escrow_prevouts,
            escrow,
            repayment,
            default,
            liquidation,
            recover,
        };
        Ok(transactions)
    }

    pub fn sign_borrower(&self, key_pair: Keypair) -> BorrowerSignatures {
        let repayment_signature = secp256k1::SECP256K1.sign_schnorr(&self.repayment_signing_data(), &key_pair);
        let default_signature = secp256k1::SECP256K1.sign_schnorr(&self.default_signing_data(), &key_pair);
        let liquidation_signature = secp256k1::SECP256K1.sign_schnorr(&self.liquidation_signing_data(), &key_pair);
        let recover_signature = secp256k1::SECP256K1.sign_schnorr(&self.recover_signing_data(), &key_pair);

        BorrowerSignatures {
            recover: recover_signature,
            repayment: repayment_signature,
            default: default_signature,
            liquidation: liquidation_signature,
        }
    }

    pub fn sign_ted_o(&self, escrow_key_pair: Keypair, prefund: Option<&super::prefund::Prefund<participant::TedO>>) -> TedOSignatures {
        let repayment_signature = secp256k1::SECP256K1.sign_schnorr(&self.repayment_signing_data(), &escrow_key_pair);
        let default_signature = secp256k1::SECP256K1.sign_schnorr(&self.default_signing_data(), &escrow_key_pair);
        let recover_signature = secp256k1::SECP256K1.sign_schnorr(&self.recover_signing_data(), &escrow_key_pair);
        let escrow = match prefund {
            Some(prefund) => self.sign_escrow(prefund),
            None => Vec::new(),
        };

        TedOSignatures {
            recover: recover_signature,
            repayment: repayment_signature,
            default: default_signature,
            escrow,
        }
    }

    pub fn sign_ted_p(&self, escrow_key_pair: Keypair, prefund: Option<&super::prefund::Prefund<participant::TedP>>) -> TedPSignatures {
        let recover_signature = secp256k1::SECP256K1.sign_schnorr(&self.recover_signing_data(), &escrow_key_pair);
        let escrow = match prefund {
            Some(prefund) => self.sign_escrow(prefund),
            None => Vec::new(),
        };

        TedPSignatures {
            recover: recover_signature,
            escrow,
        }
    }

    fn sign_escrow<P: Participant>(&self, prefund: &super::prefund::Prefund<P>) -> Vec<Signature> where P::PrefundData: super::HotKey {
        use super::HotKey;

        self.sign_escrow_external_key(prefund.participant_data.participant_key_pair(), prefund)
    }

    fn sign_escrow_external_key<P: Participant>(&self, key_pair: &Keypair, prefund: &super::prefund::Prefund<P>) -> Vec<Signature> {
        self.escrow_signing_data(prefund)
            .map(|(_, message)| secp256k1::SECP256K1.sign_schnorr(&message, &key_pair))
            .collect()
    }


    pub fn verify_borrower(&self, signatures: &BorrowerSignatures) -> Result<(), secp256k1::Error> {
        self.verify_borrower_external(self.borrower_eph.as_x_only(), signatures)
    }

    pub fn verify_borrower_external(&self, key: &XOnlyPublicKey, signatures: &BorrowerSignatures) -> Result<(), secp256k1::Error> {
        let message = self.repayment_signing_data();
        secp256k1::SECP256K1.verify_schnorr(&signatures.repayment, &message, &key)?;
        let message = self.recover_signing_data();
        secp256k1::SECP256K1.verify_schnorr(&signatures.recover, &message, &key)?;
        let message = self.default_signing_data();
        secp256k1::SECP256K1.verify_schnorr(&signatures.default, &message, &key)?;
        let message = self.liquidation_signing_data();
        secp256k1::SECP256K1.verify_schnorr(&signatures.liquidation, &message, &key)?;
        Ok(())
    }

    pub fn verify_ted_o_external(&self, key: &XOnlyPublicKey, signatures: &TedOSignatures) -> Result<(), secp256k1::Error> {
        let message = self.repayment_signing_data();
        secp256k1::SECP256K1.verify_schnorr(&signatures.repayment, &message, &key)?;
        let message = self.recover_signing_data();
        secp256k1::SECP256K1.verify_schnorr(&signatures.recover, &message, &key)?;
        let message = self.default_signing_data();
        secp256k1::SECP256K1.verify_schnorr(&signatures.default, &message, &key)?;
        Ok(())
    }

    pub fn verify_ted_p_external(&self, key: &XOnlyPublicKey, signatures: &TedPSignatures) -> Result<(), secp256k1::Error> {
        let message = self.recover_signing_data();
        secp256k1::SECP256K1.verify_schnorr(&signatures.recover, &message, &key)?;
        Ok(())
    }

    pub fn escrow_signing_data(&self, prefund: &super::prefund::Prefund<impl Participant>) -> impl '_ + Iterator<Item=(usize, secp256k1::Message)> {
        use bitcoin::sighash::{SighashCache, Prevouts, TapSighashType};

        let funding_script = prefund.funding_script();
        let leaf_script = prefund.keys.generate_multisig_script();
        let leaf_hash = leaf_script.tapscript_leaf_hash();
        let mut cache = SighashCache::new(&self.escrow);
        let prevouts = &self.escrow_prevouts;
        let prevouts = Prevouts::All(prevouts);
        self.escrow.input.iter().zip(&self.escrow_prevouts).enumerate()
            .filter(move |(_, (_, out))| out.script_pubkey == funding_script)
            .map(move |(i, (_txin, _txout))| {
                (i, cache.taproot_script_spend_signature_hash(i, &prevouts, leaf_hash, TapSighashType::Default)
                    .expect("we provided all values correctly")
                    .into())
            })
    }

    pub fn repayment_signing_data(&self) -> secp256k1::Message {
        self.signing_data_for(&self.repayment)
    }

    pub fn default_signing_data(&self) -> secp256k1::Message {
        self.signing_data_for(&self.default)
    }

    pub fn liquidation_signing_data(&self) -> secp256k1::Message {
        self.signing_data_for(&self.liquidation)
    }

    pub fn recover_signing_data(&self) -> secp256k1::Message {
        self.signing_data_for(&self.recover)
    }

    fn signing_data_for(&self, tx: &Transaction) -> secp256k1::Message {
        use bitcoin::sighash::{SighashCache, Prevouts, TapSighashType};

        // Unfortunately SigHashCache doesn't allow signing multiple transactions with same cached
        // data so we create it separately for each.
        let mut cache = SighashCache::new(tx);
        let prevout = self.escrow_output();
        let prevouts = &[prevout];
        let prevouts = Prevouts::All(prevouts);
        cache.taproot_script_spend_signature_hash(0, &prevouts, self.multisig_leaf_hash, TapSighashType::Default)
            .expect("we provided all values correctly")
            .into()
    }

    pub fn escrow_output(&self) -> &TxOut {
        &self.escrow.output[self.contract_index as usize]
    }

    #[cfg(test)]
    fn arbitrary(gen: &mut quickcheck::Gen, keys: EscrowKeys) -> Self {
        use quickcheck::Arbitrary;

        #[derive(Clone)]
        struct UnsignedTransactionsHelper {
            borrower_eph: PubKey<participant::Borrower, context::Escrow>,
            contract_index: u32,
            // Invariant: self.escrow_prevouts.len() == escrow.input.len()
            escrow_prevouts: Vec<TxOut>,
            escrow: Transaction,
            repayment: Transaction,
            default: Transaction,
            liquidation: Transaction,
            recover: Transaction,
        }

        crate::test_macros::impl_arbitrary!(UnsignedTransactionsHelper, borrower_eph, contract_index, escrow_prevouts, escrow, repayment, default, liquidation, recover);

        let helper = UnsignedTransactionsHelper::arbitrary(gen);
        let keys = keys.add_borrower_eph(helper.borrower_eph);
        let multisig_script = keys.generate_multisig_script();
        let multisig_leaf_hash = multisig_script.tapscript_leaf_hash();

        UnsignedTransactions {
            borrower_eph: helper.borrower_eph,
            multisig_leaf_hash,
            contract_index: helper.contract_index,
            escrow_prevouts: helper.escrow_prevouts,
            escrow: helper.escrow,
            repayment: helper.repayment,
            default: helper.default,
            liquidation: helper.liquidation,
            recover: helper.recover,
        }
    }
}

#[derive(Debug)]
pub(crate) enum UnsignedTransactionsDeserError {
    UnexpectedEnd,
    Secp256k1(secp256k1::Error),
    Consensus(bitcoin::consensus::encode::Error),
}

impl From<deserialize::UnexpectedEnd> for UnsignedTransactionsDeserError {
    fn from(_: deserialize::UnexpectedEnd) -> Self {
        UnsignedTransactionsDeserError::UnexpectedEnd
    }
}


impl From<bitcoin::consensus::encode::Error> for UnsignedTransactionsDeserError {
    fn from(error: bitcoin::consensus::encode::Error) -> Self {
        UnsignedTransactionsDeserError::Consensus(error)
    }
}

pub struct ReceivingEscrowSignature<P: Participant> {
    pub(crate) params: offer::EscrowParams,
    pub(crate) recover_signature: Signature,
    pub(crate) repayment_signature: Signature,
    pub(crate) keys: EscrowKeys,
    pub(crate) unsigned_txes: UnsignedTransactions,
    pub(crate) participant_data: P::PreEscrowData,
}

crate::test_macros::impl_test_traits!(ReceivingEscrowSignature<P: Participant> where { P::PreEscrowData }, params, recover_signature, repayment_signature, keys, unsigned_txes, participant_data);

#[cfg(test)]
impl<P: Participant + 'static> quickcheck::Arbitrary for ReceivingEscrowSignature<P> where P::PreEscrowData: quickcheck::Arbitrary {
    fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
        struct ReceivingEscrowSignatureHelper<P: Participant> {
            params: offer::EscrowParams,
            recover_signature: Signature,
            repayment_signature: Signature,
            keys: EscrowKeys,
            participant_data: P::PreEscrowData,
        }

        crate::test_macros::impl_test_traits!(ReceivingEscrowSignatureHelper<P: Participant> where { P::PreEscrowData }, params, recover_signature, repayment_signature, keys, participant_data);
        crate::test_macros::impl_arbitrary!(ReceivingEscrowSignatureHelper<P: Participant> where { P::PreEscrowData }, params, recover_signature, repayment_signature, keys, participant_data);

        let helper = <ReceivingEscrowSignatureHelper<P> as quickcheck::Arbitrary>::arbitrary(gen);
        let unsigned_txes = UnsignedTransactions::arbitrary(gen, helper.keys);
        ReceivingEscrowSignature {
            params: helper.params,
            recover_signature: helper.recover_signature,
            repayment_signature: helper.repayment_signature,
            keys: helper.keys,
            unsigned_txes,
            participant_data: helper.participant_data,
        }
    }
}

impl<P: Participant> ReceivingEscrowSignature<P> {
    pub fn verify_signatures(mut self, ted_o_signatures: TedOSignatures, ted_p_signatures: TedPSignatures) -> Result<SignaturesVerified<P>, (Self, SignatureVerificationError)> {
        // try { } hack
        let result = (|| {
            self.unsigned_txes.verify_ted_o_external(self.keys.ted_o.as_x_only(), &ted_o_signatures)?;
            self.unsigned_txes.verify_ted_p_external(self.keys.ted_p.as_x_only(), &ted_p_signatures)?;
            Ok(())
        })();

        // can't use `map_err` due to borrowing
        if let Err(error) = result {
            return Err((self, error));
        }

        let keys = self.keys.add_borrower_eph(self.unsigned_txes.borrower_eph);
        finalize(&mut self.unsigned_txes.recover, &keys, &self.recover_signature, &ted_o_signatures.recover, &ted_p_signatures.recover);
        let verified = SignaturesVerified {
            ted_o_signatures,
            ted_p_signatures,
            state: self,
        };
        Ok(verified)
    }

    pub fn liquidator_amount(&self) -> bitcoin::Amount {
        // We need to be pessimistic here, so we return the smaler one
        self.unsigned_txes.liquidation.output[self.params.liquidator_output_index].value.min(self.unsigned_txes.default.output[self.params.liquidator_output_index].value)
    }

    pub(crate) fn assemble_escrow<F: FnMut(secp256k1::Message) -> Result<Signature, SignatureVerificationError>>(&self, ted_o_signatures: &TedOSignatures, ted_p_signatures: &TedPSignatures, mut get_signature: F) -> Result<Transaction, SignatureVerificationError> where P::PreEscrowData: participant::PrefundData {
        use secp256k1::SECP256K1;
        use bitcoin::taproot::ControlBlock;
        use participant::PrefundData;

        let prefund = self.participant_data.prefund();
        // we have to clone due to borrowing
        let mut result = self.unsigned_txes.escrow.clone();
        let permutation = Permutation::from_keys(&prefund.keys);
        let ted_o_key = prefund.keys.ted_o.as_x_only();
        let ted_p_key = prefund.keys.ted_p.as_x_only();

        // pre-compute script and control block for faster serialization
        let script = prefund.keys.generate_multisig_script();
        let internal_key = prefund.keys.generate_internal_key();
        let merkle_branch = [prefund.borrower_return_hash].into();
        let control_block = ControlBlock {
            leaf_version: LeafVersion::TapScript,
            internal_key,
            output_key_parity: prefund.parity,
            merkle_branch,
        };
        let control_block = control_block.serialize();

        let mut ted_o_escrow_sigs = ted_o_signatures.escrow.iter();
        let mut ted_p_escrow_sigs = ted_p_signatures.escrow.iter();
        // we don't use `Iterator::zip` because that wouldn't detect fewer signatures
        for (i, message) in self.unsigned_txes.escrow_signing_data(&prefund) {
            match (ted_o_escrow_sigs.next(), ted_p_escrow_sigs.next()) {
                (Some(ted_o), Some(ted_p)) => {
                    SECP256K1.verify_schnorr(&ted_o, &message, &ted_o_key)?;
                    SECP256K1.verify_schnorr(&ted_p, &message, &ted_p_key)?;
                    let borrower = get_signature(message)?;
                    result.input[i].witness = super::assemble_witness(&borrower, ted_o, ted_p, permutation, &script, &control_block);
                },
                _ => return Err(SignatureVerificationError::MissingSignature),
            }
        }
        // Yes, there may be outstanding signatures. But what are we gonna do about them anyway? We
        // have what we wanted.
        Ok(result)
    }

    pub fn assemble_escrow_and_transition(self, ted_o_signatures: &TedOSignatures, ted_p_signatures: &TedPSignatures, get_signature: impl FnMut(secp256k1::Message) -> Result<Signature, SignatureVerificationError>) -> Result<EscrowSigned<P>, (Self, SignatureVerificationError)> where P::PreEscrowData: participant::PrefundData {
        let result = self.assemble_escrow(ted_o_signatures, ted_p_signatures, get_signature);
        match result {
            Ok(escrow) => {
                let state = EscrowSigned {
                    tx_escrow: escrow,
                    recover: self.unsigned_txes.recover,
                    participant_data: self.participant_data,
                };
                Ok(state)
            },
            Err(error) => Err((self, error)),
        }
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum SignatureVerificationError {
    InvalidSignature(secp256k1::Error),
    MissingSignature,
}

impl From<secp256k1::Error> for SignatureVerificationError {
    fn from(error: secp256k1::Error) -> Self {
        SignatureVerificationError::InvalidSignature(error)
    }
}

impl<P: Participant> super::StateData for ReceivingEscrowSignature<P> {
    const STATE_ID: constants::StateId = constants::StateId::EscrowReceivingEscrowSignatures;
    const PARTICIPANT_ID: constants::ParticipantId = P::IDENTIFIER;
}

impl<P: Participant> Serialize for ReceivingEscrowSignature<P> where P::PreEscrowData: super::Serialize {
    fn serialize(&self, out: &mut Vec<u8>) {
        // TODO: state marker
        out.extend_from_slice(self.recover_signature.as_ref());
        out.extend_from_slice(self.repayment_signature.as_ref());
        self.keys.serialize(out);
        self.params.serialize(out);
        self.unsigned_txes.serialize(out);
        self.participant_data.serialize(out);
    }
}

impl<P: Participant> super::Deserialize for ReceivingEscrowSignature<P>  where P::PreEscrowData: super::Deserialize {
    type Error = ReceivingEscrowSignatureDeserError<<P::PreEscrowData as super::Deserialize>::Error>;

    fn deserialize(bytes: &mut &[u8], version: deserialize::StateVersion) -> Result<Self, Self::Error> {
        let escrow_params_version = match version {
            deserialize::StateVersion::V0 => super::offer::EscrowParamsVersion::V0,
            deserialize::StateVersion::V1 => super::offer::EscrowParamsVersion::V1,
        };
        let recover_signature = deserialize::signature(bytes)
            .map_err(ReceivingEscrowSignatureDeserErrorInner::Secp256k1)
            .map_err(ReceivingEscrowSignatureDeserError)?;
        let repayment_signature = deserialize::signature(bytes)
            .map_err(ReceivingEscrowSignatureDeserErrorInner::Secp256k1)
            .map_err(ReceivingEscrowSignatureDeserError)?;
        let keys = offer::TedSigPubKeys::deserialize(bytes)
            .map_err(ReceivingEscrowSignatureDeserErrorInner::Keys)
            .map_err(ReceivingEscrowSignatureDeserError)?;
        let params = offer::EscrowParams::deserialize(bytes, escrow_params_version)
            .map_err(ReceivingEscrowSignatureDeserErrorInner::Params)
            .map_err(ReceivingEscrowSignatureDeserError)?;
        let unsigned_txes = UnsignedTransactions::deserialize(bytes, keys)
            .map_err(ReceivingEscrowSignatureDeserErrorInner::Txes)
            .map_err(ReceivingEscrowSignatureDeserError)?;
        let participant_data = P::PreEscrowData::deserialize(bytes, version)
            .map_err(ReceivingEscrowSignatureDeserErrorInner::Participant)
            .map_err(ReceivingEscrowSignatureDeserError)?;
        let state = ReceivingEscrowSignature {
            params,
            keys,
            unsigned_txes,
            participant_data,
            recover_signature,
            repayment_signature,
        };
        Ok(state)
    }
}

#[derive(Debug)]
pub struct ReceivingEscrowSignatureDeserError<E>(ReceivingEscrowSignatureDeserErrorInner<E>);

#[derive(Debug)]
enum ReceivingEscrowSignatureDeserErrorInner<E> {
    Secp256k1(secp256k1::Error),
    Borrower(BorrowerSignaturesDeserError),
    Keys(offer::DeserializationError),
    Params(offer::DeserializationError),
    Txes(UnsignedTransactionsDeserError),
    Participant(E),
}

pub struct SignaturesVerified<P: Participant> {
    pub(crate) ted_o_signatures: TedOSignatures,
    pub(crate) ted_p_signatures: TedPSignatures,
    pub(crate) state: ReceivingEscrowSignature<P>,
}

impl<P: Participant> SignaturesVerified<P> {
    pub fn recover_tx(&self) -> &Transaction {
        // despite the name, this transaction is now signed
        &self.state.unsigned_txes.recover
    }

    pub fn network(&self) -> bitcoin::Network {
        self.state.params.network
    }

    pub fn tweaked_key(&self) -> bitcoin::key::TweakedPublicKey {
        let keys = self.state.keys.add_borrower_eph(self.state.unsigned_txes.borrower_eph);
        output_spend_info(&keys).0.output_key()
    }

    pub fn liquidator_amount(&self) -> bitcoin::Amount {
        self.state.liquidator_amount()
    }

    pub fn escrow_output(&self) -> &TxOut {
        self.state.unsigned_txes.escrow_output()
    }

    pub fn assemble_escrow_custom(mut self, get_signature: impl FnMut(secp256k1::Message) -> Result<Signature, SignatureVerificationError>) -> Result<EscrowSigned<P>, (Self, SignatureVerificationError)> where P::PreEscrowData: participant::PrefundData {
        let result = self.state.assemble_escrow_and_transition(&self.ted_o_signatures, &self.ted_p_signatures, get_signature);
        match result {
            Ok(state) => Ok(state),
            Err((old_state, error)) => {
                self.state = old_state;
                Err((self, error))
            }
        }
    }

    pub fn participant_data(&self) -> &P::PreEscrowData {
        &self.state.participant_data
    }
}

impl<P: Participant> super::StateData for SignaturesVerified<P> {
    const PARTICIPANT_ID: constants::ParticipantId = P::IDENTIFIER;
    const STATE_ID: constants::StateId = constants::StateId::EscrowSignaturesVerified;
}

impl<P: Participant> Serialize for SignaturesVerified<P> where P::PreEscrowData: Serialize {
    fn serialize(&self, buf: &mut Vec<u8>) {
        self.state.serialize(buf);
        self.ted_o_signatures.serialize(buf);
        self.ted_p_signatures.serialize(buf);
    }
}

impl<P: Participant> Deserialize for SignaturesVerified<P> where P::PreEscrowData: Deserialize {
    type Error = SignaturesVerifiedDeserError<<P::PreEscrowData as Deserialize>::Error>;

    fn deserialize(bytes: &mut &[u8], version: deserialize::StateVersion) -> Result<Self, Self::Error> {
        let state = ReceivingEscrowSignature::deserialize(bytes, version).map_err(SignaturesVerifiedDeserErrorInner::State)?;
        let ted_o_signatures = TedOSignatures::deserialize(bytes).map_err(SignaturesVerifiedDeserErrorInner::TedOSignatures)?;
        let ted_p_signatures = TedPSignatures::deserialize(bytes).map_err(SignaturesVerifiedDeserErrorInner::TedPSignatures)?;
        Ok(SignaturesVerified {
            state,
            ted_o_signatures,
            ted_p_signatures,
        })
    }
}

crate::test_macros::impl_test_traits!(SignaturesVerified<P: Participant> where { P::PreEscrowData }, state, ted_o_signatures, ted_p_signatures);

#[derive(Debug)]
pub struct SignaturesVerifiedDeserError<E>(SignaturesVerifiedDeserErrorInner<E>);

#[derive(Debug)]
enum SignaturesVerifiedDeserErrorInner<E> {
    State(ReceivingEscrowSignatureDeserError<E>),
    TedOSignatures(TedOSignaturesDeserError),
    TedPSignatures(TedPSignaturesDeserError),
}

impl<E> From<SignaturesVerifiedDeserErrorInner<E>> for SignaturesVerifiedDeserError<E> {
    fn from(error: SignaturesVerifiedDeserErrorInner<E>) -> Self {
        SignaturesVerifiedDeserError(error)
    }
}

pub struct EscrowSigned<P: Participant> {
    /// The transaction moving satoshis from prefund to escrow.
    pub(crate) tx_escrow: Transaction,

    /// The presigned recovery transaction.
    pub recover: Transaction,

    /// Data relevant only to the specific participant.
    pub participant_data: P::PreEscrowData,
}

crate::test_macros::impl_test_traits!(EscrowSigned<P: Participant> where { P::PreEscrowData }, tx_escrow, recover, participant_data);
crate::test_macros::impl_arbitrary!(EscrowSigned<P: Participant> where { P::PreEscrowData }, tx_escrow, recover, participant_data);

impl<P: Participant> EscrowSigned<P> {
    /// Returns the transaction moving satoshis from prefund to escrow.
    pub fn tx_escrow(&self) -> &Transaction {
        &self.tx_escrow
    }
}

impl<P: Participant> super::StateData for EscrowSigned<P> where P::PreEscrowData: super::Serialize {
    const STATE_ID: constants::StateId = constants::StateId::WaitingForEscrowConfirmation;
    const PARTICIPANT_ID: constants::ParticipantId = P::IDENTIFIER;
}
impl<P: Participant> super::Serialize for EscrowSigned<P> where P::PreEscrowData: super::Serialize {
    fn serialize(&self, out: &mut Vec<u8>) {
        use bitcoin::consensus::Encodable;

        self.tx_escrow.consensus_encode(out).expect("vec doesn't error");
        self.recover.consensus_encode(out).expect("vec doesn't error");
        self.participant_data.serialize(out);
    }
}

impl<P: Participant> super::Deserialize for EscrowSigned<P> where P::PreEscrowData: super::Deserialize {
    type Error = EscrowSignedDeserError<<P::PreEscrowData as super::Deserialize>::Error>;

    fn deserialize(bytes: &mut &[u8], version: deserialize::StateVersion) -> std::result::Result<Self, Self::Error> {
        use bitcoin::consensus::Decodable;

        let tx_escrow = Transaction::consensus_decode(bytes).map_err(EscrowSignedDeserErrorInner::Escrow)?;
        let recover = Transaction::consensus_decode(bytes).map_err(EscrowSignedDeserErrorInner::Recover)?;
        let participant_data = P::PreEscrowData::deserialize(bytes, version).map_err(EscrowSignedDeserErrorInner::Participant)?;
        Ok(EscrowSigned {
            tx_escrow,
            recover,
            participant_data,
        })
    }
}

#[derive(Debug)]
pub struct EscrowSignedDeserError<E>(EscrowSignedDeserErrorInner<E>);

impl<E> From<EscrowSignedDeserErrorInner<E>> for EscrowSignedDeserError<E> {
    fn from(error: EscrowSignedDeserErrorInner<E>) -> Self {
        EscrowSignedDeserError(error)
    }
}

#[derive(Debug)]
pub enum EscrowSignedDeserErrorInner<E> {
    Escrow(bitcoin::consensus::encode::Error),
    Recover(bitcoin::consensus::encode::Error),
    Participant(E),
}

/*
impl<P: Participant> EscrowSigned<P> where P::PreEscrowData: super::HotKey {
    pub fn sign_liquidation(&self) -> Transaction {
    }
}
*/

pub(crate) fn finalize(tx: &mut Transaction, keys: &PubKeys<context::Escrow>, borrower: &Signature, ted_o: &Signature, ted_p: &Signature) {
    use bitcoin::taproot::ControlBlock;

    let (_, _, parity) = output_script(&keys);
    let script = keys.generate_multisig_script();
    let internal_key = keys.generate_internal_key();
    let merkle_branch = (&[] as &[_])
        .try_into()
        .expect("0 < 128");
    let control_block = ControlBlock {
        leaf_version: LeafVersion::TapScript,
        internal_key,
        output_key_parity: parity,
        merkle_branch,
    };
    let control_block = control_block.serialize();
    let permutation = Permutation::from_keys(&keys);
    tx.input[0].witness = super::assemble_witness(borrower, ted_o, ted_p, permutation, &script, &control_block);
}

#[derive(Debug, Clone, PartialEq)]
pub struct BorrowerSignatures {
    /// The signature of the recovery transaction
    pub recover: Signature,

    /// The signature of the repayment transaction
    pub repayment: Signature,

    /// The signature of the default transaction
    pub default: Signature,

    /// The signature of the liquidation transaction
    pub liquidation: Signature,
}

crate::test_macros::impl_arbitrary!(BorrowerSignatures, recover, repayment, default, liquidation);

impl BorrowerSignatures {
    pub fn serialize(&self, out: &mut Vec<u8>) {
        // Warning: The order of these must stay fixed forever!
        out.reserve(1 + 4 * 64);
        out.push(constants::MessageId::StateSigsFromBorrower as u8);
        out.extend_from_slice(self.recover.as_ref());
        out.extend_from_slice(self.repayment.as_ref());
        out.extend_from_slice(self.default.as_ref());
        out.extend_from_slice(self.liquidation.as_ref());
    }

    pub fn deserialize(bytes: &mut &[u8]) -> Result<Self, BorrowerSignaturesDeserError> {
        fn read_signature(bytes: &mut &[u8]) -> Result<Signature, BorrowerSignaturesDeserErrorInner> {
            deserialize::signature(bytes).map_err(Into::into)
        }

        if bytes.len() < 1 + 4 * 64 {
            return Err(BorrowerSignaturesDeserErrorInner::UnexpectedEnd.into());
        }

        if bytes[0] != constants::MessageId::StateSigsFromBorrower as u8 {
            return Err(BorrowerSignaturesDeserError(BorrowerSignaturesDeserErrorInner::InvalidMessage(bytes[0])));
        }

        *bytes = &bytes[1..];
        let recover = read_signature(bytes)?;
        let repayment = read_signature(bytes)?;
        let default = read_signature(bytes)?;
        let liquidation = read_signature(bytes)?;

        let signatures = BorrowerSignatures {
            recover,
            repayment,
            default,
            liquidation,
        };

        Ok(signatures)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TedOSignatures {
    pub recover: Signature,
    pub repayment: Signature,
    pub default: Signature,
    pub escrow: Vec<Signature>,
}

crate::test_macros::impl_arbitrary!(TedOSignatures, recover, repayment, default, escrow);

impl TedOSignatures {
    pub fn serialize(&self, out: &mut Vec<u8>) {
        out.reserve((self.escrow.len() + 3) * 64);
        out.push(constants::MessageId::StateSigsFromTedO as u8);
        out.extend_from_slice(self.recover.as_ref());
        out.extend_from_slice(self.repayment.as_ref());
        out.extend_from_slice(self.default.as_ref());
        out.extend_from_slice(&(self.escrow.len() as u32).to_be_bytes());
        for signature in &self.escrow {
            out.extend_from_slice(signature.as_ref());
        }
    }

    pub fn deserialize(bytes: &mut &[u8]) -> Result<Self, TedOSignaturesDeserError> {
        if bytes.len() < 3 * 64 + 4 {
            return Err(TedOSignaturesDeserError(TedXSignaturesDeserErrorInner::UnexpectedEnd));
        }
        if bytes[0] != constants::MessageId::StateSigsFromTedO as u8 {
            return Err(TedOSignaturesDeserError(TedXSignaturesDeserErrorInner::InvalidMessage(bytes[0])));
        }
        *bytes = &bytes[1..];
        let recover = deserialize::signature(bytes)
            .map_err(TedXSignaturesDeserErrorInner::Secp256k1)?;
        let repayment = deserialize::signature(bytes)
            .map_err(TedXSignaturesDeserErrorInner::Secp256k1)?;
        let default = deserialize::signature(bytes)
            .map_err(TedXSignaturesDeserErrorInner::Secp256k1)?;
        let len = deserialize::be::<u32>(bytes)?;
        // One signature per input
        if len > MAX_INPUT_COUNT {
            return Err(TedXSignaturesDeserErrorInner::TooManySignatures(len).into());
        }
        let len = len as usize;
        let mut escrow = Vec::with_capacity(len);
        for _ in 0..len {
            let signature = deserialize::signature(bytes)
                .map_err(TedXSignaturesDeserErrorInner::Secp256k1)?;
            escrow.push(signature);
        }
        let signatures = TedOSignatures {
            recover,
            repayment,
            default,
            escrow,
        };
        Ok(signatures)
    }
}

#[derive(Debug)]
pub struct TedOSignaturesDeserError(TedXSignaturesDeserErrorInner);

impl From<deserialize::UnexpectedEnd> for TedOSignaturesDeserError {
    fn from(_: deserialize::UnexpectedEnd) -> Self {
        TedOSignaturesDeserError(TedXSignaturesDeserErrorInner::UnexpectedEnd)
    }
}

impl From<TedXSignaturesDeserErrorInner> for TedOSignaturesDeserError {
    fn from(error: TedXSignaturesDeserErrorInner) -> Self {
        TedOSignaturesDeserError(error)
    }
}

impl From<TedXSignaturesDeserErrorInner> for TedPSignaturesDeserError {
    fn from(error: TedXSignaturesDeserErrorInner) -> Self {
        TedPSignaturesDeserError(error)
    }
}

#[derive(Debug)]
enum TedXSignaturesDeserErrorInner {
    UnexpectedEnd,
    InvalidMessage(u8),
    Secp256k1(secp256k1::Error),
    TooManySignatures(u32),
}

#[derive(Debug)]
pub struct TedPSignaturesDeserError(TedXSignaturesDeserErrorInner);

#[derive(Debug, Clone, PartialEq)]
pub struct TedPSignatures {
    pub recover: Signature,
    pub escrow: Vec<Signature>,
}

crate::test_macros::impl_arbitrary!(TedPSignatures, recover, escrow);

impl TedPSignatures {
    pub fn serialize(&self, out: &mut Vec<u8>) {
        out.reserve((self.escrow.len() + 3) * 64);
        out.push(constants::MessageId::StateSigsFromTedP as u8);
        out.extend_from_slice(self.recover.as_ref());
        out.extend_from_slice(&(self.escrow.len() as u32).to_be_bytes());
        for signature in &self.escrow {
            out.extend_from_slice(signature.as_ref());
        }
    }

    pub fn deserialize(bytes: &mut &[u8]) -> Result<Self, TedPSignaturesDeserError> {
        if bytes.len() < 1 * 64 + 4 {
            return Err(TedPSignaturesDeserError(TedXSignaturesDeserErrorInner::UnexpectedEnd));
        }
        if bytes[0] != constants::MessageId::StateSigsFromTedP as u8 {
            return Err(TedPSignaturesDeserError(TedXSignaturesDeserErrorInner::InvalidMessage(bytes[0])));
        }
        *bytes = &bytes[1..];
        let recover = deserialize::signature(bytes)
            .map_err(TedXSignaturesDeserErrorInner::Secp256k1)?;
        let len = deserialize::be::<u32>(bytes)?;
        // One signature per input
        if len > MAX_INPUT_COUNT {
            return Err(TedXSignaturesDeserErrorInner::TooManySignatures(len).into());
        }
        let len = len as usize;
        let mut escrow = Vec::with_capacity(len);
        for _ in 0..len {
            let signature = deserialize::signature(bytes)
                .map_err(TedXSignaturesDeserErrorInner::Secp256k1)?;
            escrow.push(signature);
        }
        let signatures = TedPSignatures {
            recover,
            escrow,
        };
        Ok(signatures)
    }
}

#[derive(Debug)]
pub struct BorrowerSignaturesDeserError(BorrowerSignaturesDeserErrorInner);

impl From<deserialize::UnexpectedEnd> for TedPSignaturesDeserError {
    fn from(_: deserialize::UnexpectedEnd) -> Self {
        TedPSignaturesDeserError(TedXSignaturesDeserErrorInner::UnexpectedEnd)
    }
}

impl From<BorrowerSignaturesDeserErrorInner> for BorrowerSignaturesDeserError {
    fn from(error: BorrowerSignaturesDeserErrorInner) -> Self {
        BorrowerSignaturesDeserError(error)
    }
}

#[derive(Debug)]
enum BorrowerSignaturesDeserErrorInner {
    UnexpectedEnd,
    InvalidMessage(u8),
    Secp256k1(secp256k1::Error),
}

impl From<secp256k1::Error> for BorrowerSignaturesDeserErrorInner {
    fn from(error: secp256k1::Error) -> Self {
        BorrowerSignaturesDeserErrorInner::Secp256k1(error)
    }
}

#[derive(Debug)]
pub enum BorrowerInfoError {
    ContractPositionOob,
    Undercollateralized,
}

pub(crate) fn output_spend_info(keys: &PubKeys<context::Escrow>) -> (TaprootSpendInfo, TapLeafHash) {
    let multisig_script = keys.generate_multisig_script();
    let multisig_leaf_hash = multisig_script.tapscript_leaf_hash();
    // If there's a single leaf it's also the root
    // see https://github.com/rust-bitcoin/rust-bitcoin/issues/1393
    let root = TapNodeHash::from(multisig_leaf_hash);
    let internal_key = keys.generate_internal_key();
    let spend_info = TaprootSpendInfo::new_key_spend(secp256k1::SECP256K1, internal_key, Some(root));

    (spend_info, multisig_leaf_hash)
}

pub(crate) fn output_script(keys: &PubKeys<context::Escrow>) -> (ScriptBuf, TapLeafHash, secp256k1::Parity) {
    let (spend_info, multisig_leaf_hash) = output_spend_info(keys);

    let parity = spend_info.output_key_parity();
    let output_script = ScriptBuf::new_p2tr_tweaked(spend_info.output_key());
    (output_script, multisig_leaf_hash, parity)
}

#[derive(Debug, Clone, PartialEq)]
pub enum TedSignatures {
    TedO(TedOSignatures),
    TedP(TedPSignatures),
}

impl TedSignatures {
    pub fn serialize(&self, buf: &mut Vec<u8>) {
        match self {
            TedSignatures::TedO(message) => message.serialize(buf),
            TedSignatures::TedP(message) => message.serialize(buf),
        }
    }

    pub fn deserialize(bytes: &mut &[u8]) -> Result<Option<Self>, TedSignaturesDeserError> {
        use super::constants::MessageId;
        use core::convert::TryFrom;

        match bytes.first() {
            None => Ok(None),
            Some(message_id) => {
                match MessageId::try_from(*message_id).map_err(|_| TedSignaturesDeserErrorInner::InvalidMessageId(*message_id))? {
                    MessageId::StateSigsFromTedO => Ok(Some(TedSignatures::TedO(TedOSignatures::deserialize(bytes).map_err(TedSignaturesDeserErrorInner::TedO)?))),
                    MessageId::StateSigsFromTedP => Ok(Some(TedSignatures::TedP(TedPSignatures::deserialize(bytes).map_err(TedSignaturesDeserErrorInner::TedP)?))),
                    _ => Err(TedSignaturesDeserErrorInner::InvalidMessageId(*message_id).into()),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct BroadcastRequest {
    pub signatures: Vec<Signature>,
}

impl BroadcastRequest {
    pub fn deserialize(bytes: &mut &[u8]) -> Result<Self, BroadcastRequestDeserError> {
        let message_id = bytes.first().ok_or(BroadcastRequestDeserErrorInner::UnexpectedEnd)?;
        if *message_id != constants::MessageId::EscrowSigsFromBorrower as u8 {
            return Err(BroadcastRequestDeserErrorInner::InvalidMessageId(*message_id).into());
        }
        *bytes = &bytes[1..];
        let len = deserialize::be::<u32>(bytes)? as usize;
        let mut signatures = Vec::with_capacity(len);
        for _ in 0..len {
            let sig = deserialize::signature(bytes).map_err(BroadcastRequestDeserErrorInner::InvalidSignature)?;
            signatures.push(sig);
        }
        Ok(BroadcastRequest { signatures })
    }
}

#[derive(Debug)]
pub struct BroadcastRequestDeserError(BroadcastRequestDeserErrorInner);

#[derive(Debug)]
enum BroadcastRequestDeserErrorInner {
    UnexpectedEnd,
    InvalidMessageId(u8),
    InvalidSignature(secp256k1::Error)
}

impl From<BroadcastRequestDeserErrorInner> for BroadcastRequestDeserError {
    fn from(error: BroadcastRequestDeserErrorInner) -> Self {
        BroadcastRequestDeserError(error)
    }
}

impl From<deserialize::UnexpectedEnd> for BroadcastRequestDeserError {
    fn from(_: deserialize::UnexpectedEnd) -> Self {
        BroadcastRequestDeserErrorInner::UnexpectedEnd.into()
    }
}

#[cfg(test)]
impl quickcheck::Arbitrary for TedSignatures {
    fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
        if bool::arbitrary(gen) {
            TedSignatures::TedO(TedOSignatures::arbitrary(gen))
        } else {
            TedSignatures::TedP(TedPSignatures::arbitrary(gen))
        }
    }
}

#[derive(Debug)]
pub struct TedSignaturesDeserError(TedSignaturesDeserErrorInner);

impl From<TedSignaturesDeserErrorInner> for TedSignaturesDeserError {
    fn from(error: TedSignaturesDeserErrorInner) -> Self {
        TedSignaturesDeserError(error)
    }
}

#[derive(Debug)]
enum TedSignaturesDeserErrorInner {
    InvalidMessageId(u8),
    TedO(TedOSignaturesDeserError),
    TedP(TedPSignaturesDeserError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::Deserialize;

    crate::test_macros::check_roundtrip_with_version!(roundtrip_receiving_borrower_info, ReceivingBorrowerInfo<participant::Borrower>);
    crate::test_macros::check_roundtrip_with_version!(roundtrip_waiting_for_escrow_confirmation, WaitingForEscrowConfirmation<participant::Borrower>);
    crate::test_macros::check_roundtrip_with_version!(roundtrip_receiving_escrow_signature, ReceivingEscrowSignature<participant::Borrower>);
    crate::test_macros::check_roundtrip!(roundtrip_borrower_info, BorrowerInfo<validation::Unvalidated>);
    crate::test_macros::check_roundtrip!(roundtrip_borrower_signatures, BorrowerSignatures);
    crate::test_macros::check_roundtrip!(roundtrip_ted_o_signatures, TedOSignatures);
    crate::test_macros::check_roundtrip!(roundtrip_ted_p_signatures, TedPSignatures);
}
