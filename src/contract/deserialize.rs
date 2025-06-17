use core::convert::{TryFrom, TryInto};

pub(crate) trait Int {
    type Bytes: Sized + for<'a> TryFrom<&'a [u8]>;

    fn from_be_bytes(bytes: Self::Bytes) -> Self;
    fn from_le_bytes(bytes: Self::Bytes) -> Self;
}

macro_rules! impl_int {
    ($($type:ty),*) => {
        $(
            impl Int for $type {
                type Bytes = [u8; core::mem::size_of::<$type>()];

                fn from_be_bytes(bytes: Self::Bytes) -> Self {
                    <$type>::from_be_bytes(bytes)
                }

                fn from_le_bytes(bytes: Self::Bytes) -> Self {
                    <$type>::from_le_bytes(bytes)
                }
            }
        )*
    }
}

impl_int!(u16, u32, u64);

pub(crate) fn be<T: Int>(bytes: &mut &[u8]) -> Result<T, UnexpectedEnd> {
    if bytes.len() < core::mem::size_of::<T::Bytes>() {
        return Err(UnexpectedEnd);
    }
    let byte_arr: T::Bytes = bytes[..core::mem::size_of::<T::Bytes>()].try_into().map_err(|_| UnexpectedEnd)?;
    *bytes = &bytes[core::mem::size_of::<T::Bytes>()..];
    Ok(T::from_be_bytes(byte_arr))
}

pub(crate) fn le<T: Int>(bytes: &mut &[u8]) -> Result<T, UnexpectedEnd> {
    if bytes.len() < core::mem::size_of::<T::Bytes>() {
        return Err(UnexpectedEnd);
    }
    let byte_arr: T::Bytes = bytes[..core::mem::size_of::<T::Bytes>()].try_into().map_err(|_| UnexpectedEnd)?;
    *bytes = &bytes[core::mem::size_of::<T::Bytes>()..];
    Ok(T::from_le_bytes(byte_arr))
}

pub(crate) fn signature(bytes: &mut &[u8]) -> Result<secp256k1::schnorr::Signature, secp256k1::Error> {
    if bytes.len() < 64 {
        return Err(secp256k1::Error::InvalidSignature);
    }
    let result = secp256k1::schnorr::Signature::from_slice(&bytes[..64]);
    *bytes = &bytes[64..];
    result
}

pub(crate) fn key_pair(bytes: &mut &[u8]) -> Result<secp256k1::Keypair, secp256k1::Error> {
    if bytes.len() < 32 {
        return Err(secp256k1::Error::InvalidSecretKey);
    }
    let result = secp256k1::Keypair::from_seckey_slice(secp256k1::SECP256K1, &bytes[..32]);
    *bytes = &bytes[32..];
    result
}

pub(crate) fn magic(bytes: &mut &[u8]) -> Result<bitcoin::p2p::Magic, UnexpectedEnd> {
    match bytes.get(..4) {
        Some(magic) => {
            *bytes = &bytes[4..];
            Ok(bitcoin::p2p::Magic::from_bytes(magic.try_into().expect("statically valid")))
        },
        None => Err(UnexpectedEnd),
    }
}

#[derive(Debug)]
pub(crate) struct UnexpectedEnd;

/// Just to avoid duplicating version values (SSOT).
macro_rules! version_enum {
    (pub enum $name:ident { $($variant:ident = $value:expr),* $(,)? }) => {
        #[must_use = "Protect the code against forgetting to handle new variants"]
        #[derive(Copy, Clone, Eq, PartialEq, Debug)]
        pub enum $name {
            $($variant = $value,)*
        }

        impl $name {
            pub const fn from_num(num: u32) -> Option<Self> {
                match num {
                    $(
                        $value => Some(Self::$variant),
                    )*
                    _ => None,
                }
            }
        }
    }
}
pub(crate) use version_enum;

version_enum! {
    pub enum StateVersion {
        V0 = 0x00,
        V1 = 0x01,
    }
}

impl StateVersion {
    pub const CURRENT: Self = Self::V1;

    /// Deserializes state version.
    ///
    /// # Legacy version handling
    ///
    /// The version number was present in the initial release which poses a challenge when
    /// deserializing state files because we figured we do need some backwards compatibility. To
    /// resolve this we observe that all state files store the participant ID in the first byte.
    /// This ID is one of few valid values - it does not use the full range of `u8`. We can use
    /// this to flag new serializations with some byte that is not a participant ID. Naturally we
    /// pick 255, the highest possible number.
    ///
    /// So all new state files start with 255 followed by 4-byte big endian version number. When a
    /// non-255 byte is encountered the cursor doesn't move and version 0 is assumed.
    ///
    /// All serializations serialize the new format. We do not attempt to make old clients
    /// compatible because they are (currently) all up to date.
    pub fn deserialize(bytes: &mut &[u8]) -> Result<Self, StateVersionDeserError> {
        if *bytes.get(0).ok_or(UnexpectedEnd)? == 255 {
            *bytes = &bytes[1..];
            let num = crate::contract::deserialize::be::<u32>(bytes)?;
            Self::from_num(num).ok_or(StateVersionDeserError::UnsupportedVersion(num))
        } else {
            Ok(StateVersion::V0)
        }
    }

    /// Serializes the state version including the initial 255 byte.
    ///
    /// See [`Self::deserialize`] for information about serialization.
    pub fn serialize(self, out: &mut Vec<u8>) {
        out.reserve(1 + 4);
        out.push(255);
        out.extend_from_slice(&(self as u32).to_be_bytes());
    }
}

/// Error returned when deserializing version number fails.
pub enum StateVersionDeserError {
    /// The input data is too short.
    UnexpectedEnd,
    /// The version number is not supported (currently always higher).
    UnsupportedVersion(u32),
}

impl From<crate::contract::deserialize::UnexpectedEnd> for StateVersionDeserError {
    fn from(_: crate::contract::deserialize::UnexpectedEnd) -> Self {
        Self::UnexpectedEnd
    }
}
