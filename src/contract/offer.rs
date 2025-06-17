//! Defines the Firefish offer and related types.
//!
//! The offer module contains all the information about the offer
//! that is shared between the participants and Firefish verification service.

use bitcoin::TxOut;
use bitcoin::p2p::Magic;
use core::convert::TryInto;
use core::fmt;

use super::{context, participant, deserialize};
use super::pub_keys::{PubKey, PubKeys};
use bitcoin::blockdata::FeeRate;

pub struct MandatoryOfferFields {
    /// The network this contract operates on.
    pub network: bitcoin::Network,

    pub liquidator_script_default: bitcoin::ScriptBuf,
    pub liquidator_script_liquidation: bitcoin::ScriptBuf,
    pub min_collateral: bitcoin::Amount,

    /// The lock time of recover transaction.
    pub recover_lock_time: bitcoin::absolute::LockTime,

    /// The lock time of default transaction.
    pub default_lock_time: bitcoin::absolute::LockTime,

    pub ted_o_keys: AllParticipantKeys<participant::TedO>,
    pub ted_p_keys: AllParticipantKeys<participant::TedP>,
}

impl MandatoryOfferFields {
    pub fn into_offer(self) -> Offer {
        self.into_offer_with_optional(Default::default())
    }

    pub fn into_offer_with_optional(self, optional: OptionalOfferFields) -> Offer {
        use bitcoin::secp256k1::rand::Rng;

        let liquidator_output_index = bitcoin::secp256k1::rand::thread_rng()
            .gen_range::<usize, _>(0..=optional.extra_termination_outputs.len());
        let escrow = EscrowParams {
            network: self.network,
            liquidator_script_default: self.liquidator_script_default,
            liquidator_script_liquidation: self.liquidator_script_liquidation,
            min_collateral: self.min_collateral,
            extra_termination_outputs: optional.extra_termination_outputs,
            liquidator_output_index,
            recover_lock_time: self.recover_lock_time,
            default_lock_time: self.default_lock_time,
        };
        let prefund_keys = TedSigPubKeys {
            ted_o: self.ted_o_keys.prefund,
            ted_p: self.ted_p_keys.prefund,
        };
        let escrow_keys = TedSigPubKeys {
            ted_o: self.ted_o_keys.escrow,
            ted_p: self.ted_p_keys.escrow,
        };
        Offer {
            escrow,
            escrow_keys,
            prefund_keys,
        }
    }
}

#[derive(Default)]
#[non_exhaustive]
pub struct OptionalOfferFields {
    pub extra_termination_outputs: Vec<TxOut>,
}

/// The initialization information about the contract.
///
/// These are the parameters required to initialize the contract.
/// They are provided byt the lender in collaboration with Firefish.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Offer {
    pub escrow: EscrowParams,
    pub escrow_keys: TedSigPubKeys<context::Escrow>,
    pub prefund_keys: TedSigPubKeys<context::Prefund>,
}

impl Offer {
    const VERSION: u8 = 1;
    const ESCROW_PARAMS_VERSION: EscrowParamsVersion = match EscrowParamsVersion::from_num(Offer::VERSION as u32) { Some(version) => version, None => unreachable!(), };

    pub fn deserialize(bytes: &mut &[u8]) -> Result<Self, DeserializationError> {
        if bytes.len() < 151 {
            return Err(DeserializationError::UnexpectedEnd);
        }

        if bytes[0] != Offer::VERSION {
            return Err(DeserializationError::UnknownVersion(bytes[0]));
        }

        *bytes = &bytes[1..];
        let prefund_keys = TedSigPubKeys::deserialize(bytes)?;
        let escrow_keys = TedSigPubKeys::deserialize(bytes)?;
        let escrow = EscrowParams::deserialize(bytes, Self::ESCROW_PARAMS_VERSION)?;
        let offer = Offer {
            escrow_keys,
            prefund_keys,
            escrow,
        };
        Ok(offer)
    }

    pub fn serialize(&self, out: &mut Vec<u8>) {
        out.reserve(self.escrow.reserve_suggestion() + 1 + 4 * 32);
        out.push(Offer::VERSION);
        self.prefund_keys.serialize(out);
        self.escrow_keys.serialize(out);
        self.escrow.serialize(out);
    }
}

crate::test_macros::impl_arbitrary!(Offer, escrow, escrow_keys, prefund_keys);

#[derive(Debug)]
pub enum DeserializationError {
    UnexpectedEnd,
    UnknownVersion(u8),
    InvalidKey(bitcoin::secp256k1::Error),
    UnknownNetwork(Magic),
    InvalidLiquidatorIndex(u8),
    Consensus(bitcoin::consensus::encode::Error),
    LiquidatorOutputIndexOutOfRange { index: usize, count: usize },
    TooManyExtraOutputs(usize),
}

