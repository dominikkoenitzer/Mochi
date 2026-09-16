//! Framing for everything Mochi sends over a pipe.
//!
//! The wire format is newline delimited JSON:
//!
//! * one JSON value per message, serialised without embedded newlines,
//! * terminated by a single `\n` (never `\r\n`),
//! * UTF-8, no BOM, no length prefix.
//!
//! That makes the protocol trivial to speak from PowerShell or Python and easy
//! to eyeball in a log. Empty lines are skipped so a client may send `\n` as a
//! cheap keepalive.

use std::io::{BufRead, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Largest line the readers accept, to keep a confused peer from eating all
/// available memory. 8 MiB is far more than any realistic state dump.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// Serialises `value` as one newline terminated JSON line and flushes.
pub fn write_message<W: Write, T: Serialize>(writer: &mut W, value: &T) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    debug_assert!(
        !line.contains(&b'\n'),
        "serde_json::to_vec never emits newlines"
    );
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()
}

/// Reads one newline terminated JSON line.
///
/// Returns `Ok(None)` at end of stream. Blank lines are skipped.
pub fn read_message<R: BufRead, T: DeserializeOwned>(reader: &mut R) -> std::io::Result<Option<T>> {
    match read_line(reader)? {
        None => Ok(None),
        Some(line) => {
            let value = serde_json::from_str(&line).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("malformed message: {e}"),
                )
            })?;
            Ok(Some(value))
        }
    }
}

/// Reads one non-empty line, trimming the trailing `\n` and any stray `\r`.
///
/// Returns `Ok(None)` at end of stream.
pub fn read_line<R: BufRead>(reader: &mut R) -> std::io::Result<Option<String>> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = {
            let mut limited = std::io::Read::take(&mut *reader, MAX_MESSAGE_BYTES as u64);
            limited.read_line(&mut line)?
        };
        if read == 0 {
            return Ok(None);
        }
        if read >= MAX_MESSAGE_BYTES && !line.ends_with('\n') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "message exceeds the maximum line length",
            ));
        }
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        if !trimmed.is_empty() {
            return Ok(Some(trimmed.to_owned()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, Direction, Response};

    #[test]
    fn a_message_is_one_json_line() {
        let mut buf = Vec::new();
        write_message(
            &mut buf,
            &Command::Focus {
                direction: Direction::Left,
            },
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "{\"cmd\":\"focus\",\"direction\":\"left\"}\n"
        );
    }

    #[test]
    fn write_then_read_round_trips() {
        let mut buf = Vec::new();
        write_message(&mut buf, &Command::State).unwrap();
        write_message(&mut buf, &Command::Stop { whkd: true }).unwrap();

        let mut reader = std::io::BufReader::new(buf.as_slice());
        assert_eq!(
            read_message::<_, Command>(&mut reader).unwrap(),
            Some(Command::State)
        );
        assert_eq!(
            read_message::<_, Command>(&mut reader).unwrap(),
            Some(Command::Stop { whkd: true })
        );
        assert_eq!(read_message::<_, Command>(&mut reader).unwrap(), None);
    }

    #[test]
    fn blank_lines_and_crlf_are_tolerated() {
        let raw = "\n\r\n{\"cmd\":\"retile\"}\r\n\n";
        let mut reader = std::io::BufReader::new(raw.as_bytes());
        assert_eq!(
            read_message::<_, Command>(&mut reader).unwrap(),
            Some(Command::Retile)
        );
        assert_eq!(read_message::<_, Command>(&mut reader).unwrap(), None);
    }

    #[test]
    fn garbage_is_an_invalid_data_error_not_a_panic() {
        let mut reader = std::io::BufReader::new(&b"not json\n"[..]);
        let err = read_message::<_, Command>(&mut reader).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn an_oversized_line_is_refused_and_the_stream_stays_usable() {
        // A peer that never sends a newline must not be able to make the reader
        // allocate without bound.
        let mut raw = vec![b'x'; MAX_MESSAGE_BYTES + 32];
        raw.extend_from_slice(b"\n{\"cmd\":\"retile\"}\n");
        let mut reader = std::io::BufReader::new(raw.as_slice());

        let err = read_message::<_, Command>(&mut reader).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("maximum line length"), "{err}");

        // The rest of the oversized line is junk, then the stream resynchronises
        // on the next newline, which is what keeps a connection usable.
        assert_eq!(
            read_message::<_, Command>(&mut reader).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(
            read_message::<_, Command>(&mut reader).unwrap(),
            Some(Command::Retile)
        );
    }

    #[test]
    fn a_half_line_at_end_of_stream_is_an_error_not_a_hang() {
        let mut reader = std::io::BufReader::new(&br#"{"cmd":"sta"#[..]);
        assert_eq!(
            read_message::<_, Command>(&mut reader).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(read_message::<_, Command>(&mut reader).unwrap(), None);
    }

    #[test]
    fn responses_use_the_same_framing() {
        let mut buf = Vec::new();
        write_message(&mut buf, &Response::error("no such monitor")).unwrap();
        let mut reader = std::io::BufReader::new(buf.as_slice());
        let got: Response = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(got.error_message(), Some("no such monitor"));
    }
}
