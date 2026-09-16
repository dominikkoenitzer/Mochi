//! UTF-16 helpers. Win32 speaks wide strings, Rust does not.

/// Turns a NUL terminated, or completely filled, UTF-16 buffer into a `String`.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Turns a Rust string into a NUL terminated UTF-16 vector.
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// UTF-16 without the terminator, for the draw calls that take a length.
pub fn to_wide_unterminated(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nul_ends_the_string() {
        assert_eq!(from_wide(&[b'a' as u16, b'b' as u16, 0, b'c' as u16]), "ab");
    }

    #[test]
    fn a_full_buffer_without_a_nul_still_decodes() {
        let buf: Vec<u16> = "MochiTest 1".encode_utf16().collect();
        assert_eq!(from_wide(&buf), "MochiTest 1");
    }

    #[test]
    fn umlauts_survive_the_round_trip() {
        let wide = to_wide("Zürich");
        assert_eq!(*wide.last().unwrap(), 0);
        assert_eq!(from_wide(&wide), "Zürich");
        assert_eq!(to_wide_unterminated("Zürich").len(), wide.len() - 1);
    }
}