impl From<deserialize::UnexpectedEnd> for DeserializationError {
    fn from(_: deserialize::UnexpectedEnd) -> Self {
        DeserializationError::UnexpectedEnd
    }
}

impl From<bitcoin::secp256k1::Error> for DeserializationError {
    fn from(error: bitcoin::secp256k1::Error) -> Self {
        DeserializationError::InvalidKey(error)
    }
}

impl From<bitcoin::consensus::encode::Error> for DeserializationError {
    fn from(error: bitcoin::consensus::encode::Error) -> Self {
        DeserializationError::Consensus(error)
    }
}

/// The information about the escrow contract excluding the keys.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct EscrowParams {
    /// The network this contract operates on.
    pub network: bitcoin::Network,

    /// The liquidator script used when the contract is terminated because it was not repaid.
    pub liquidator_script_default: bitcoin::ScriptBuf,

    /// The liquidator script used when the contract is terminated because the price fell too much.
    pub liquidator_script_liquidation: bitcoin::ScriptBuf,

    /// The minimal collatral required for the loan.
    pub min_collateral: bitcoin::Amount,

    /// The extra outputs used in termination transaction.
    ///
    /// There's usually only one: the output used for bumping the fees.
    pub extra_termination_outputs: Vec<TxOut>,

    /// If the borrower wants to over-collaterize he needs to bump this index.
    pub liquidator_output_index: usize,

    /// The lock time of recover transaction.
    pub recover_lock_time: bitcoin::absolute::LockTime,

    /// The lock time of default transaction.
    pub default_lock_time: bitcoin::absolute::LockTime,
}

impl EscrowParams {
    pub(crate) fn deserialize(bytes: &mut &[u8], version: EscrowParamsVersion) -> Result<Self, DeserializationError> {
        if bytes.len() < 8 {
            return Err(DeserializationError::UnexpectedEnd);
        }

        // Yes, this wastes three bytes since there are only 4 networks today.
        // However `bitcoin::Network` is (rightly) `#[non_exhaustive]` and if we used it naively
        // we would be forced to panic or error in serialization code which is very bad.
        // We could use our own enum but then we'd have to manually add variants each time there's
        // a new network.
        //
        // Using magic saves us from all that trouble. If a new network is added and supported by
        // `rust-bitcoin` all we need is to update the library (modulo frequent breaking changes)
        // and it will work out of the box.
        let network = Magic::from_bytes(bytes[..4].try_into().expect("checked above"));
        let network = bitcoin::Network::from_magic(network)
            .ok_or(DeserializationError::UnknownNetwork(network))?;

        let liquidator_output_index = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize;
        *bytes = &bytes[8..];
        let recover_lock_time = bitcoin::consensus::Decodable::consensus_decode(bytes)?;
        let default_lock_time = bitcoin::consensus::Decodable::consensus_decode(bytes)?;
        let (liquidator_script_default, liquidator_script_liquidation, min_collateral) = match version {
            EscrowParamsVersion::V0 => {
                let liquidator_output: bitcoin::TxOut = bitcoin::consensus::Decodable::consensus_decode(bytes)?;
                let default = liquidator_output.script_pubkey.clone();
                (default, liquidator_output.script_pubkey, liquidator_output.value)
            },
            EscrowParamsVersion::V1 => {
                let liquidator_script_default = bitcoin::consensus::Decodable::consensus_decode(bytes)?;
                let liquidator_script_liquidation = bitcoin::consensus::Decodable::consensus_decode(bytes)?;
                let min_collateral = bitcoin::consensus::Decodable::consensus_decode(bytes)?;
                (liquidator_script_default, liquidator_script_liquidation, min_collateral)
            },
        };
        let extra_output_count = deserialize::be::<u32>(bytes)? as usize;
        if extra_output_count > 4_000_000 / 9 {
           return Err(DeserializationError::TooManyExtraOutputs(extra_output_count));
        }
        if liquidator_output_index > extra_output_count {
            return Err(DeserializationError::LiquidatorOutputIndexOutOfRange { index: liquidator_output_index, count: extra_output_count });
        }
        let mut extra_termination_outputs = Vec::with_capacity(extra_output_count);
        for _ in 0..extra_output_count {
            extra_termination_outputs.push(bitcoin::consensus::Decodable::consensus_decode(bytes)?);
        }
        let escrow_params = EscrowParams {
            network,
            recover_lock_time,
            default_lock_time,
            liquidator_script_default,
            liquidator_script_liquidation,
            min_collateral,
            liquidator_output_index,
            extra_termination_outputs,
        };
        Ok(escrow_params)
    }

