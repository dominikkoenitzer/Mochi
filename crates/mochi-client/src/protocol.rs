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
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        // `read_until`, not `read_line`. `read_line` consumes the bytes and
        // validates UTF-8 only afterwards, and on failure it hands back nothing
        // that says whether the newline was among the bytes it ate. Discarding
        // to the next newline on the guess that it was not swallowed the WHOLE
        // of the next message when it was: one stray non-UTF-8 byte on its own
        // line cost the message that followed it, and from then on every
        // response was attributed to the command after the one it answered.
        // Reading the raw bytes makes the question answerable: the newline is
        // there or it is not.
        let read = {
            let mut limited = std::io::Read::take(&mut *reader, MAX_MESSAGE_BYTES as u64);
            limited.read_until(b'\n', &mut bytes)?
        };
        if read == 0 {
            return Ok(None);
        }

        if bytes.last() != Some(&b'\n') {
            // No newline, so this is not a whole message, and there are exactly
            // two ways to get here.
            if read >= MAX_MESSAGE_BYTES {
                // Over-long: the peer is still mid-line. Everything up to the
                // next newline is the rest of that one message and is thrown
                // away rather than left in the stream, because starting the
                // next read where the limit happened to fall would frame the
                // tail as a message of its own, and `MAX_MESSAGE_BYTES` of
                // padding followed by a stop command is one line on the wire
                // that must never become a command.
                discard_to_newline(reader)?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "message exceeds the maximum line length",
                ));
            }
            // Or the stream ended mid-line. A truncated write is not a message
            // either: a stop command with its newline lost is not an
            // instruction to stop, and it used to be obeyed as one.
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the stream ended in the middle of a message",
            ));
        }

        // A whole line in hand, newline included, so a failure here needs
        // nothing discarded: the stream already sits at the next message.
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "message is not valid UTF-8",
            ));
        };

        let trimmed = text.trim_end_matches('\n').trim_end_matches('\r');
        if !trimmed.is_empty() {
            return Ok(Some(trimmed.to_owned()));
        }
    }
}

/// Throws away bytes up to and including the next `\n`.
///
/// Nothing is buffered, so the peer cannot make this allocate. Returns at end
/// of stream as well, in which case the next read reports end of stream too.
fn discard_to_newline<R: BufRead>(reader: &mut R) -> std::io::Result<()> {
    loop {
        let (found, used) = {
            let buf = match reader.fill_buf() {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if buf.is_empty() {
                return Ok(());
            }
            match buf.iter().position(|b| *b == b'\n') {
                Some(at) => (true, at + 1),
                None => (false, buf.len()),
            }
        };
        reader.consume(used);
        if found {
            return Ok(());
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
        write_message(&mut buf, &Command::Stop).unwrap();

        let mut reader = std::io::BufReader::new(buf.as_slice());
        assert_eq!(
            read_message::<_, Command>(&mut reader).unwrap(),
            Some(Command::State)
        );
        assert_eq!(
            read_message::<_, Command>(&mut reader).unwrap(),
            Some(Command::Stop)
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

        // The rest of the oversized line went with it, so the stream is back on
        // a message boundary and the next real message reads cleanly, which is
        // what keeps a connection usable.
        assert_eq!(
            read_message::<_, Command>(&mut reader).unwrap(),
            Some(Command::Retile)
        );
    }

    #[test]
    fn the_tail_of_an_oversized_line_never_becomes_a_message() {
        // One unterminated line on the wire: padding, then something that looks
        // like a command, then the only newline. A reader that starts a fresh
        // window where the size limit hit would hand the second half to its
        // caller as a message the peer never framed, which on the command pipe
        // means stopping the window manager.
        let mut raw = vec![b'x'; MAX_MESSAGE_BYTES];
        raw.extend_from_slice(br#"{"cmd":"stop"}"#);
        raw.push(b'\n');
        let mut reader = std::io::BufReader::new(raw.as_slice());

        let err = read_message::<_, Command>(&mut reader).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            read_message::<_, Command>(&mut reader).unwrap(),
            None,
            "the tail of an over-long line was framed as a command"
        );
    }

    #[test]
    fn a_bad_line_does_not_swallow_the_good_one_after_it() {
        // The half that was missed. The existing test covers an UNTERMINATED
        // bad line, where discarding to the next newline is right. This is the
        // terminated one: the newline was already eaten with the bad byte, so
        // discarding again ate the whole of the next message. A pipelining
        // script lost a command silently and then read every response one
        // command out of step.
        let mut reader = std::io::BufReader::new(&b"\xFF\n{\"cmd\":\"retile\"}\n"[..]);

        let first = read_line(&mut reader);
        assert!(first.is_err(), "a non-UTF-8 line must be refused");

        let second = read_line(&mut reader)
            .expect("the stream should still be framed")
            .expect("the message after it is still there");
        assert_eq!(
            second, "{\"cmd\":\"retile\"}",
            "the good line was swallowed"
        );
    }

    #[test]
    fn a_message_without_its_newline_is_not_a_message() {
        // A truncated write is not an instruction. This one used to be obeyed:
        // the length guard only fired for an over-long line, so a SHORT line
        // that ended at EOF fell straight through and was executed.
        let mut reader = std::io::BufReader::new(&b"{\"cmd\":\"stop\"}"[..]);
        assert!(
            read_line(&mut reader).is_err(),
            "an unterminated line was accepted as a whole message"
        );

        // And a properly terminated one is still accepted, of course.
        let mut good = std::io::BufReader::new(&b"{\"cmd\":\"stop\"}\n"[..]);
        assert_eq!(
            read_line(&mut good).expect("reads").expect("a message"),
            "{\"cmd\":\"stop\"}"
        );
    }

    #[test]
    fn a_line_that_is_not_utf8_does_not_desync_the_framing_either() {
        // The same attack, with one byte that is not UTF-8 in the padding.
        // `read_line` consumes the bytes and validates them afterwards, so it
        // failed with the reader left mid-line and the guard below it skipped:
        // the tail was then framed as a message of its own, and on the command
        // pipe that means stopping the window manager on a command the peer
        // never sent.
        let mut raw = vec![0xFF; MAX_MESSAGE_BYTES];
        raw.extend_from_slice(br#"{"cmd":"stop"}"#);
        raw.push(b'\n');
        let mut reader = std::io::BufReader::new(raw.as_slice());

        let err = read_message::<_, Command>(&mut reader).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            read_message::<_, Command>(&mut reader).unwrap(),
            None,
            "the tail of a line that was not utf-8 was framed as a command"
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
