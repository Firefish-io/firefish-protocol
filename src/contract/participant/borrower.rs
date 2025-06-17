use bitcoin::{Transaction, Sequence, OutPoint, Script, ScriptBuf, Address, TxOut, Amount};
use bitcoin::locktime::absolute::{LockTime, Height};
use bitcoin::key::Keypair;
use bitcoin::blockdata::{Weight, FeeRate};
use bitcoin::blockdata::transaction::InputWeightPrediction;
use core::convert::{TryFrom, TryInto};
use super::super::{prefund, escrow, context, deserialize};
use super::super::offer::{self, Offer};
use super::super::pub_keys::PubKey;
use super::super::constants;
use secp256k1::SECP256K1;

use crate::contract::primitives::SpendableTxo;

#[derive(Debug, PartialEq, Clone)]
#[non_exhaustive]
pub struct PrefundData {
    key_pair: Keypair,
    prefund_lock_time: Sequence,
}

crate::test_macros::impl_arbitrary!(PrefundData, key_pair, prefund_lock_time);

impl PrefundData {
    pub(crate) fn borrower_key_and_leaf_script(&self) -> (PubKey<super::Borrower, context::Prefund>, ScriptBuf) {
        let pub_key = PubKey::from_key_pair(&self.key_pair);
        let tapscript = pub_key.borrower_prefund_script(self.prefund_lock_time);
        (pub_key, tapscript)
    }
}

impl super::super::HotKey for PrefundData {
    fn participant_key_pair(&self) -> &Keypair {
        &self.key_pair
    }
}

impl super::super::Serialize for PrefundData {
    fn serialize(&self, out: &mut Vec<u8>) {
        use bitcoin::consensus::Encodable;

        out.extend_from_slice(&self.key_pair.secret_bytes());
        self.prefund_lock_time.consensus_encode(out).expect("vec doesn't error");
    }
}

impl super::super::Deserialize for PrefundData {
    type Error = PrefundDataDeserError;

    fn deserialize(bytes: &mut &[u8], version: deserialize::StateVersion) -> std::result::Result<Self, Self::Error> {
        use bitcoin::consensus::Decodable;

        match version {
            deserialize::StateVersion::V0 => (),
            deserialize::StateVersion::V1 => (),
        }
        if bytes.len() < 36 {
            return Err(PrefundDataDeserError(PrefundDataDeserErrorInner::UnexpectedEnd));
        }
        let key_pair = Keypair::from_seckey_slice(SECP256K1, &bytes[..32])
            .map_err(PrefundDataDeserErrorInner::Secp256k1)
            .map_err(PrefundDataDeserError)?;
        *bytes = &bytes[32..];
        let sequence = Sequence::consensus_decode(bytes).expect("length was checked");
        Ok(PrefundData { key_pair, prefund_lock_time: sequence })
    }
}

#[derive(Debug)]
pub struct PrefundDataDeserError(PrefundDataDeserErrorInner);

#[derive(Debug)]
enum PrefundDataDeserErrorInner {
    UnexpectedEnd,
    Secp256k1(secp256k1::Error),
}

#[derive(PartialEq, Clone, Debug)]
pub struct EscrowData {
    prefund: prefund::Prefund<super::Borrower>,
    return_script: ScriptBuf,
}

impl super::PrefundData for EscrowData {
    type Participant = super::Borrower;

    fn prefund(&self) -> &prefund::Prefund<Self::Participant> {
        &self.prefund
    }
}

crate::test_macros::impl_arbitrary!(EscrowData, prefund, return_script);

impl super::super::Serialize for EscrowData {
    fn serialize(&self, out: &mut Vec<u8>) {
        use bitcoin::consensus::Encodable;

        out.push(constants::state_id::BORROWER_ESCROW_DATA);
        self.return_script.consensus_encode(out).expect("vec doesn't error");
        self.prefund.serialize(out);
    }
}

impl super::super::Deserialize for EscrowData {
    type Error = EscrowDataDeserError;

    fn deserialize(bytes: &mut &[u8], version: deserialize::StateVersion) -> Result<Self, Self::Error> {
        use bitcoin::consensus::Decodable;

        if bytes.len() < 1 {
            return Err(EscrowDataDeserErrorInner::UnexpectedEnd.into());
        }
        if bytes[0] != constants::state_id::BORROWER_ESCROW_DATA {
            return Err(EscrowDataDeserErrorInner::InvalidState(bytes[0]).into());
        }
        *bytes = &bytes[1..];
        let return_script = ScriptBuf::consensus_decode(bytes).map_err(EscrowDataDeserErrorInner::Consensus)?;
        let prefund = prefund::Prefund::deserialize(bytes, version).map_err(EscrowDataDeserErrorInner::Prefund)?;

        Ok(EscrowData {
            prefund,
            return_script,
        })
    }
}