    pub(crate) fn serialize(&self, out: &mut Vec<u8>) {
        use bitcoin::consensus::Encodable;

        out.extend_from_slice(&self.network.magic().to_bytes());
        out.extend_from_slice(&(self.liquidator_output_index as u32).to_be_bytes());
        self.recover_lock_time.consensus_encode(out).expect("vec doesn't error");
        self.default_lock_time.consensus_encode(out).expect("vec doesn't error");
        self.liquidator_script_default.consensus_encode(out).expect("vec doesn't error");
        self.liquidator_script_liquidation.consensus_encode(out).expect("vec doesn't error");
        self.min_collateral.consensus_encode(out).expect("vec doesn't error");
        out.extend_from_slice(&(self.extra_termination_outputs.len() as u32).to_be_bytes());
        for output in &self.extra_termination_outputs {
            output.consensus_encode(out).expect("vec doesn't error");
        }
    }

    pub(crate) fn reserve_suggestion(&self) -> usize {
        use bitcoin::consensus::encode::VarInt;

        let excluding_liquidator_script = self.extra_termination_outputs.iter()
            .map(|txout| txout.script_pubkey.len() + VarInt(txout.script_pubkey.len() as u64).size())
            .sum::<usize>()
            + 4 + 1 + 2*8 + 4;

        let default = self.liquidator_script_default.len() + VarInt(self.liquidator_script_default.len() as u64).size();
        let liquidation = self.liquidator_script_liquidation.len() + VarInt(self.liquidator_script_liquidation.len() as u64).size();
        excluding_liquidator_script + default + liquidation
    }
}

deserialize::version_enum! {
    pub enum EscrowParamsVersion {
        V0 = 0x00,
        V1 = 0x01,
    }
}

#[cfg(test)]
impl quickcheck::Arbitrary for EscrowParams {
    fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
        #[derive(Clone)]
        struct EscrowParamsHelper {
            network: bitcoin::Network,
            liquidator_script_default: bitcoin::ScriptBuf,
            liquidator_script_liquidation: bitcoin::ScriptBuf,
            min_collateral: bitcoin::Amount,
            extra_termination_outputs: Vec<TxOut>,
            recover_lock_time: bitcoin::absolute::LockTime,
            default_lock_time: bitcoin::absolute::LockTime,
        }
        crate::test_macros::impl_arbitrary!(EscrowParamsHelper, network, recover_lock_time, default_lock_time, liquidator_script_default, liquidator_script_liquidation, min_collateral, extra_termination_outputs);

        let helper = EscrowParamsHelper::arbitrary(gen);
        let liquidator_output_index = loop {
            let index = usize::arbitrary(gen);
            if index <= helper.extra_termination_outputs.len() {
                break index;
            }
        };
        EscrowParams {
            network: helper.network,
            liquidator_script_default: helper.liquidator_script_default,
            liquidator_script_liquidation: helper.liquidator_script_liquidation,
            min_collateral: helper.min_collateral,
            extra_termination_outputs: helper.extra_termination_outputs,
            recover_lock_time: helper.recover_lock_time,
            default_lock_time: helper.default_lock_time,
            liquidator_output_index,
        }
    }
}

/// The keys provided by TedSig.
pub struct TedSigPubKeys<C> {
    /// The public key of TED-O
    pub ted_o: PubKey<participant::TedO, C>,

    /// The public key of TED-P
    pub ted_p: PubKey<participant::TedP, C>,
}

crate::test_macros::impl_test_traits!(TedSigPubKeys<C>, ted_o, ted_p);

impl<C> TedSigPubKeys<C> {
    pub(crate) fn deserialize(bytes: &mut &[u8]) -> Result<Self, DeserializationError> {
        let ted_o = PubKey::deserialize_raw(bytes)?;
        let ted_p = PubKey::deserialize_raw(bytes)?;

        Ok(TedSigPubKeys { ted_o, ted_p, })
    }

    pub(crate) fn serialize(&self, out: &mut Vec<u8>) {
        self.ted_o.serialize_raw(out);
        self.ted_p.serialize_raw(out);
    }
}

impl<C> Copy for TedSigPubKeys<C> {}

impl<C> TedSigPubKeys<C> {
    /// Add the ephemeral public keys of the borrower to the keys.
    pub fn add_borrower_eph(self, borrower_eph: PubKey<participant::Borrower, C>) -> PubKeys<C> {
        PubKeys {
            borrower_eph,
            ted_o: self.ted_o,
            ted_p: self.ted_p,
        }
    }
}

