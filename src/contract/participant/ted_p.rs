use bitcoin::{key::Keypair, Transaction};
use super::super::{Serialize, Deserialize, HotKey, prefund, escrow, offer, deserialize};
use secp256k1::schnorr::Signature;

#[derive(Clone, PartialEq, Debug)]
#[non_exhaustive]
pub struct PrefundData {
    key_pair: Keypair,
}

crate::test_macros::impl_arbitrary!(PrefundData, key_pair);

impl Serialize for PrefundData {
    fn serialize(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.key_pair.secret_bytes());
    }
}

impl Deserialize for PrefundData {
    type Error = PrefundDataDeserError;

    fn deserialize(bytes: &mut &[u8], version: deserialize::StateVersion) -> Result<Self, Self::Error> {
        match version {
            deserialize::StateVersion::V0 => (),
            deserialize::StateVersion::V1 => (),
        }
        let key_pair = deserialize::key_pair(bytes)
            .map_err(PrefundDataDeserErrorInner::Secp256k1)
            .map_err(PrefundDataDeserError)?;
        Ok(PrefundData { key_pair, })
    }
}

#[derive(Debug)]
pub struct PrefundDataDeserError(PrefundDataDeserErrorInner);

#[derive(Debug)]
enum PrefundDataDeserErrorInner {
    Secp256k1(secp256k1::Error),
}


impl HotKey for PrefundData {
    fn participant_key_pair(&self) -> &Keypair {
        &self.key_pair
    }
}

#[derive(Clone, PartialEq, Debug)]
#[non_exhaustive]
pub struct EscrowData {
    prefund: prefund::State<super::TedP>,
    key_pair: Keypair,
}

crate::test_macros::impl_arbitrary!(EscrowData, prefund, key_pair);

impl Serialize for EscrowData {
    fn serialize(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.key_pair.secret_bytes());
        self.prefund.serialize_unversioned(out);
    }
}

impl Deserialize for EscrowData {
    type Error = EscrowDataDeserError;

    fn deserialize(bytes: &mut &[u8], version: deserialize::StateVersion) -> Result<Self, Self::Error> {
        let key_pair = deserialize::key_pair(bytes)
            .map_err(EscrowDataDeserErrorInner::Secp256k1)
            .map_err(EscrowDataDeserError)?;
        let prefund = prefund::State::deserialize_fixed_version(bytes, version)
            .map_err(EscrowDataDeserErrorInner::Prefund)
            .map_err(EscrowDataDeserError)?;
        Ok(EscrowData { key_pair, prefund, })
    }
}

#[derive(Debug)]
pub struct EscrowDataDeserError(EscrowDataDeserErrorInner);

#[derive(Debug)]
enum EscrowDataDeserErrorInner {
    Secp256k1(secp256k1::Error),
    Prefund(prefund::StateDeserError<PrefundDataDeserError>),
}

impl super::super::SetBorrowerSpendInfo for EscrowData {
    fn set_borrower_spend_info(self, info: prefund::BorrowerSpendInfo) -> Result<Self, (Self, super::super::BorrowerInfoError)> {
        match self.prefund {
            prefund::State::ReceivingBorrowerInfo(state) => {
                let new_state = state.borrower_info_received(secp256k1::SECP256K1, info);
                Ok(EscrowData {
                    prefund: prefund::State::Ready(new_state),
                    key_pair: self.key_pair,
                })
            },
            prefund @ prefund::State::Ready(_) => {
                Err((EscrowData {
                    prefund,
                    key_pair: self.key_pair,
                }, super::super::BorrowerInfoError::AlreadyReceived))
            }
        }
    }
}

pub fn init(prefund_key_pair: Keypair, escrow_key_pair: Keypair, offer: offer::Offer) -> escrow::ReceivingBorrowerInfo<super::TedP> {
    let prefund_data = PrefundData {
        key_pair: prefund_key_pair,
    };
    let prefund = prefund::State::with_participant_data(offer.prefund_keys, offer.escrow.network, prefund_data);
    let escrow_data = EscrowData {
        prefund,
        key_pair: escrow_key_pair,
    };
    escrow::ReceivingBorrowerInfo::with_participant_data(offer.escrow, offer.escrow_keys, escrow_data)
}

impl escrow::ReceivingBorrowerInfo<super::TedP> {
    pub fn ted_p_set_and_sign_transactions(self, transactions: escrow::UnsignedTransactions, borrower: escrow::BorrowerSignatures) -> (escrow::WaitingForEscrowConfirmation<super::TedP>, escrow::TedPSignatures) {
        let prefund = match &self.participant_data.prefund {
            prefund::State::Ready(prefund) => Some(prefund),
            prefund::State::ReceivingBorrowerInfo(_) => None,
        };
        let signatures = transactions.sign_ted_p(self.participant_data.key_pair, prefund);
        let state = self.transactions_presigned(transactions, borrower);
        (state, signatures)
    }
}

impl escrow::WaitingForEscrowConfirmation<super::TedP> {
    pub fn sign_repayment(&mut self, ted_o_signature: &Signature) -> &Transaction {
        let signature = secp256k1::SECP256K1.sign_schnorr(&self.unsigned_txes.repayment_signing_data(), &self.participant_data.key_pair);
        let keys = self.keys.add_borrower_eph(self.unsigned_txes.borrower_eph);
        escrow::finalize(&mut self.unsigned_txes.repayment, &keys, &self.borrower.repayment, ted_o_signature, &signature);
        &self.unsigned_txes.repayment
    }

    pub fn sign_default(&mut self, ted_o_signature: &Signature) -> &Transaction {
        let signature = secp256k1::SECP256K1.sign_schnorr(&self.unsigned_txes.default_signing_data(), &self.participant_data.key_pair);
        let keys = self.keys.add_borrower_eph(self.unsigned_txes.borrower_eph);
        escrow::finalize(&mut self.unsigned_txes.default, &keys, &self.borrower.default, ted_o_signature, &signature);
        &self.unsigned_txes.default
    }

    pub fn sign_liquidation(&mut self, ted_o_signature: &Signature) -> &Transaction {
        let signature = secp256k1::SECP256K1.sign_schnorr(&self.unsigned_txes.liquidation_signing_data(), &self.participant_data.key_pair);
        let keys = self.keys.add_borrower_eph(self.unsigned_txes.borrower_eph);
        escrow::finalize(&mut self.unsigned_txes.liquidation, &keys, &self.borrower.liquidation, ted_o_signature, &signature);
        &self.unsigned_txes.liquidation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    crate::test_macros::check_roundtrip_with_version!(roundtrip_prefund_data, PrefundData);
    crate::test_macros::check_roundtrip_with_version!(roundtrip_escrow_data, EscrowData);
}