#[derive(Debug)]
pub struct EscrowDataDeserError(EscrowDataDeserErrorInner);

impl From<EscrowDataDeserErrorInner> for EscrowDataDeserError {
    fn from(error: EscrowDataDeserErrorInner) -> Self {
        EscrowDataDeserError(error)
    }
}

#[derive(Debug)]
enum EscrowDataDeserErrorInner {
    UnexpectedEnd,
    InvalidState(u8),
    Consensus(bitcoin::consensus::encode::Error),
    Prefund(<prefund::Prefund<super::Borrower> as super::super::Deserialize>::Error),
}

/// A convenient alias for [`WaitingForFunding::new`]
pub fn init_prefund(offer: Offer, params: PrefundParams) -> WaitingForFunding {
    WaitingForFunding::new(offer, params)
}

#[derive(Debug, Clone, PartialEq)]
pub struct WaitingForFunding {
    escrow: escrow::ReceivingBorrowerInfo<super::Borrower>,
}

crate::test_macros::impl_arbitrary!(WaitingForFunding, escrow);

impl WaitingForFunding {
    pub fn new(offer: Offer, params: PrefundParams) -> Self {
        use bitcoin::taproot::LeafVersion;

        let prefund = PrefundData {
            key_pair: params.mandatory.key_pair,
            prefund_lock_time: params.mandatory.lock_time,
        };
        let (pub_key, tapscript) = prefund.borrower_key_and_leaf_script();
        let receiver = prefund::ReceivingBorrowerInfo::with_participant_data(offer.prefund_keys, offer.escrow.network, prefund);
        let leaf_hash = bitcoin::sighash::ScriptPath::new(&tapscript, LeafVersion::TapScript)
            .leaf_hash();
        let borrower_info = prefund::BorrowerSpendInfo {
            key: pub_key,
            return_hash: leaf_hash.into(),
        };
        let prefund = receiver.borrower_info_received(SECP256K1, borrower_info);

        let escrow_data = EscrowData {
            prefund,
            return_script: params.mandatory.return_script,
        };
        let escrow = escrow::ReceivingBorrowerInfo::with_participant_data(offer.escrow, offer.escrow_keys, escrow_data);
        WaitingForFunding {
            escrow,
        }
    }

    fn from_escrow_data_and_offer(escrow_data: EscrowData, offer: Offer) -> Self {
        let escrow = escrow::ReceivingBorrowerInfo::with_participant_data(offer.escrow, offer.escrow_keys, escrow_data);
        WaitingForFunding {
            escrow,
        }
    }

    pub fn borrower_info(&self) -> prefund::BorrowerSpendInfo {
        self.escrow.participant_data.prefund.borrower_info()
    }

    pub fn network(&self) -> bitcoin::Network {
        self.escrow.params.network
    }

    pub fn funding_address(&self) -> Address {
        let data = &self.escrow.participant_data;
        data.prefund.funding_address()
    }

    pub fn liquidator_amount(&self) -> Amount {
        self.escrow.params.min_collateral
    }

