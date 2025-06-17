// FIXME: this was a mistake, enum (like below) is better because the compiler checks for collisions
pub(crate) mod state_id {
    pub(crate) const BORROWER_ESCROW_DATA: u8 = 0x06;
}

macro_rules! u8_enum {
    ($vis:vis enum $name:ident { $($variant:ident = $val:expr),* $(,)? }) => {
        #[derive(Debug, Copy, Clone, Eq, PartialEq)]
        $vis enum $name {
            $($variant = $val,)*
        }

        impl core::convert::TryFrom<u8> for $name {
            type Error = InvalidEnumValue;

            fn try_from(val: u8) -> Result<Self, Self::Error> {
                match val {
                    $($val => Ok($name::$variant),)*
                    _ => Err(InvalidEnumValue(val))
                }
            }
        }
    }
}

u8_enum! {
    pub enum StateId {
        PrefundReceivingBorrowerData = 0,
        Prefund = 1,
        WaitingForFunding = 2,
        EscrowReceivingBorrowerInfo = 3,
        EscrowReceivingStateSignatures = 4,
        EscrowReceivingEscrowSignatures = 5,
        EscrowSignaturesVerified = 6,
        WaitingForEscrowConfirmation = 7,
    }
}

u8_enum! {
    pub enum MessageId {
        Offer = 0,
        PrefundHints = 1,
        PrefundBorrowerInfo = 2,
        EscrowHints = 3,
        EscrowBorrowerInfo = 4,
        StateSigsFromBorrower = 5,
        StateSigsFromTedO = 6,
        StateSigsFromTedP = 7,
        EscrowSigsFromBorrower = 8,
    }
}

pub enum ParticipantId {
    Verifier = 0,
    Borrower = 1,
    TedO = 2,
    TedP = 3,
}

#[derive(Debug)]
pub struct InvalidEnumValue(u8);
