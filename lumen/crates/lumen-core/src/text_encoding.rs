//! Best-effort decoding of text whose encoding is not known in advance (a fetched or
//! locally-read playlist, or a pasted/uploaded list of links). Honors a byte-order mark, then
//! tries plain UTF-8, then falls back to encoding detection. Unlike
//! [`crate::subtitles::store::normalise`], this never fails: worst case a handful of characters
//! come out wrong, which is still better than discarding the file's content as a block of
//! replacement characters the way a bare `String::from_utf8_lossy` on non-UTF-8 bytes would.
pub fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if (bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]))
        && let Some((encoding, _)) = encoding_rs::Encoding::for_bom(bytes)
    {
        let (text, _, _) = encoding.decode(bytes);
        return text.into_owned();
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    detector.feed(bytes, true);
    let encoding = detector.guess(None, chardetng::Utf8Detection::Deny);
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_utf8_passes_through() {
        assert_eq!(decode_text("hello, café".as_bytes()), "hello, café");
    }

    #[test]
    fn a_utf16le_bom_is_honored() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "hello".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert_eq!(decode_text(&bytes), "hello");
    }

    #[test]
    fn a_utf16be_bom_is_honored() {
        let mut bytes = vec![0xFE, 0xFF];
        for unit in "hello".encode_utf16() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        assert_eq!(decode_text(&bytes), "hello");
    }

    #[test]
    fn a_utf8_bom_is_stripped() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("hello".as_bytes());
        assert_eq!(decode_text(&bytes), "hello");
    }

    #[test]
    fn non_utf8_bytes_decode_to_something_other_than_a_wall_of_replacement_characters() {
        // "Привет" in Windows-1251, no BOM: not valid UTF-8, and chardetng should recognize it
        // as Cyrillic rather than this falling back to U+FFFD for every byte.
        let bytes: &[u8] = &[0xcf, 0xf0, 0xe8, 0xe2, 0xe5, 0xf2];
        let decoded = decode_text(bytes);
        assert!(
            !decoded.chars().all(|c| c == '\u{FFFD}'),
            "expected a real decode, got: {decoded:?}"
        );
    }
}