    pub fn funding_received(self, funding: Funding, message: &mut Vec<u8>) -> Result<escrow::ReceivingEscrowSignature<super::Borrower>, (Self, FundingError)> {
        let escrow_data = &self.escrow.participant_data;
        let prefund = &escrow_data.prefund;

        let funding_script = prefund.funding_script();
        let eph_key_pair = Keypair::new_global(&mut rand::thread_rng());
        let eph_pubkey = PubKey::new(eph_key_pair.x_only_public_key().0);
        //let escrow_output = escrow.escrow_output(eph_pubkey);

        let mut max_lock_height = Height::from_consensus(0).expect("zero blocks is valid height");
        let txos = extract_spendable_outputs(funding.mandatory.transactions, &mut max_lock_height, |script| *script == funding_script);

        if txos.is_empty() {
            let error = FundingError {
                reason: FundingErrorReason::NoMatchingOutputs,
            };
            return Err((self, error));
        }

        // We can't simply instantiate `UnsignedTransactions` and call `size()` on each because
        // they don't have the witnesses filled so the calulation would be wrong.
        // Thus we have to predict fees based on expected sizes.
        // In case of prefund there's an exact, known size.
        let prefund_witness_elem_sizes = &[
            64, // len of signature1
            64, // len of signature2
            64, // len of signature3
                  33  // len of push_x_only_key (1 instr + 32 B data)
                +  1  // len of OP_CHECKSIGVERIFY
                + 33  // len of push_x_only_key (1 instr + 32 B data)
                +  1  // len of OP_CHECKSIGVERIFY
                + 33  // len of push_x_only_key (1 instr + 32 B data)
                +  1, // len of OP_CHECKSIG
                  33  // base len of control block
                + 32  // len of the hash hiding the borrower conditions
        ];
        let prefund_spend_input_prediction = InputWeightPrediction::new(0, prefund_witness_elem_sizes.iter().copied());

        let escrow_witness_elem_sizes = &[
            64, // len of signature1
            64, // len of signature2
            64, // len of signature3
                  33  // len of push_x_only_key (1 instr + 32 B data)
                +  1  // len of OP_CHECKSIGVERIFY
                + 33  // len of push_x_only_key (1 instr + 32 B data)
                +  1  // len of OP_CHECKSIGVERIFY
                + 33  // len of push_x_only_key (1 instr + 32 B data)
                +  1, // len of OP_CHECKSIG
                  33  // base len of control block
                      // note: there's only one script so no other nodes
        ];
        let escrow_spend_input_prediction = InputWeightPrediction::new(0, escrow_witness_elem_sizes.iter().copied());

        // witness version (1B) + OP_PUSHBYTES_32 + x-only key (32 B)
        let escrow_out_script_lengths = core::iter::once(1 + 1 + 32)
            .chain(funding.escrow_extra_outputs.iter().map(|txout| txout.script_pubkey.len()));
        let escrow_weight = predict_tx_weight(txos.len(), prefund_spend_input_prediction, escrow_out_script_lengths);
        let repayment_out_script_lengths = core::iter::once(escrow_data.return_script.len())
            .chain(funding.repayment_extra_outputs.iter().map(|txout| txout.script_pubkey.len()));
        let repayment_weight = predict_tx_weight(1, escrow_spend_input_prediction, repayment_out_script_lengths);
        let recover_out_script_lengths = core::iter::once(escrow_data.return_script.len())
            .chain(funding.recover_extra_outputs.iter().map(|txout| txout.script_pubkey.len()));
        let recover_weight = predict_tx_weight(1, escrow_spend_input_prediction, recover_out_script_lengths);
        let default_out_script_lengths = self.escrow.params.extra_termination_outputs.iter()
            .map(|txout| txout.script_pubkey.len())
            .chain(core::iter::once(self.escrow.params.liquidator_script_default.len()));
        let liquidation_out_script_lengths = self.escrow.params.extra_termination_outputs.iter()
            .map(|txout| txout.script_pubkey.len())
            .chain(core::iter::once(self.escrow.params.liquidator_script_liquidation.len()));
        let default_weight = predict_tx_weight(1, escrow_spend_input_prediction, default_out_script_lengths);
        let liquidation_weight = predict_tx_weight(1, escrow_spend_input_prediction, liquidation_out_script_lengths);
        let escrow_funding_amount = sum_txouts_amount(txos.iter().map(|txo| &txo.tx_out));
        let escrow_extra_amount = sum_txouts_amount(&funding.escrow_extra_outputs);

        let escrow_fee = escrow_weight * funding.mandatory.escrow_fee_rate;
        let repayment_fee = repayment_weight * funding.mandatory.finalization_fee_rate;
        let recover_fee = recover_weight * funding.mandatory.finalization_fee_rate;
        let default_fee = default_weight * funding.mandatory.finalization_fee_rate;
        let liquidation_fee = liquidation_weight * funding.mandatory.finalization_fee_rate;

        let termination_extra_amount = sum_txouts_amount(&self.escrow.params.extra_termination_outputs);
        let collateral = termination_extra_amount + self.escrow.params.min_collateral;
        let repayment_extra_amount = sum_txouts_amount(&funding.repayment_extra_outputs);
        let recover_extra_amount = sum_txouts_amount(&funding.recover_extra_outputs);

        let required_escrow_amount = *[repayment_fee + repayment_extra_amount, recover_fee + recover_extra_amount, default_fee + collateral, liquidation_fee + collateral]
            .iter().max().expect("non-empty array");
        let escrow_cost = escrow_fee + escrow_extra_amount;
        let required_funding_amount = required_escrow_amount + escrow_cost;
        if escrow_funding_amount < required_funding_amount {
            return Err((self, FundingError { reason: FundingErrorReason::Underfunded { required: required_funding_amount, available: escrow_funding_amount }}));
        }
        let escrow_amount = escrow_funding_amount - escrow_cost;
        let recover_txout = TxOut {
            value: escrow_amount - recover_fee - recover_extra_amount,
            script_pubkey: escrow_data.return_script.clone(),
        };
        let mut recover_outputs = funding.recover_extra_outputs;
        recover_outputs.push(recover_txout);
        let repayment_txout = TxOut {
            value: escrow_amount - repayment_fee - repayment_extra_amount,
            script_pubkey: escrow_data.return_script.clone(),
        };
        let mut repayment_outputs = funding.repayment_extra_outputs;
        repayment_outputs.push(repayment_txout);

        let fee_bump_amount = sum_txouts_amount(&self.escrow.params.extra_termination_outputs);

        let collateral_amount_default = escrow_amount - default_fee - fee_bump_amount;
        let collateral_amount_liquidation = escrow_amount - liquidation_fee - fee_bump_amount;

        // Borrower info created by the borrower is always valid
        let info = escrow::BorrowerInfo::<escrow::validation::Validated> {
            inputs: txos,
            tx_height: max_lock_height,
            escrow_eph_key: eph_pubkey,
            escrow_extra_outputs: funding.escrow_extra_outputs,
            escrow_contract_output_position: funding.escrow_contract_output_position,
            escrow_amount,
            collateral_amount_default,
            collateral_amount_liquidation,
            recover_outputs,
            repayment_outputs,
            _phantom: Default::default(),
        };
        info.serialize(message);
        let transactions = self.escrow.borrower_info(info);
        let sigs = transactions.sign_borrower(eph_key_pair);

        sigs.serialize(message);

        Ok(self.escrow.transactions_validated(transactions, sigs.recover, sigs.repayment))
    }

