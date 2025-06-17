#[cfg(test)]
use bitcoin::{TxIn, TxOut, OutPoint, Transaction};

macro_rules! impl_arbitrary {
    ($type:ident$(<$($gen:ident),*>)? as custom, $($field:ident),*) => {
        #[cfg(test)]
        impl$(<$($gen: 'static),*>)?  crate::test_macros::qc_help::Arbitrary for $type$(<$($gen),*>)? {
            fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
                $type {
                    $(
                        $field: crate::test_macros::qc_help::Hack::new(0).arbitrary(gen),
                    )*
                }
            }
        }
    };
    ($type:ident$(<$($gen:ident $(: $bound:path)?),*>)? where { $($bound_ty:ty),+ }, $($field:ident),*) => {
        #[cfg(test)]
        impl$(<$($gen: 'static $(+ $bound)?),*>)?  quickcheck::Arbitrary for $type$(<$($gen),*>)? where $($bound_ty: quickcheck::Arbitrary + Clone),+ {
            fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
                $type {
                    $(
                        $field: crate::test_macros::qc_help::Hack::new(0).arbitrary(gen),
                    )*
                }
            }
        }
    };
    ($type:ident$(<$($gen:ident),*>)?, $($field:ident),*) => {
        #[cfg(test)]
        impl$(<$($gen: 'static),*>)?  quickcheck::Arbitrary for $type$(<$($gen),*>)? {
            fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
                let _ = &gen;
                $type {
                    $(
                        $field: crate::test_macros::qc_help::Hack::new(0).arbitrary(gen),
                    )*
                }
            }
        }
    };
}
pub(crate) use impl_arbitrary;

impl_arbitrary!(TxIn as custom, previous_output, script_sig, sequence, witness);
impl_arbitrary!(TxOut as custom, value, script_pubkey);
impl_arbitrary!(OutPoint as custom, txid, vout);
impl_arbitrary!(Transaction as custom, version, input, output, lock_time);

/// Implements `Debug`, `PartialEq` and `Clone`
macro_rules! impl_test_traits {
    ($type:ident$(<$($gen:ident $(: $gen_bound:path)?),*>)? where { $($bound_ty:ty),* }, $($field:ident),*) => {
        impl$(<$($gen $(: $gen_bound)?),*>)? core::fmt::Debug for $type$(<$($gen),*>)? where $($bound_ty: core::fmt::Debug),* {
            fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                f.debug_struct(stringify!($type))
                    $(
                        .field(stringify!($field), &self.$field)
                    )*
                    .finish()
            }
        }

        impl$(<$($gen $(: $gen_bound)?),*>)? PartialEq for $type$(<$($gen),*>)? where $($bound_ty: PartialEq),* {
            fn eq(&self, other: &Self) -> bool {
                true
                $(
                    || self.$field == other.$field
                )*
            }
        }

        impl$(<$($gen $(: $gen_bound)?),*>)? Clone for $type$(<$($gen),*>)? where $($bound_ty: Clone),* {
            fn clone(&self) -> Self {
                $type {
                    $(
                        $field: self.$field.clone(),
                    )*
                }
            }
        }
    };
    ($type:ident$(<$($gen:ident),*>)?, $($field:ident),*) => {
        crate::test_macros::impl_test_traits!($type$(<$($gen),*>)? where {}, $($field),*);
    };
}
pub(crate) use impl_test_traits;

