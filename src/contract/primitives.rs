//! Primitives shared by both subcontracts.

use core::marker::PhantomData;
use bitcoin::{OutPoint, ScriptBuf, Sequence, TxOut, TxIn, Witness};

/// Contains all information required to spend an output excluding signatures.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SpendableTxo {
    pub out_point: OutPoint,
    pub tx_out: TxOut,
    pub sequence: Sequence,
}

impl SpendableTxo {
    /// Converts into a tuple `TxOut`, `TxIn` where `TxIn` has empty signature data.
    ///
    /// The resulting tuple represents the connection between two transactions where the `TxOut` is
    /// being spent by the `TxIn`. The signature data, which means `script_sig` and `witness` is
    /// empty.
    pub fn unpack_with_empty_sig(self) -> (TxOut, TxIn) {
        let txin = TxIn {
            previous_output: self.out_point,
            script_sig: ScriptBuf::new(),
            sequence: self.sequence,
            witness: Witness::new(),
        };
        (self.tx_out, txin)
    }

    pub(crate) fn serialize(&self, out: &mut Vec<u8>) {
        use bitcoin::consensus::Encodable;

        self.out_point.consensus_encode(out).expect("vec doesn't error");
        self.tx_out.consensus_encode(out).expect("vec doesn't error");
        self.sequence.consensus_encode(out).expect("vec doesn't error");
    }

    pub(crate) fn deserialize(bytes: &mut &[u8]) -> Result<Self, bitcoin::consensus::encode::Error> {
        use bitcoin::consensus::Decodable;

        let out_point = Decodable::consensus_decode(bytes)?;
        let tx_out = Decodable::consensus_decode(bytes)?;
        let sequence = Decodable::consensus_decode(bytes)?;

        Ok(SpendableTxo { out_point, tx_out, sequence })
    }
}

crate::test_macros::impl_arbitrary!(SpendableTxo, out_point, tx_out, sequence);

/// Shared seed for randomization of transactions.
///
/// To make it harder for chain analysts to identify the transactions belonging to this contract
/// some information in transactions is randomized. To ensure the randomization is deterministic all
/// participants share the same seed. The shared seed is stored inside this type.
#[derive(Copy, Clone)]
pub struct SharedSeed([u8; 32]);

/// Key used by borrower for signing the transaction.
pub struct EphemeralPrivateKey<Contract>(bitcoin::PrivateKey, PhantomData<Contract>);

#[derive(Copy, Clone)]
pub(crate) struct Permutation([KeyIndex; 3]);

impl Permutation {
    pub(crate) fn from_keys<C>(keys: &super::pub_keys::PubKeys<C>) -> Self {
        let sorted = keys.sorted();
        let ted_o_idx = sorted.binary_search(&keys.ted_o.as_x_only()).expect("it's there");
        let ted_p_idx = sorted.binary_search(&keys.ted_p.as_x_only()).expect("it's there");

        let mut permutation = [KeyIndex::Zero, KeyIndex::Zero, KeyIndex::Zero];

        // Zero is implied
        permutation[ted_o_idx] = KeyIndex::One;
        permutation[ted_p_idx] = KeyIndex::Two;

        Permutation(permutation)
    }

    pub(crate) fn permute<T: Copy>(&self, input: [T; 3]) -> [T; 3] {
        [input[self.0[0] as usize], input[self.0[1] as usize], input[self.0[2] as usize]]
    }
}

#[derive(Copy, Clone)]
enum KeyIndex {
    Zero = 0,
    One = 1,
    Two = 2,
}

#[cfg(test)]
mod tests {
    #[test]
    fn permutation() {
        use secp256k1::XOnlyPublicKey;
        let key_a = XOnlyPublicKey::from_slice(&hex_lit::hex!("0000000000000000000000000000000000000000000000000000000000000001")).unwrap();
        let key_b = XOnlyPublicKey::from_slice(&hex_lit::hex!("0000000000000000000000000000000000000000000000000000000000000002")).unwrap();
        let key_c = XOnlyPublicKey::from_slice(&hex_lit::hex!("0000000000000000000000000000000000000000000000000000000000000003")).unwrap();

        fn check_permutation(key_a: XOnlyPublicKey, key_b: XOnlyPublicKey, key_c: XOnlyPublicKey) {
            use super::super::pub_keys::{PubKeys, PubKey};
            // Doesn't matter which contract for this test
            let keys = PubKeys::<super::super::context::Escrow>::new(PubKey::new(key_a), PubKey::new(key_b), PubKey::new(key_c)).unwrap();
            let permutation = super::Permutation::from_keys(&keys);
            let permuted = permutation.permute([&key_a, &key_b, &key_c]);
            assert_eq!(permuted, keys.sorted());
        }

        check_permutation(key_a, key_b, key_c);
        check_permutation(key_a, key_c, key_b);
        check_permutation(key_b, key_a, key_c);
        check_permutation(key_b, key_c, key_a);
        check_permutation(key_c, key_a, key_b);
        check_permutation(key_c, key_b, key_a);
    }

    quickcheck::quickcheck! {
        fn spendable_txo_roundtrips(txo: super::SpendableTxo) -> bool {
            let mut bytes = Vec::new();
            txo.serialize(&mut bytes);
            let mut byte_ref = &*bytes;
            let txo2 = super::SpendableTxo::deserialize(&mut byte_ref).unwrap();
            txo2 == txo && byte_ref.is_empty()
        }
    }
}