    pub fn funding_cancel(&self, transactions: Vec<Transaction>, fee_rate: FeeRate, current_height: Height, delay_rtl: RelativeDelay) -> Result<Transaction, FundingError> {
        self.escrow.participant_data.funding_cancel(transactions, fee_rate, current_height, delay_rtl)
    }

    pub fn serialize(&self, out: &mut Vec<u8>) {
        use super::super::Serialize;

        deserialize::StateVersion::CURRENT.serialize(out);
        out.push(constants::ParticipantId::Borrower as u8);
        out.push(constants::StateId::WaitingForFunding as u8);
        self.escrow.serialize(out);
    }

    pub fn deserialize(bytes: &mut &[u8]) -> Result<Self, WaitingForFundingError> {
        use super::super::Deserialize;

        let version = deserialize::StateVersion::deserialize(bytes)
            .map_err(WaitingForFundingErrorInner::from)?;
        if bytes.len() < 2 {
            return Err(WaitingForFundingErrorInner::UnexpectedEnd.into());
        }
        if bytes[0] != constants::ParticipantId::Borrower as u8 {
            return Err(WaitingForFundingErrorInner::InvalidParticipant(bytes[0]).into());
        }
        if bytes[1] != constants::StateId::WaitingForFunding as u8 {
            return Err(WaitingForFundingErrorInner::InvalidState(bytes[1]).into());
        }
        *bytes = &bytes[2..];
        let escrow = escrow::ReceivingBorrowerInfo::deserialize(bytes, version)
            .map_err(WaitingForFundingErrorInner::Escrow)?;

        Ok(WaitingForFunding { escrow })
    }
}

impl EscrowData {
    pub(crate) fn funding_cancel(&self, transactions: Vec<Transaction>, fee_rate: FeeRate, current_height: Height, delay_rtl: RelativeDelay) -> Result<Transaction, FundingError> {
        let return_script = self.return_script.clone();
        self.prefund.funding_cancel(transactions, fee_rate, current_height, delay_rtl, return_script)
    }
}

