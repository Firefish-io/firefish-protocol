pub mod borrower;
pub mod ted;
pub mod ted_o;
pub mod ted_p;

use super::constants;

pub trait Participant {
    const IDENTIFIER: constants::ParticipantId;
    /// Used to identify the participant in string-encoded objects.
    const HUMAN_IDENTIFIER: char;
    type PrefundData;
    type PreEscrowData;
}

pub enum Borrower {}
pub enum TedO {}
pub enum TedP {}

macro_rules! impl_participant {
    ($participant:ty, $module:ident, $identifier:ident, $human_identifier:expr) => {
        impl Participant for $participant {
            const IDENTIFIER: constants::ParticipantId = constants::ParticipantId::$identifier;
            const HUMAN_IDENTIFIER: char = $human_identifier;
            type PrefundData = $module::PrefundData;
            type PreEscrowData = $module::EscrowData;
        }
    }
}

impl_participant!(Borrower, borrower, Borrower, 'b');
impl_participant!(TedO, ted_o, TedO, 'o');
impl_participant!(TedP, ted_p, TedP, 'p');

pub trait PrefundData: Sized {
    type Participant: Participant<PreEscrowData=Self>;

    fn prefund(&self) -> &super::prefund::Prefund<Self::Participant>;
}

#[derive(Default, Debug, Copy, Clone, Eq, PartialEq)]
pub struct NoData;

crate::test_macros::impl_arbitrary!(NoData,);

impl super::Serialize for NoData {
    fn serialize(&self, _buf: &mut Vec<u8>) {}
}

impl super::Deserialize for NoData {
    type Error = core::convert::Infallible;

    fn deserialize(_buf: &mut &[u8], _version: super::deserialize::StateVersion) -> Result<Self, Self::Error> {
        Ok(NoData)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Ted<O, P> {
    O(O),
    P(P),
}

impl<O, P> Ted<O, P> {
    pub fn name(&self) -> &str {
        match self {
            Ted::O(_) => "TED-O",
            Ted::P(_) => "TED-P",
        }
    }
}
