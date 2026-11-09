//! SHA-256 and HMAC-SHA256 through `ring`, in the shape the request signers use.

use ring::{digest, hmac};

pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(digest::digest(&digest::SHA256, data).as_ref());
    out
}

pub(crate) fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data).as_ref());
    out
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes { text.push(DIGITS[(byte >> 4) as usize] as char); text.push(DIGITS[(byte & 15) as usize] as char); }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn bytes(length: usize) -> Vec<u8> { (0..length).map(|index| (index * 31 + 7) as u8).collect() }

    #[test]
    fn digests_agree_with_sha2_around_every_padding_boundary() {
        for length in (0..300).chain([1000, 4096, 4097]) {
            let data = bytes(length);
            assert_eq!(sha256(&data).as_slice(), Sha256::digest(&data).as_slice(), "length {length}");
        }
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn hmac_matches_rfc_4231_and_handles_long_keys() {
        assert_eq!(hex(&hmac_sha256(&[0x0b; 20], b"Hi There")), "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
        assert_eq!(hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        // A key longer than the block is hashed first (RFC 4231 test case 6).
        assert_eq!(
            hex(&hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
        );
    }
}