#[cfg(test)]
macro_rules! check_roundtrip {
    ($name:ident, $ty:ty) => {
        mod $name {
            #[allow(unused)]
            use super::*;
            quickcheck::quickcheck! {
                fn roundtrip(val: $ty) -> bool {
                    let mut bytes = Vec::new();
                    val.serialize(&mut bytes);
                    let val2 = <$ty>::deserialize(&mut &*bytes).unwrap();

                    assert_eq!(val2, val);
                    true
                }
            }

            quickcheck::quickcheck! {
                fn garbage(val: $ty, modify: Vec<(usize, u8)>, insert: Vec<(usize, u8)>, delete: Vec<usize>) -> bool {
                    let mut bytes = Vec::new();
                    val.serialize(&mut bytes);
                    if !bytes.is_empty() {
                        for (pos, byte) in modify {
                            let pos = pos % bytes.len();
                            bytes[pos] = byte;
                        }
                    }

                    for (pos, byte) in insert {
                        let pos = pos % (bytes.len() + 1);
                        bytes.insert(pos, byte);
                    }

                    for pos in delete {
                        if bytes.is_empty() {
                            break
                        }
                        let pos = pos % bytes.len();
                        bytes.remove(pos);
                    }

                    let _ = <$ty>::deserialize(&mut &*bytes);

                    true
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) use check_roundtrip;

#[cfg(test)]
macro_rules! check_roundtrip_with_version {
    ($name:ident, $ty:ty) => {
        quickcheck::quickcheck! {
            fn $name(val: $ty) -> bool {
                let mut bytes = Vec::new();
                val.serialize(&mut bytes);
                let val2 = <$ty>::deserialize(&mut &*bytes, crate::contract::deserialize::StateVersion::CURRENT).unwrap();

                assert_eq!(val2, val);
                true
            }
        }
    }
}

#[cfg(test)]
pub(crate) use check_roundtrip_with_version;

/// Module containing a horribly-looking hack to seamlessly implement `Arbitrary`.
///
/// What we want to achieve is to have `impl_arbitrary!` macro where we only define the name of the
/// struct we want to implement `Arbitrary` for and the list of fields. We don't want to repeat
/// field types.
///
/// Since we don't want to repeat the field types we have to rely on inference and because Rust
/// lacks specialization we have to somehow resolve potential conflict during inference. To solve
/// this we define a trait on arbitrary integers and abuse the fallback to `i32` to pick the
/// preferred impl - in this case upstream since we assume they can do something better.
///
/// The usage can be seen in `impl_arbitrary` macro. It shouldn't be needed outside of macros.
#[cfg(test)]
pub(crate) mod qc_help {
    /// Our version of the `Arbitrary` trait.
    ///
    /// The compiler allows us to impl this for foreign types.
    pub(crate) trait Arbitrary: 'static + Sized {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self;
    }

    impl Arbitrary for bitcoin::ScriptBuf {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            bitcoin::ScriptBuf::from(<Vec<u8> as quickcheck::Arbitrary>::arbitrary(gen))
        }
    }

    impl Arbitrary for bitcoin::Sequence {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            use quickcheck::Arbitrary;
            bitcoin::Sequence::from_consensus(u32::arbitrary(gen))
        }
    }

    impl Arbitrary for bitcoin::transaction::Version {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            use quickcheck::Arbitrary;
            bitcoin::transaction::Version(i32::arbitrary(gen))
        }
    }


    impl Arbitrary for bitcoin::Txid {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            use quickcheck::Arbitrary;
            use bitcoin::hashes::Hash;

            let mut txid = [0u8; 32];
            for byte in &mut txid {
                *byte = u8::arbitrary(gen);
            }
            Hash::from_byte_array(txid)
        }
    }

    impl Arbitrary for bitcoin::Amount {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            use quickcheck::Arbitrary;

            bitcoin::Amount::from_sat(u64::arbitrary(gen))
        }
    }

    impl Arbitrary for bitcoin::FeeRate {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            use quickcheck::Arbitrary;

            bitcoin::FeeRate::from_sat_per_kwu(u64::arbitrary(gen))
        }
    }

    impl Arbitrary for bitcoin::locktime::absolute::Height {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            use quickcheck::Arbitrary;

            loop {
                if let Ok(height) = bitcoin::locktime::absolute::Height::from_consensus(u32::arbitrary(gen)) {
                    break height;
                }
            }
        }
    }

    impl Arbitrary for bitcoin::taproot::TapNodeHash {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            use quickcheck::Arbitrary;
            use bitcoin::hashes::Hash;

            let mut txid = [0u8; 32];
            for byte in &mut txid {
                *byte = u8::arbitrary(gen);
            }
            Hash::from_byte_array(txid)
        }
    }

    impl<T: Arbitrary> Arbitrary for [T; 2] {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            [T::arbitrary(gen), T::arbitrary(gen)]
        }
    }

    impl<T: Arbitrary> Arbitrary for Vec<T> {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            use quickcheck::Arbitrary;

            let size = loop {
                let size = usize::arbitrary(gen);
                if size < 20 {
                    break size;
                }
            };
            let mut vec = Vec::with_capacity(size);
            for _ in 0..size {
                vec.push(T::arbitrary(gen));
            }
            vec
        }
    }

    impl Arbitrary for bitcoin::Network {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            use bitcoin::Network;
            *gen.choose(&[Network::Bitcoin, Network::Testnet, Network::Regtest, Network::Signet]).unwrap()
        }
    }

    impl Arbitrary for bitcoin::absolute::LockTime {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            use quickcheck::Arbitrary;
            bitcoin::absolute::LockTime::from_consensus(u32::arbitrary(gen))
        }
    }

    impl Arbitrary for secp256k1::XOnlyPublicKey {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            use quickcheck::Arbitrary;

            let mut buf = [0u8; 32];
            loop {
                for byte in &mut buf {
                    *byte = u8::arbitrary(gen);
                }
                if let Ok(key) = secp256k1::XOnlyPublicKey::from_slice(&buf) {
                    break key;
                }
            }
        }
    }

    impl Arbitrary for secp256k1::Parity {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            *gen.choose(&[secp256k1::Parity::Even, secp256k1::Parity::Odd]).unwrap()
        }
    }

    impl Arbitrary for bitcoin::key::TweakedPublicKey {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            // not dangerous in prop tests
            bitcoin::key::TweakedPublicKey::dangerous_assume_tweaked(Arbitrary::arbitrary(gen))
        }
    }

    impl Arbitrary for bitcoin::key::Keypair {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            use quickcheck::Arbitrary;

            let mut buf = [0u8; 32];
            loop {
                for byte in &mut buf {
                    *byte = u8::arbitrary(gen);
                }
                if let Ok(key) = bitcoin::key::Keypair::from_seckey_slice(secp256k1::SECP256K1, &buf) {
                    break key;
                }
            }
        }
    }

    impl Arbitrary for bitcoin::Witness {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            let vec: Vec<Vec<u8>> = quickcheck::Arbitrary::arbitrary(gen);
            bitcoin::Witness::from_slice(&vec)
        }
    }

    impl Arbitrary for secp256k1::schnorr::Signature {
        fn arbitrary(gen: &mut quickcheck::Gen) -> Self {
            use quickcheck::Arbitrary;

            let mut buf = [0u8; 64];
            loop {
                for byte in &mut buf {
                    *byte = u8::arbitrary(gen);
                }
                if let Ok(signature) = secp256k1::schnorr::Signature::from_slice(&buf) {
                    break signature;
                }
            }
        }
    }

    /// This ZST handles dispatch to the appropriate trait.
    pub(crate) struct Hack<T>(core::marker::PhantomData<T>);

    impl<T> Hack<T> {
        /// Create the value.
        ///
        /// The value is unused, we just want the compiler to use `{integer}` for `T`.
        pub(crate) fn new(_: T) -> Self {
            Hack(Default::default())
        }

        /// Generate arbitrary value.
        pub(crate) fn arbitrary<U>(&self, gen: &mut quickcheck::Gen) -> U where T: HorribleArbitrary<U> {
            T::horrible_arbitrary(gen)
        }
    }

    /// Arbitrary trait that uses `Self` as marker type only.
    ///
    /// This trait is implemented for all `i32` and `u8` depending on which trait `T` implements.
    pub(crate) trait HorribleArbitrary<T> {
        fn horrible_arbitrary(gen: &mut quickcheck::Gen) -> T;
    }

    impl<T: quickcheck::Arbitrary> HorribleArbitrary<T> for i32 {
        fn horrible_arbitrary(gen: &mut quickcheck::Gen) -> T {
            T::arbitrary(gen)
        }
    }

    impl<T: Arbitrary> HorribleArbitrary<T> for u8 {
        fn horrible_arbitrary(gen: &mut quickcheck::Gen) -> T {
            T::arbitrary(gen)
        }
    }

    impl<T: 'static> Arbitrary for core::marker::PhantomData<T> {
        fn arbitrary(_: &mut quickcheck::Gen) -> Self {
            Default::default()
        }
    }
}

/// Abbreviation for out arbitrary trait.
#[cfg(test)]
pub(crate) fn arbitrary<T: qc_help::Arbitrary>(gen: &mut quickcheck::Gen) -> T {
    T::arbitrary(gen)
}