impl prefund::Prefund<super::Borrower> {
    pub fn funding_cancel(&self, transactions: Vec<Transaction>, fee_rate: FeeRate, current_height: Height, delay_rtl: RelativeDelay, return_script: ScriptBuf) -> Result<Transaction, FundingError> {
        let funding_script = self.funding_script();

        let mut max_lock_height = Height::from_consensus(0).expect("zero blocks is valid height");
        let mut txos = extract_spendable_outputs(transactions, &mut max_lock_height, |script| *script == funding_script);

        if txos.is_empty() {
            let error = FundingError {
                reason: FundingErrorReason::NoMatchingOutputs,
            };
            return Err(error);
        }

        let sequence = delay_rtl.offset_sequence(self.participant_data.prefund_lock_time)?;
        for txo in &mut txos {
            txo.sequence = sequence;
        }

        let (_, leaf_script) = self.participant_data.borrower_key_and_leaf_script();

        let witness_elem_sizes = [
            64, // len of schnorr signature
            leaf_script.len(),

              33 // base len of control block
            + 32 // len of merkle proof
        ];
        let input_weight_prediction = InputWeightPrediction::new(0, witness_elem_sizes.iter().copied());
        let return_script_len = return_script.len();
        let weight = predict_tx_weight(txos.len(), input_weight_prediction, core::iter::once(return_script_len));
        let total_input_amount = txos.iter()
            .map(|txo| txo.tx_out.value)
            .sum::<Amount>();
        let fee = weight * fee_rate;
        if fee > total_input_amount {
            let error = FundingError {
                reason: FundingErrorReason::Underfunded { required: fee, available: total_input_amount }
            };
            return Err(error);
        }
        let output_value = total_input_amount - fee;

        let tx_out = TxOut {
            value: output_value,
            script_pubkey: return_script,
        };

        Ok(self.spend_borrower(txos, vec![tx_out], current_height))
    }
}

#[derive(Copy, Clone)]
pub enum RelativeDelay {
    Height(u32),
    TimeUnits(u32),
    Zero,
}

impl RelativeDelay {
    fn offset_sequence(self, sequence: bitcoin::Sequence) -> Result<bitcoin::Sequence, FundingError> {
        match (self, sequence.is_height_locked(), sequence.is_time_locked()) {
            (RelativeDelay::Zero, _, _) => Ok(sequence),
            (RelativeDelay::Height(height), true, _) => {
                let sequence = sequence.0.checked_add(height).ok_or(FundingError { reason: FundingErrorReason::Overflow })?;
                let sequence = bitcoin::Sequence(sequence);
                if sequence.is_height_locked() {
                    Ok(sequence)
                } else {
                    Err(FundingError { reason: FundingErrorReason::Overflow })
                }
            },
            (RelativeDelay::TimeUnits(time), _, true) => {
                let sequence = sequence.0.checked_add(time).ok_or(FundingError { reason: FundingErrorReason::Overflow })?;
                let sequence = bitcoin::Sequence(sequence);
                if sequence.is_time_locked() {
                    Ok(sequence)
                } else {
                    Err(FundingError { reason: FundingErrorReason::Overflow })
                }
            },
            (_, false, false) => Err(FundingError { reason: FundingErrorReason::NotLocked }),
            _ => Err(FundingError { reason: FundingErrorReason::UnitMismatch }),
        }
    }
}

#[derive(Debug)]
pub struct WaitingForFundingError(WaitingForFundingErrorInner);

impl From<WaitingForFundingErrorInner> for WaitingForFundingError {
    fn from(error: WaitingForFundingErrorInner) -> Self {
        WaitingForFundingError(error)
    }
}

#[derive(Debug)]
enum WaitingForFundingErrorInner {
    UnexpectedEnd,
    UnsupportedVersion(u32),
    InvalidState(u8),
    InvalidParticipant(u8),
    Escrow(<escrow::ReceivingBorrowerInfo<super::Borrower> as super::super::Deserialize>::Error),
}

impl From<deserialize::StateVersionDeserError> for WaitingForFundingErrorInner {
    fn from(value: deserialize::StateVersionDeserError) -> Self {
        match value {
            deserialize::StateVersionDeserError::UnexpectedEnd => WaitingForFundingErrorInner::UnexpectedEnd,
            deserialize::StateVersionDeserError::UnsupportedVersion(version) => WaitingForFundingErrorInner::UnsupportedVersion(version),
        }
    }
}

#[non_exhaustive]
pub struct Funding {
    pub mandatory: MandatoryFundingParams,
    pub escrow_extra_outputs: Vec<TxOut>,
    pub escrow_contract_output_position: u32,
    pub repayment_extra_outputs: Vec<TxOut>,
    pub recover_extra_outputs: Vec<TxOut>,
}

pub struct MandatoryFundingParams {
    pub transactions: Vec<Transaction>,
    pub escrow_fee_rate: FeeRate,
    pub finalization_fee_rate: FeeRate,
}

