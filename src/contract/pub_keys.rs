//pub use self::header_bits::*;
use super::participant;
use bitcoin::{key::XOnlyPublicKey, ScriptBuf, key::Keypair, Sequence};
use bitcoin::key::UntweakedPublicKey;
use bitcoin::blockdata::script;
use bitcoin::blockdata::opcodes::all::*;
use core::fmt;
use core::marker::PhantomData;
use super::context;

/// Contains public keys of all participants.
pub struct PubKeys<Contract> {
    pub borrower_eph: PubKey<participant::Borrower, Contract>,
    pub ted_o: PubKey<participant::TedO, Contract>,
    pub ted_p: PubKey<participant::TedP, Contract>,
}

impl<C> Copy for PubKeys<C> { }

crate::test_macros::impl_test_traits!(PubKeys<C>, borrower_eph, ted_o, ted_p);

impl<C> Eq for PubKeys<C> { }

impl<C> PubKeys<C> {
    pub fn new(borrower_eph: PubKey<participant::Borrower, C>, ted_o: PubKey<participant::TedO, C>, ted_p: PubKey<participant::TedP, C>) -> Result<Self, Error> {
        if borrower_eph.0 == ted_o.0 || borrower_eph.0 == ted_p.0 || ted_o.0 == ted_p.0 {
            Err(Error::DuplicateKeys)
        } else {
            Ok(PubKeys {
                borrower_eph,
                ted_o,
                ted_p,
            })
        }
    }

    pub(crate) fn sorted(&self) -> [&XOnlyPublicKey; 3] {
        let mut keys = [&self.borrower_eph.0, &self.ted_o.0, &self.ted_p.0];
        keys.sort();
        keys
    }

    pub fn generate_internal_key(&self) -> UntweakedPublicKey {
        // Hash of "Firefish NUMS 79BE667E F9DCBBAC 55A06295 CE870B07 029BFCDB 2DCE28D9 59F2815B 16F81798\n"
        XOnlyPublicKey::from_slice(&hex_lit::hex!("42bd12e5ccca5b830e755b1e9d7104bdf89819276746d7b5d42cb2a227bff08d")).expect("we statically know the input and it is correct")
    }

    pub fn generate_multisig_script(&self) -> ScriptBuf {
        let keys = self.sorted();
        script::Builder::new()
            .push_x_only_key(&keys[0])
            .push_opcode(OP_CHECKSIGVERIFY)
            .push_x_only_key(&keys[1])
            .push_opcode(OP_CHECKSIGVERIFY)
            .push_x_only_key(&keys[2])
            .push_opcode(OP_CHECKSIG)
            .into_script()
    }

    pub(crate) fn serialize_raw(&self, out: &mut Vec<u8>) {
        self.borrower_eph.serialize_raw(out);
        self.ted_o.serialize_raw(out);
        self.ted_p.serialize_raw(out);
    }

    pub(crate) fn deserialize_raw(bytes: &mut &[u8]) -> Result<Self, RawDeserError> {
        let borrower_eph = PubKey::deserialize_raw(bytes)?;
        let ted_o = PubKey::deserialize_raw(bytes)?;
        let ted_p = PubKey::deserialize_raw(bytes)?;
        Self::new(borrower_eph, ted_o, ted_p).map_err(RawDeserError::DuplicateKeys)
    }
}

crate::test_macros::impl_arbitrary!(PubKeys<C>, borrower_eph, ted_o, ted_p);

#[derive(Debug)]
pub(crate) enum RawDeserError {
    InvalidKey(bitcoin::secp256k1::Error),
    DuplicateKeys(Error),
}

impl From<bitcoin::secp256k1::Error> for RawDeserError {
    fn from(error: bitcoin::secp256k1::Error) -> Self {
        RawDeserError::InvalidKey(error)
    }
}

#[derive(Debug)]
pub enum Error {
    DuplicateKeys,
}

/// Represents a single message in the key echange protocol.
///
/// This message originated from `Sender` and is broadcasted to all other participants.
pub struct PubKey<Sender, Contract>(XOnlyPublicKey, PhantomData<(Sender, Contract)>);

impl<S, C> Copy for PubKey<S, C> {}
impl<S, C> Clone for PubKey<S, C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S, C> Eq for PubKey<S, C> {}
impl<S, C> PartialEq for PubKey<S, C> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<Sender, Contract> PubKey<Sender,Contract> {
    pub fn new(key: XOnlyPublicKey) -> Self {
        PubKey(key, Default::default())
    }