crate::test_macros::impl_arbitrary!(TedSigPubKeys<C>, ted_o, ted_p);

/// Helper for parsing and displaying all TedSig keys.
pub struct AllParticipantKeys<P: participant::Participant> {
    pub prefund: PubKey<P, context::Prefund>,
    pub escrow: PubKey<P, context::Escrow>,
}

impl<P: participant::Participant> fmt::Display for AllParticipantKeys<P> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // prefix with magic string to distinguish them
        write!(f, "ffa{}k", P::HUMAN_IDENTIFIER)?;
        fmt::Display::fmt(self.prefund.as_x_only(), f)?;
        fmt::Display::fmt(self.escrow.as_x_only(), f)?;
        Ok(())
    }
}

pub enum AnyTedSigKeys {
    TedO(AllParticipantKeys<participant::TedO>),
    TedP(AllParticipantKeys<participant::TedP>),
}

impl core::str::FromStr for AnyTedSigKeys {
    type Err = TedSigKeysParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 5 + 64 + 64 {
            return Err(TedSigKeysParseError::InvalidLength(s.len()));
        }
        if !s.starts_with("ffa") {
            return Err(TedSigKeysParseError::InvalidPrefix(s.into()));
        }
        let mut chars = s[3..].chars();
        let participant = chars.next().ok_or_else(|| TedSigKeysParseError::InvalidPrefix(s.into()))?;
        if !participant.is_ascii() {
            return Err(TedSigKeysParseError::InvalidPrefix(s.into()));
        }
        if chars.next() != Some('k') {
            return Err(TedSigKeysParseError::InvalidPrefix(s.into()));
        }
        // Required for safe slicing
        for c in chars.clone() {
            if !c.is_ascii() {
                return Err(TedSigKeysParseError::NonAsciiChar(c));
            }
        }
        // we checked the length above and it only contains ascii
        // a key has 2*32 hex digits
        let prefund = chars.as_str()[..64].parse().map_err(TedSigKeysParseError::InvalidKey)?;
        let escrow = chars.as_str()[64..].parse().map_err(TedSigKeysParseError::InvalidKey)?;

        match participant {
            'o' => Ok(AnyTedSigKeys::TedO(AllParticipantKeys { prefund: PubKey::new(prefund), escrow: PubKey::new(escrow) })),
            'p' => Ok(AnyTedSigKeys::TedP(AllParticipantKeys { prefund: PubKey::new(prefund), escrow: PubKey::new(escrow) })),
            x => Err(TedSigKeysParseError::InvalidParticipant(x)),
        }
    }
}

impl core::convert::TryFrom<String> for AnyTedSigKeys {
    type Error = TedSigKeysParseError;

    fn try_from(string: String) -> Result<Self, Self::Error> {
        string.parse()
    }
}

#[derive(Debug)]
pub enum TedSigKeysParseError {
    InvalidPrefix(String),
    InvalidParticipant(char),
    NonAsciiChar(char),
    InvalidLength(usize),
    InvalidKey(bitcoin::secp256k1::Error),
}

/// Suggestions for various parameters of the contract provided by Firefish.
///
/// The borrwer doesn't have to obey these suggestions but to meaningfully not obey them he has to
/// be a power user. Thus the initial version will almost-blindly accept them.
#[non_exhaustive]
pub struct PrefundHints {
    /// How much should the borrower reserve for paying miner fees.
    ///
    /// Firefish computes this as `expected_fee_rate * expected_transaction_size`
    fee_reserve: bitcoin::Amount,
}

/// Suggestions for various parameters of the contract provided by Firefish.
///
/// The borrwer doesn't have to obey these suggestions but to meaningfully not obey them he has to
/// be a power user. Thus the initial version will almost-blindly accept them.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct EscrowHints {
    /// The fee rate to use for funding the escrow contract.
    pub fee_rate: FeeRate,

    /// Transaction output used for fee bumping.
    pub escrow_fee_bump_txout: bitcoin::TxOut,

    /// Transaction output used for fee bumping.
    pub finalization_fee_bump_txout: bitcoin::TxOut,

    /// Transactions in the mempool or chain that have the script in at least one of the outputs
    /// equal to the script generated by prefund.
    pub transactions: Vec<bitcoin::Transaction>,
}

crate::test_macros::impl_arbitrary!(EscrowHints, fee_rate, finalization_fee_bump_txout, escrow_fee_bump_txout, transactions);

