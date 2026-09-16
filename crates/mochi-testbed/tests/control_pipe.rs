//! The control pipe under the conditions a second shell actually produces: a
//! client that connects and then says nothing, one that hangs up in the middle
//! of a command, one that sends nonsense, and two that arrive at once.
//!
//! Every test serves on its own private pipe name, so none of this touches a
//! `mochi-testwin` host the user may have running on the default name, and two
//! of these tests can run in parallel like any other cargo test.

#![cfg(windows)]

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mochi_testbed::TestWindows;
use mochi_testbed::control::{self, Request, Response, SpawnRequest};

/// Long enough for a loaded machine, short enough that a hang is not a hang.
const DEADLINE: Duration = Duration::from_secs(10);

/// A pipe name no other test and no running host uses.
fn private_pipe() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    format!(
        r"\\.\pipe\mochi-testbed-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}

/// A server that answers `Ping` and nothing else, and counts what it saw.
fn serve_counting(pipe: &str) -> Arc<AtomicU32> {
    let seen = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&seen);
    control::serve_on(pipe, move |request| {
        counted.fetch_add(1, Ordering::SeqCst);
        match request {
            Request::Ping => Response::ok(),
            _ => Response::failed("this test host only answers ping"),
        }
    })
    .expect("serve on a private pipe");
    seen
}

/// A raw client connection, the way a foreign tool would open the pipe.
///
/// Retries while every instance is busy: the listener creates the next instance
/// immediately after accepting, so busy is a matter of microseconds.
fn raw_client(pipe: &str) -> std::fs::File {
    let deadline = Instant::now() + DEADLINE;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe)
        {
            Ok(file) => return file,
            Err(e) if Instant::now() < deadline => {
                assert!(
                    e.raw_os_error() == Some(231) || e.raw_os_error() == Some(2),
                    "opening {pipe} failed with something other than busy: {e}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("cannot open {pipe}: {e}"),
        }
    }
}

/// One line of the answer, without the newline.
fn read_reply(pipe: &mut std::fs::File) -> String {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while let Ok(read) = pipe.read(&mut byte) {
        if read == 0 || byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
    }
    String::from_utf8_lossy(&line).into_owned()
}

fn skip_without_desktop() -> bool {
    let empty = mochi_testbed::monitors().is_ok_and(|monitors| monitors.is_empty());
    if empty {
        eprintln!("skipped: this session has no monitors");
    }
    empty
}

#[test]
fn a_client_that_connects_and_stalls_does_not_block_the_next_one() {
    let pipe = private_pipe();
    let seen = serve_counting(&pipe);

    // Connected, and deliberately silent: the worker thread that took this
    // connection is now blocked in ReadFile for as long as this test runs.
    let stalled = raw_client(&pipe);

    let answer = control::send_on(&pipe, &Request::Ping).expect("the second client is served");
    assert!(answer.ok, "{answer:?}");
    assert_eq!(answer.pid, std::process::id());
    assert_eq!(
        seen.load(Ordering::SeqCst),
        1,
        "only the ping reached the handler"
    );

    // A third one, to show the listener is still accepting behind the stall.
    assert!(
        control::send_on(&pipe, &Request::Ping)
            .expect("a third client")
            .ok
    );
    drop(stalled);
}

#[test]
fn a_client_that_hangs_up_mid_command_leaves_the_server_serving() {
    let pipe = private_pipe();
    let seen = serve_counting(&pipe);

    let mut half = raw_client(&pipe);
    half.write_all(br#"{"cmd":"pi"#).expect("half a request");
    half.flush().ok();
    drop(half);

    let answer = control::send_on(&pipe, &Request::Ping).expect("the next client is served");
    assert!(answer.ok, "{answer:?}");
    assert_eq!(
        seen.load(Ordering::SeqCst),
        1,
        "the truncated line never reached the handler"
    );
}

#[test]
fn a_line_that_is_not_a_request_is_answered_with_an_error() {
    let pipe = private_pipe();
    let seen = serve_counting(&pipe);

    let mut client = raw_client(&pipe);
    client.write_all(b"this is not json\n").expect("write");
    let reply: Response = serde_json::from_str(&read_reply(&mut client)).expect("a JSON answer");
    assert!(!reply.ok);
    let error = reply.error.unwrap_or_default();
    assert!(error.contains("cannot parse the request"), "{error}");
    drop(client);

    // A line larger than the pipe buffer: the server reads it all before it
    // answers, so this cannot deadlock against a client that is still writing.
    let mut fat = raw_client(&pipe);
    let mut oversized = vec![b'x'; 128 * 1024];
    oversized.push(b'\n');
    fat.write_all(&oversized).expect("write an oversized line");
    let reply: Response =
        serde_json::from_str(&read_reply(&mut fat)).expect("a JSON answer to the oversized line");
    assert!(!reply.ok);
    drop(fat);

    let answer = control::send_on(&pipe, &Request::Ping).expect("the server is still serving");
    assert!(answer.ok, "{answer:?}");
    assert_eq!(
        seen.load(Ordering::SeqCst),
        1,
        "neither bad line reached the handler"
    );
}

#[test]
fn two_spawn_requests_at_once_both_succeed() {
    if skip_without_desktop() {
        return;
    }

    let pipe = private_pipe();
    // The real thing: the handler spawns windows, exactly as the host does, so
    // two requests that arrive together really do create windows together.
    let batches: Arc<Mutex<Vec<TestWindows>>> = Arc::new(Mutex::new(Vec::new()));
    let served = Arc::clone(&batches);
    control::serve_on(&pipe, move |request| match request {
        Request::Spawn { options } => match TestWindows::spawn_with(&options.into_options()) {
            Ok(batch) => {
                let windows = batch.windows();
                served
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(batch);
                Response::ok().with_windows(windows)
            }
            Err(e) => Response::failed(e.to_string()),
        },
        _ => Response::ok(),
    })
    .expect("serve on a private pipe");

    let request = |prefix: &str| Request::Spawn {
        options: SpawnRequest {
            count: 1,
            monitor: 0,
            title_prefix: prefix.to_string(),
            ..SpawnRequest::default()
        },
    };

    let pipe_for_thread = pipe.clone();
    let first =
        std::thread::spawn(move || control::send_on(&pipe_for_thread, &request("MochiTest")));
    let second = control::send_on(&pipe, &request("MochiTest"));
    let first = first.join().expect("the first client thread");

    let first = first.expect("the first spawn is answered");
    let second = second.expect("the second spawn is answered");
    for answer in [&first, &second] {
        assert!(answer.ok, "{:?}", answer.error);
        assert_eq!(answer.windows.len(), 1, "one window per request");
        assert!(answer.windows[0].visible, "the window is on the screen");
    }
    assert_ne!(
        first.windows[0].hwnd, second.windows[0].hwnd,
        "two requests produced the same window"
    );

    // Everything this test put on the desktop goes away with the batches.
    let handles: Vec<i64> = [&first, &second]
        .iter()
        .map(|answer| answer.windows[0].hwnd)
        .collect();
    batches
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    mochi_testbed::wait_until_gone(&handles, DEADLINE).expect("both windows are closed again");
}
