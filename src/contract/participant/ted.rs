use super::super::{offer, prefund, escrow, constants::MessageId};
use core::convert::TryFrom;

pub enum IncomingMessage {
    Offer(offer::Offer),
    PrefundInfo(prefund::BorrowerSpendInfo),
    EscrowInfo(escrow::BorrowerInfoMessage),
}

impl IncomingMessage {
    pub fn deserialize(bytes: &mut &[u8]) -> Result<Self, MessageDeserError> {
        let message_id = *bytes.first().ok_or(MessageDeserError::Empty)?;
        let message_id = MessageId::try_from(message_id).map_err(|_| MessageDeserError::InvalidMessageId(message_id))?;
        match message_id {
            MessageId::Offer => {
                *bytes = &bytes[1..];
                Ok(IncomingMessage::Offer(offer::Offer::deserialize(bytes)?))
            },
            MessageId::PrefundBorrowerInfo => Ok(IncomingMessage::PrefundInfo(prefund::BorrowerSpendInfo::deserialize(bytes)?)),
            MessageId::EscrowBorrowerInfo => Ok(IncomingMessage::EscrowInfo(escrow::BorrowerInfoMessage::deserialize(bytes)?)),
            _ => Err(MessageDeserError::InvalidMessageId(message_id as u8))
        }
    }
}

#[derive(Debug)]
pub enum MessageDeserError {
    Empty,
    InvalidMessageId(u8),
    InvalidOffer(offer::DeserializationError),
    InvalidPrefundInfo(prefund::BorrowerSpendInfoDeserError),
    InvalidEscrowInfo(escrow::BorrowerInfoMessageDeserError),
}

impl From<offer::DeserializationError> for MessageDeserError {
    fn from(value: offer::DeserializationError) -> Self {
        Self::InvalidOffer(value)
    }
}

impl From<prefund::BorrowerSpendInfoDeserError> for MessageDeserError {
    fn from(value: prefund::BorrowerSpendInfoDeserError) -> Self {
        Self::InvalidPrefundInfo(value)
    }
}

impl From<escrow::BorrowerInfoMessageDeserError> for MessageDeserError {
    fn from(value: escrow::BorrowerInfoMessageDeserError) -> Self {
        Self::InvalidEscrowInfo(value)
    }
}