    pub fn from_key_pair(key_pair: &Keypair) -> Self {
        PubKey(key_pair.x_only_public_key().0, Default::default())
    }
}

impl<Sender, Contract> PubKey<Sender,Contract> where Contract: ContractNumber {
    pub fn from_xpub(xpub: &bitcoin::bip32::Xpub, derivation_path: &bitcoin::bip32::DerivationPath) -> Self {
        let derivation_path = derivation_path.extend(&[Contract::CHILD_NUMBER]);
        let key = xpub
            .derive_pub(&secp256k1::SECP256K1, &derivation_path)
            .expect("failed to derive")
            .to_x_only_pub();
        Self::new(key)
    }
}

pub trait ContractNumber {
    const CHILD_NUMBER: bitcoin::bip32::ChildNumber;
}

impl ContractNumber for context::Prefund {
    const CHILD_NUMBER: bitcoin::bip32::ChildNumber = bitcoin::bip32::ChildNumber::Normal { index: 0 };
}

impl ContractNumber for context::Escrow {
    const CHILD_NUMBER: bitcoin::bip32::ChildNumber = bitcoin::bip32::ChildNumber::Normal { index: 1 };
}

impl<P, C> PubKey<P, C> {
    pub fn as_x_only(&self) -> &XOnlyPublicKey {
        &self.0
    }

    pub(crate) fn serialize_raw(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0.serialize())
    }

    pub(crate) fn deserialize_raw(bytes: &mut &[u8]) -> Result<Self, bitcoin::secp256k1::Error> {
        if bytes.len() < 32 {
            return Err(secp256k1::Error::InvalidPublicKey);
        }
        let key = XOnlyPublicKey::from_slice(&bytes[..32])?;
        *bytes = &bytes[32..];
        Ok(PubKey(key, Default::default()))
    }
}

impl PubKey<participant::Borrower, context::Prefund> {
    pub fn borrower_prefund_script(&self, lock_time: Sequence) -> ScriptBuf {
        bitcoin::blockdata::script::Builder::new()
            .push_int(lock_time.to_consensus_u32().into())
            .push_opcode(OP_CSV) // cehck sequence verify
            .push_opcode(OP_DROP) // CSV leaves the item on the stack, even in taproot
            .push_x_only_key(&self.0)
            .push_opcode(OP_CHECKSIG)
            .into_script()
    }
}

impl<P, C> fmt::Debug for PubKey<P, C> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Public key {} of {} contract belonging to {}", self.0, core::any::type_name::<C>(), core::any::type_name::<P>())
    }
}

#[cfg(test)]
impl<P: 'static, C: 'static> quickcheck::Arbitrary for PubKey<P, C> {
    fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
        PubKey::new(crate::test_macros::arbitrary(gen))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pub_keys_sorted() {
        use secp256k1::XOnlyPublicKey;
        use super::{PubKeys, PubKey};

        fn check_sorted(key_a: XOnlyPublicKey, key_b: XOnlyPublicKey, key_c: XOnlyPublicKey) {
            let keys = PubKeys::<super::super::context::Escrow>::new(PubKey::new(key_a), PubKey::new(key_b), PubKey::new(key_c)).unwrap();
            let sorted = keys.sorted();
            assert!(sorted[0] < sorted[1] && sorted[1] < sorted[2]);
        }

        let key_a = XOnlyPublicKey::from_slice(&hex_lit::hex!("0000000000000000000000000000000000000000000000000000000000000001")).unwrap();
        let key_b = XOnlyPublicKey::from_slice(&hex_lit::hex!("0000000000000000000000000000000000000000000000000000000000000002")).unwrap();
        let key_c = XOnlyPublicKey::from_slice(&hex_lit::hex!("0000000000000000000000000000000000000000000000000000000000000003")).unwrap();

        check_sorted(key_a, key_b, key_c);
        check_sorted(key_a, key_c, key_b);
        check_sorted(key_b, key_a, key_c);
        check_sorted(key_b, key_c, key_a);
        check_sorted(key_c, key_a, key_b);
        check_sorted(key_c, key_b, key_a);
    }

    quickcheck::quickcheck! {
        fn pub_keys_roundtrips(keys: super::PubKeys<super::super::context::Escrow>) -> bool {
            let mut bytes = Vec::new();
            keys.serialize_raw(&mut bytes);
            let keys2 = super::PubKeys::deserialize_raw(&mut &*bytes).unwrap();

            keys == keys2
        }
    }
}