impl MandatoryFundingParams {
    pub fn into_funding(self) -> Funding {
        Funding::new(self)
    }
}

impl Funding {
    pub fn new(mandatory: MandatoryFundingParams) -> Self {
        Funding {
            mandatory,
            escrow_extra_outputs: Default::default(),
            escrow_contract_output_position: 0,
            repayment_extra_outputs: Default::default(),
            recover_extra_outputs: Default::default(),
        }
    }

    pub fn from_hints(hints: offer::EscrowHints) -> Self {
        let mandatory = MandatoryFundingParams {
            transactions: hints.transactions,
            escrow_fee_rate: hints.fee_rate,
            // Rely mostly on fee bumping while allowing the opportunity to not pay any when
            // mempool is empty.
            finalization_fee_rate: FeeRate::BROADCAST_MIN,
        };
        Funding {
            mandatory,
            // No extra outputs by default
            escrow_extra_outputs: vec![hints.escrow_fee_bump_txout],
            escrow_contract_output_position: 0,
            // Insert fee bumping outputs only
            repayment_extra_outputs: vec![hints.finalization_fee_bump_txout.clone()],
            recover_extra_outputs: vec![hints.finalization_fee_bump_txout],
        }
    }
}

pub struct MandatoryPrefundParams {
    pub key_pair: Keypair,
    pub lock_time: Sequence,
    pub return_script: ScriptBuf,
}

impl MandatoryPrefundParams {
    pub fn into_params(self) -> PrefundParams {
        PrefundParams::new(self)
    }
}

#[non_exhaustive]
pub struct PrefundParams {
    pub mandatory: MandatoryPrefundParams,
}

impl PrefundParams {
    pub fn new(mandatory: MandatoryPrefundParams) -> Self {
        PrefundParams {
            mandatory,
        }
    }
}

#[derive(Debug)]
pub struct FundingError {
    pub reason: FundingErrorReason,
}

#[derive(Debug)]
pub enum FundingErrorReason {
    NoMatchingOutputs,
    Underfunded { required: Amount, available: Amount, },
    Overflow,
    NotLocked,
    UnitMismatch,
}

/// Extracts outputs with matching scripts from the previous transactions.
///
/// This performs a bunch of heavy lifting:
///
/// * Identifies all outputs
/// * Identifies the largest block-based lock time, if any
/// * Sets sequences to enable lock time if the height is not 0
///
/// All this locktime stuff is to implement anti-fee-sniping. Apart from incentivizing the miners
/// to not reorg the chain it also minimizes differences between the resulting transaction and
/// other transactions in the chain making analysis harder.
fn extract_spendable_outputs(transactions: impl IntoIterator<Item=Transaction>, max_lock_height: &mut Height, is_owned: impl Fn(&Script) -> bool) -> Vec<SpendableTxo> {
    let mut outputs = transactions.into_iter().flat_map(|transaction| {
        let txid = transaction.compute_txid();
        // Cheaper checks go first
        // Ignore non-block locktimes as those are not used to prevent fee sniping.
        if let LockTime::Blocks(height) = transaction.lock_time.into() {
            if height > *max_lock_height && transaction.is_lock_time_enabled() {
                *max_lock_height = height;
            }
        }

        transaction.output
            .into_iter()
            .enumerate()
            .filter(|(_, tx_out)| is_owned(&tx_out.script_pubkey))
            .map(move |(i, tx_out)| {
                // This is a sanity check that protects future changes extending this code from
                // accidentally introducing a malleability-caused vulnerability.
                // The code is currently written so that any input could be used for funding the
                // transaction, not just prefund. This could make the transactions cheaper and
                // a bit faster to process. However naive extension that doesn't ensure the inputs
                // are witness would cause a vulnerability. This should be checked by the caller
                // but it's not implemented right now because prefund implies SegWit. However, once
                // it's implemented, if the caller forgot to check this will save him from trouble.
                assert!(tx_out.script_pubkey.is_witness_program(), "danger: the input is not SegWit");

                // This won't panic because more than 2^32 outputs wouldn't fit into block
                // so the transaction would be rejected by the deserializer.
                let vout = i.try_into()
                    .expect("DoS protection failed");

                SpendableTxo {
                    tx_out,
                    out_point: OutPoint {
                        txid,
                        vout, 
                    },
                    // placeholder, we will patch it up in subsequent iteration so that all are the
                    // same value (to avoid leaking information).
                    sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                }
            })
    }).collect::<Vec<_>>();

    if max_lock_height.to_consensus_u32() != 0 {
        for output in &mut outputs {
            // Activate both RBF and lock time
            output.sequence = Sequence::ZERO;
        }
    }

    outputs
}

