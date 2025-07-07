use hex::{FromHex, FromHexError, decode_to_slice};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Invalid UID length, must be 4, 7 or 10")]
    InvalidLength,
}

/// Represents an NFC UID of standard lengths.
///
/// NFC UIDs come in three typical sizes:
///
/// - `Single`: 4 bytes
/// - `Double`: 7 bytes
/// - `Triple`: 10 bytes
///
/// This enum encapsulates these variants and ensures correct sizing during
/// construction via `TryFrom<&[u8]>` or hex parsing (`FromHex`).
///
/// Use [`as_bytes()`] to get a byte slice of the underlying UID.
///
/// # Examples
///
/// ```
/// use bloop_server_framework::nfc_uid::NfcUid;
///
/// let uid = NfcUid::try_from(&[0x01, 0x02, 0x03, 0x04][..]).unwrap();
/// assert_eq!(uid.as_bytes(), &[0x01, 0x02, 0x03, 0x04]);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum NfcUid {
    Single([u8; 4]),
    Double([u8; 7]),
    Triple([u8; 10]),
}

impl Default for NfcUid {
    fn default() -> Self {
        Self::Single(Default::default())
    }
}

impl TryFrom<&[u8]> for NfcUid {
    type Error = Error;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        match value.len() {
            4 => Ok(NfcUid::Single(value.try_into().unwrap())),
            7 => Ok(NfcUid::Double(value.try_into().unwrap())),
            10 => Ok(NfcUid::Triple(value.try_into().unwrap())),
            _ => Err(Error::InvalidLength),
        }
    }
}

impl FromHex for NfcUid {
    type Error = FromHexError;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let hex_bytes = hex.as_ref();
        let mut decoded = vec![0u8; hex_bytes.len() / 2];
        decode_to_slice(hex, &mut decoded)?;

        NfcUid::try_from(decoded.as_slice()).map_err(|_| FromHexError::InvalidStringLength)
    }
}

impl NfcUid {
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            NfcUid::Single(data) => data,
            NfcUid::Double(data) => data,
            NfcUid::Triple(data) => data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex::FromHex;

    #[test]
    fn from_valid_bytes() {
        let bytes4 = [0x01, 0x02, 0x03, 0x04];
        let bytes7 = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07];
        let bytes10 = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A];

        let uid4 = NfcUid::try_from(&bytes4[..]).unwrap();
        let uid7 = NfcUid::try_from(&bytes7[..]).unwrap();
        let uid10 = NfcUid::try_from(&bytes10[..]).unwrap();

        assert_eq!(uid4.as_bytes(), &bytes4);
        assert_eq!(uid7.as_bytes(), &bytes7);
        assert_eq!(uid10.as_bytes(), &bytes10);
    }

    #[test]
    fn from_invalid_bytes_length() {
        let bytes = [0x01, 0x02];
        let err = NfcUid::try_from(&bytes[..]).unwrap_err();
        assert!(matches!(err, Error::InvalidLength));

        let bytes = [0u8; 5];
        let err = NfcUid::try_from(&bytes[..]).unwrap_err();
        assert!(matches!(err, Error::InvalidLength));
    }

    #[test]
    fn from_valid_hex() {
        let hex4 = "01020304";
        let hex7 = "01020304050607";
        let hex10 = "0102030405060708090a";

        let uid4 = NfcUid::from_hex(hex4).unwrap();
        let uid7 = NfcUid::from_hex(hex7).unwrap();
        let uid10 = NfcUid::from_hex(hex10).unwrap();

        assert_eq!(uid4.as_bytes(), &[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(uid7.as_bytes(), &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07]);
        assert_eq!(
            uid10.as_bytes(),
            &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a]
        );
    }

    #[test]
    fn from_invalid_hex_format() {
        let invalid_hex = "xyz"; // Invalid hex
        assert!(NfcUid::from_hex(invalid_hex).is_err());

        let invalid_length = "010203"; // 3 bytes → invalid UID length
        let result = NfcUid::from_hex(invalid_length);
        assert!(matches!(result, Err(FromHexError::InvalidStringLength)));
    }
}
