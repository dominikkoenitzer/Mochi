//! UTF-16 helpers. Win32 speaks wide strings, Rust does not.

/// Turns a NUL terminated (or fully filled) UTF-16 buffer into a `String`.
///
/// Anything after the first NUL is dropped. Unpaired surrogates are replaced
/// rather than rejected: a window title is never worth an error.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Turns a Rust string into a NUL terminated UTF-16 vector.
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The file name part of a Windows path, or the whole thing if there is no separator.
pub fn file_name(path: &str) -> &str {
    match path.rfind(['\\', '/']) {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nul_terminates_the_string() {
        let buf = [b'a' as u16, b'b' as u16, 0, b'c' as u16];
        assert_eq!(from_wide(&buf), "ab");
    }

    #[test]
    fn a_full_buffer_without_nul_still_decodes() {
        let buf: Vec<u16> = "hello".encode_utf16().collect();
        assert_eq!(from_wide(&buf), "hello");
    }

    #[test]
    fn umlauts_survive_the_round_trip() {
        let wide = to_wide("Könitzer");
        assert_eq!(*wide.last().unwrap(), 0);
        assert_eq!(from_wide(&wide), "Könitzer");
    }

    #[test]
    fn file_name_takes_the_last_component() {
        assert_eq!(file_name(r"C:\Program Files\Mochi\mochi.exe"), "mochi.exe");
        assert_eq!(file_name("mochi.exe"), "mochi.exe");
        assert_eq!(file_name(""), "");
        assert_eq!(file_name(r"C:\dir\"), "");
    }
}