fn sum_txouts_amount<'a>(txos: impl IntoIterator<Item=&'a TxOut>) -> Amount {
    txos.into_iter().map(|txout| txout.value).sum()
}

fn predict_tx_weight(input_count: usize, input_prediction: InputWeightPrediction, txouts: impl Iterator<Item=usize>) -> Weight {
    bitcoin::transaction::predict_weight(core::iter::repeat(input_prediction).take(input_count), txouts)
}

impl escrow::SignaturesVerified<super::Borrower> {
    pub fn assemble_escrow(self) -> Result<escrow::EscrowSigned<super::Borrower>, (Self, escrow::SignatureVerificationError)> {
        let sig_key = self.state.participant_data.prefund.participant_data.key_pair;
        self.assemble_escrow_custom(|message| {
            Ok(SECP256K1.sign_schnorr(&message, &sig_key))
        })
    }
}

impl escrow::EscrowSigned<super::Borrower> {
    pub fn serialize_broadcast_request(&self, buf: &mut Vec<u8>) {
        buf.push(constants::MessageId::EscrowSigsFromBorrower as u8);
        buf.extend_from_slice(&(self.tx_escrow().input.len() as u32).to_be_bytes());

        let keys = &self.participant_data.prefund.keys;
        let borrower = keys.borrower_eph.as_x_only();
        let ted_o = keys.ted_o.as_x_only();
        let ted_p = keys.ted_p.as_x_only();
        // Reverse order because the witness is reversed (stack)
        let signature_position = match (borrower < ted_o, borrower < ted_p) {
            (true, true) => 2,
            (true, false) => 1,
            (false, true) => 1,
            (false, false) => 0
        };
        for input in &self.tx_escrow().input {
            let element = input.witness
                .iter()
                .nth(signature_position)
                .expect("transaction is finalised");
            // Sanity check
            secp256k1::schnorr::Signature::from_slice(element).unwrap();
            buf.extend_from_slice(element);
        }
    }
}

/// Contains all possible borrower states.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    WaitingForFunding(WaitingForFunding),
    ReceivingEscrowSignature { state: escrow::ReceivingEscrowSignature<super::Borrower>, received: Option<escrow::TedSignatures> },
    SignaturesVerified(escrow::SignaturesVerified<super::Borrower>),
    EscrowSigned(escrow::EscrowSigned<super::Borrower>),
}

impl State {
    pub fn serialize(&self, buf: &mut Vec<u8>) {
        use super::super::Serialize;

        match self {
            State::WaitingForFunding(state) => state.serialize(buf),
            State::ReceivingEscrowSignature { state, received: None } => state.serialize_with_header(buf),
            State::ReceivingEscrowSignature { state, received: Some(received) } => {
                state.serialize_with_header(buf);
                received.serialize(buf);
            },
            State::SignaturesVerified(state) => state.serialize_with_header(buf),
            State::EscrowSigned(state) => state.serialize_with_header(buf),
        }
    }

    pub fn deserialize(bytes: &mut &[u8]) -> Result<Self, StateDeserError> {
        use constants::StateId;
        use super::super::Deserialize;

        // Because we need to pass the original bytes to the inner functions we need to work with a
        // copy.
        let mut bytes_tmp: &[u8] = *bytes;

        // Normalize the position of the cursor
        let version = deserialize::StateVersion::deserialize(&mut bytes_tmp).map_err(StateDeserErrorInner::from)?;
        match version {
            deserialize::StateVersion::V0 => (),
            deserialize::StateVersion::V1 => (),
        }
        let first = bytes_tmp.get(1).ok_or(StateDeserErrorInner::UnexpectedEnd)?;
        let state_id = StateId::try_from(*first).map_err(StateDeserErrorInner::InvalidStateId)?;
        let state = match state_id {
            StateId::WaitingForFunding => State::WaitingForFunding(WaitingForFunding::deserialize(bytes).map_err(StateDeserErrorInner::WaitingForFunding)?),
            StateId::EscrowReceivingEscrowSignatures => {
                let state = escrow::ReceivingEscrowSignature::deserialize_with_header(bytes).map_err(StateDeserErrorInner::ReceivingEscrowSignature)?;
                let received = escrow::TedSignatures::deserialize(bytes).map_err(StateDeserErrorInner::TedSignatures)?;
                State::ReceivingEscrowSignature { state, received }
            },
            StateId::EscrowSignaturesVerified => State::SignaturesVerified(escrow::SignaturesVerified::deserialize_with_header(bytes).map_err(StateDeserErrorInner::SignaturesVerified)?),
            StateId::WaitingForEscrowConfirmation => State::EscrowSigned(escrow::EscrowSigned::deserialize_with_header(bytes).map_err(StateDeserErrorInner::EscrowSigned)?),
            unexpected => return Err(StateDeserErrorInner::UnexpectedStateId(unexpected).into()),
        };
        Ok(state)
    }

