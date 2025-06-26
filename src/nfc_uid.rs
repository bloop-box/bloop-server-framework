use hex::{FromHex, FromHexError, decode_to_slice};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NfcUid(pub [u8; 7]);

impl NfcUid {
    pub fn as_bytes(&self) -> &[u8; 7] {
        &self.0
    }
}

impl FromHex for NfcUid {
    type Error = FromHexError;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let mut out = Self([0; 7]);
        decode_to_slice(hex, &mut out.0)?;
        Ok(out)
    }
}