impl EscrowHints {
    pub fn new(fee_rate: FeeRate, escrow_fee_bump_txout: bitcoin::TxOut, finalization_fee_bump_txout: bitcoin::TxOut, transactions: Vec<bitcoin::Transaction>) -> Self {
        EscrowHints {
            fee_rate,
            finalization_fee_bump_txout,
            escrow_fee_bump_txout,
            transactions,
        }
    }

    pub fn serialize(&self, buf: &mut Vec<u8>) {
        use bitcoin::consensus::Encodable;

        buf.push(super::constants::MessageId::EscrowHints as u8);
        buf.extend_from_slice(&self.fee_rate.to_sat_per_kwu().to_be_bytes());
        self.escrow_fee_bump_txout.consensus_encode(buf).expect("vec doesn't error");
        self.finalization_fee_bump_txout.consensus_encode(buf).expect("vec doesn't error");
        buf.extend_from_slice(&(self.transactions.len() as u32).to_be_bytes());
        for transaction in &self.transactions {
            transaction.consensus_encode(buf).expect("vec doesn't error");
        }
    }

    pub fn deserialize(bytes: &mut &[u8]) -> Result<Self, EscrowHintsDeserError> {
        use bitcoin::consensus::Decodable;

        let message_id = bytes.get(0).ok_or(super::deserialize::UnexpectedEnd)?;
        if *message_id != super::constants::MessageId::EscrowHints as u8 {
            return Err(EscrowHintsDeserErrorInner::InvalidMessageId(*message_id).into());
        }
        *bytes = &bytes[1..];
        let fee_rate = FeeRate::from_sat_per_kwu(deserialize::be(bytes)?);
        let escrow_fee_bump_txout = TxOut::consensus_decode(bytes)
            .map_err(EscrowHintsDeserErrorInner::InvalidTxOut)?;
        let finalization_fee_bump_txout = TxOut::consensus_decode(bytes)
            .map_err(EscrowHintsDeserErrorInner::InvalidTxOut)?;
        let transaction_count = deserialize::be::<u32>(bytes)? as usize;
        let transactions = (0..transaction_count)
            .map(|_| bitcoin::Transaction::consensus_decode(bytes))
            .collect::<Result<Vec<_>, _>>()
            .map_err(EscrowHintsDeserErrorInner::InvalidTransaction)?;

        Ok(EscrowHints {
            fee_rate,
            escrow_fee_bump_txout,
            finalization_fee_bump_txout,
            transactions,
        })
    }
}

#[derive(Debug)]
pub struct EscrowHintsDeserError(EscrowHintsDeserErrorInner);

impl From<deserialize::UnexpectedEnd> for EscrowHintsDeserError {
    fn from(_: deserialize::UnexpectedEnd) -> Self {
        EscrowHintsDeserError(EscrowHintsDeserErrorInner::UnexpectedEnd)
    }
}

#[derive(Debug)]
enum EscrowHintsDeserErrorInner {
    UnexpectedEnd,
    InvalidMessageId(u8),
    InvalidTxOut(bitcoin::consensus::encode::Error),
    InvalidTransaction(bitcoin::consensus::encode::Error),
}

impl From<EscrowHintsDeserErrorInner> for EscrowHintsDeserError {
    fn from(error: EscrowHintsDeserErrorInner) -> Self {
        EscrowHintsDeserError(error)
    }
}

#[cfg(test)]
mod tests {
    quickcheck::quickcheck! {
        fn tedsig_pub_keys_roundtrips(keys: super::TedSigPubKeys<super::context::Escrow>) -> bool {
            let mut bytes = Vec::new();
            keys.serialize(&mut bytes);
            let keys2 = super::TedSigPubKeys::<super::context::Escrow>::deserialize(&mut &*bytes).unwrap();
            keys2 == keys
        }

        fn escrow_params_roundtrips(escrow_params: super::EscrowParams) -> bool {
            let mut bytes = Vec::new();
            escrow_params.serialize(&mut bytes);
            let mut bytes = &*bytes;
            let escrow_params2 = super::EscrowParams::deserialize(&mut bytes, super::Offer::ESCROW_PARAMS_VERSION).unwrap();
            escrow_params2 == escrow_params && bytes.len() == 0
        }

        fn offer_roundtrips(offer: super::Offer) -> bool {
            let mut bytes = Vec::new();
            offer.serialize(&mut bytes);
            let mut bytes = &*bytes;
            let offer2 = super::Offer::deserialize(&mut bytes).unwrap();
            offer2 == offer && bytes.len() == 0
        }
    }

    crate::test_macros::check_roundtrip!(roundtrip_escrow_hints, super::super::EscrowHints);
}