    pub fn network(&self) -> bitcoin::Network {
        match self {
            State::WaitingForFunding(state) => state.network(),
            State::ReceivingEscrowSignature { state, .. } => state.params.network,
            State::SignaturesVerified(state) => state.state.params.network,
            State::EscrowSigned(_) => panic!("should not be called"),
        }
    }

    pub fn funding_cancel(&self, transactions: Vec<Transaction>, fee_rate: FeeRate, current_height: Height, delay_rtl: RelativeDelay) -> Result<Transaction, FundingError> {
        let escrow_data = match self {
            State::WaitingForFunding(state) => &state.escrow.participant_data,
            State::ReceivingEscrowSignature { state, .. } => &state.participant_data,
            State::SignaturesVerified(state) => &state.state.participant_data,
            State::EscrowSigned(state) => &state.participant_data,
        };

        escrow_data.funding_cancel(transactions, fee_rate, current_height, delay_rtl)
    }

    fn from_escrow_data_and_offer(escrow_data: EscrowData, offer: Offer) -> Self {
        State::WaitingForFunding(WaitingForFunding::from_escrow_data_and_offer(escrow_data, offer))
    }

    /// Changes the state back to WaitingForFunding.
    pub fn reset(&mut self, offer: Offer) {
        match self {
            State::WaitingForFunding(_) => (), // nothing to do
            State::ReceivingEscrowSignature { state, .. } => {
                *self = Self::from_escrow_data_and_offer(state.participant_data.clone(), offer);
            },
            State::SignaturesVerified(state) => {
                *self = Self::from_escrow_data_and_offer(state.state.participant_data.clone(), offer);
            },
            State::EscrowSigned(state) => {
                *self = Self::from_escrow_data_and_offer(state.participant_data.clone(), offer);
            },
        }
    }
}

#[cfg(test)]
impl quickcheck::Arbitrary for State {
    fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
        if bool::arbitrary(gen) {
            State::WaitingForFunding(WaitingForFunding::arbitrary(gen))
        } else {
            State::ReceivingEscrowSignature { state: escrow::ReceivingEscrowSignature::arbitrary(gen), received: Option::arbitrary(gen) }
        }
    }
}

#[derive(Debug)]
pub struct StateDeserError(StateDeserErrorInner);

#[derive(Debug)]
enum StateDeserErrorInner {
    UnexpectedEnd,
    UnsupportedVersion(u32),
    InvalidStateId(constants::InvalidEnumValue),
    UnexpectedStateId(constants::StateId),
    WaitingForFunding(WaitingForFundingError),
    ReceivingEscrowSignature(super::super::StateDeserError<escrow::ReceivingEscrowSignatureDeserError<EscrowDataDeserError>>),
    TedSignatures(escrow::TedSignaturesDeserError),
    SignaturesVerified(super::super::StateDeserError<escrow::SignaturesVerifiedDeserError<EscrowDataDeserError>>),
    EscrowSigned(super::super::StateDeserError<escrow::EscrowSignedDeserError<EscrowDataDeserError>>),
}

impl From<StateDeserErrorInner> for StateDeserError {
    fn from(error: StateDeserErrorInner) -> Self {
        StateDeserError(error)
    }
}

impl From<deserialize::StateVersionDeserError> for StateDeserErrorInner {
    fn from(value: deserialize::StateVersionDeserError) -> Self {
        match value {
            deserialize::StateVersionDeserError::UnexpectedEnd => StateDeserErrorInner::UnexpectedEnd,
            deserialize::StateVersionDeserError::UnsupportedVersion(version) => StateDeserErrorInner::UnsupportedVersion(version),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    crate::test_macros::check_roundtrip!(roundtrip_waiting_for_funding, WaitingForFunding);
    crate::test_macros::check_roundtrip!(roundtrip_state, State);
}
