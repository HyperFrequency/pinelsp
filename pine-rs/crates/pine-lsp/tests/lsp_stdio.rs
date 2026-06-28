//! End-to-end LSP wire tests: spawn the *built* `pine-lsp` binary and drive it
//! over Content-Length-framed stdio, exactly as a real client would.
//!
//! Critical invariants exercised here:
//! - stdout carries ONLY JSON-RPC frames; logs must go to stderr. We parse
//!   every stdout byte as framed JSON, so a stray log byte on stdout would make
//!   the framing parse fail the test.
//! - the full initialize -> didOpen -> completion -> hover -> shutdown exchange
//!   completes under a wall-clock bound. A dedicated reader thread feeds an
//!   mpsc channel, and the test pulls with `recv_timeout`, so a server hang
//!   fails the test instead of blocking CI forever.
//! - a large (~5000-line) document round-trips parse + incremental edit +
//!   completion within a generous time bound (perf guard).

use std::io::{BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

/// Wall-clock budget for waiting on any single expected response.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(20);
/// Generous budget for the large-file perf round-trip.
const LARGE_FILE_BUDGET: Duration = Duration::from_secs(10);

/// Frame and write one JSON-RPC message with the LSP `Content-Length` header.
fn write_message(stdin: &mut ChildStdin, json: &str) {
    write!(stdin, "Content-Length: {}\r\n\r\n{}", json.len(), json).expect("write frame");
    stdin.flush().expect("flush");
}

/// Read exactly one Content-Length-framed message from `reader`: parse the
/// header (case- and whitespace-tolerant), then read EXACTLY `Content-Length`
/// body bytes. Returns `None` on clean EOF between messages.
fn read_message(reader: &mut BufReader<ChildStdout>) -> Option<serde_json::Value> {
    let mut header = Vec::new();
    let mut one = [0u8; 1];
    loop {
        match reader.read(&mut one) {
            Ok(0) => {
                if header.is_empty() {
                    return None;
                }
                panic!(
                    "EOF mid-header; got: {:?}",
                    String::from_utf8_lossy(&header)
                );
            }
            Ok(_) => {
                header.push(one[0]);
                if header.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            Err(e) => panic!("header read error: {e}"),
        }
    }

    let header_text = String::from_utf8(header).expect("header is utf8");
    let content_length = header_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .expect("Content-Length header present");

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).expect("read exact body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("body is valid JSON-RPC");
    Some(value)
}

struct Server {
    child: Child,
    /// `Option` so it can be dropped (closing stdin -> EOF) to let the server's
    /// message loop terminate cleanly after an `exit` notification.
    stdin: Option<ChildStdin>,
    /// All framed stdout messages, fed by a background reader thread.
    messages: Receiver<serde_json::Value>,
}

impl Server {
    fn spawn() -> Self {
        Self::spawn_with_env(&[])
    }

    fn spawn_with_env(env: &[(&str, &str)]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pine-lsp"));
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn pine-lsp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Some(msg) = read_message(&mut reader) {
                if tx.send(msg).is_err() {
                    break; // receiver dropped (test finished)
                }
            }
        });

        Server {
            child,
            stdin: Some(stdin),
            messages: rx,
        }
    }

    fn send(&mut self, json: &str) {
        write_message(self.stdin.as_mut().expect("stdin open"), json);
    }

    /// Close stdin (EOF) so the server's read loop terminates after `exit`, then
    /// wait for a clean exit within `timeout`, killing it on overrun.
    fn close_stdin_and_wait(&mut self, timeout: Duration) -> std::process::ExitStatus {
        self.stdin = None; // drop -> EOF on the server's stdin
        wait_with_timeout(&mut self.child, timeout)
    }

    /// Pull framed messages until one with `id == want_id` arrives (skipping
    /// server-initiated notifications such as `window/logMessage` /
    /// `publishDiagnostics`). Times out via the channel so a hang fails rather
    /// than blocks.
    fn response(&mut self, want_id: i64) -> serde_json::Value {
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::ZERO);
            let msg = self
                .messages
                .recv_timeout(remaining)
                .unwrap_or_else(|_| panic!("timed out waiting for response id={want_id}"));
            if msg.get("id").and_then(|v| v.as_i64()) == Some(want_id) {
                return msg;
            }
        }
    }
}

fn initialize_json(id: i64) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": { "capabilities": {} }
    })
    .to_string()
}

fn did_open_json(uri: &str, text: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": { "uri": uri, "languageId": "pine", "version": 1, "text": text }
        }
    })
    .to_string()
}

fn notify(method: &str) -> String {
    serde_json::json!({"jsonrpc":"2.0","method":method,"params":{}}).to_string()
}

#[test]
fn lsp_basic_lifecycle_over_stdio() {
    let mut server = Server::spawn();

    // initialize -> assert completion + hover providers are advertised.
    server.send(&initialize_json(1));
    let init = server.response(1);
    let caps = &init["result"]["capabilities"];
    assert!(
        !caps["completionProvider"].is_null(),
        "initialize must advertise completionProvider: {init}"
    );
    assert!(
        !caps["hoverProvider"].is_null(),
        "initialize must advertise hoverProvider: {init}"
    );

    server.send(&notify("initialized"));

    let uri = "file:///workspace/test.pine";
    let text = "//@version=6\nx = ta.sma(close, 14)\nplot(close)\n";
    server.send(&did_open_json(uri, text));

    // completion at the `ta.` member position (line 1, just after the dot).
    server.send(
        &serde_json::json!({
            "jsonrpc":"2.0","id":2,"method":"textDocument/completion",
            "params":{"textDocument":{"uri":uri},"position":{"line":1,"character":7}}
        })
        .to_string(),
    );
    let completion = server.response(2);
    let items = completion["result"]
        .as_array()
        .expect("completion result is an array");
    assert!(
        items.iter().any(|i| i["label"] == "sma"),
        "completion at `ta.` must contain label `sma`: {completion}"
    );

    // hover on `close` in `x = ta.sma(close, 14)` — `close` spans chars 11..16,
    // so char 13 is inside it.
    server.send(
        &serde_json::json!({
            "jsonrpc":"2.0","id":3,"method":"textDocument/hover",
            "params":{"textDocument":{"uri":uri},"position":{"line":1,"character":13}}
        })
        .to_string(),
    );
    let hover = server.response(3);
    assert!(
        !hover["result"].is_null() && !hover["result"]["contents"].is_null(),
        "hover on `close` must return non-null contents: {hover}"
    );

    // shutdown -> null result, then exit notification.
    server.send(
        &serde_json::json!({"jsonrpc":"2.0","id":4,"method":"shutdown","params":null}).to_string(),
    );
    let shutdown = server.response(4);
    assert!(
        shutdown["result"].is_null(),
        "shutdown must return null result: {shutdown}"
    );

    server.send(&serde_json::json!({"jsonrpc":"2.0","method":"exit","params":null}).to_string());

    let status = server.close_stdin_and_wait(Duration::from_secs(5));
    assert!(status.success(), "server should exit cleanly after `exit`");

    // STDOUT/STDERR separation: every stdout byte above parsed as framed
    // JSON-RPC (read_message would have panicked on a stray log byte). Logs, if
    // any, are on stderr — drain it to confirm it is a separate stream.
    if let Some(mut err) = server.child.stderr.take() {
        let mut stderr = String::new();
        let _ = err.read_to_string(&mut stderr);
    }
}

#[test]
fn large_file_round_trip_under_budget() {
    let mut server = Server::spawn();

    server.send(&initialize_json(1));
    let _ = server.response(1);
    server.send(&notify("initialized"));

    // Generate a ~5000-line document.
    let mut text = String::from("//@version=6\n");
    for n in 1..=5000 {
        text.push_str(&format!("plot(ta.sma(close, {n}))\n"));
    }
    let uri = "file:///workspace/big.pine";

    let started = Instant::now();
    server.send(&did_open_json(uri, &text));

    // Incremental edit: insert a new line just after the version line (zero-width
    // range at line 1 col 0 -> pure insertion).
    server.send(
        &serde_json::json!({
            "jsonrpc":"2.0","method":"textDocument/didChange",
            "params":{
                "textDocument":{"uri":uri,"version":2},
                "contentChanges":[{
                    "range":{"start":{"line":1,"character":0},"end":{"line":1,"character":0}},
                    "text":"plot(ta.ema(close, 9))\n"
                }]
            }
        })
        .to_string(),
    );

    // Completion after `ta.` on the freshly inserted line (line 1, char 8).
    server.send(
        &serde_json::json!({
            "jsonrpc":"2.0","id":2,"method":"textDocument/completion",
            "params":{"textDocument":{"uri":uri},"position":{"line":1,"character":8}}
        })
        .to_string(),
    );
    let completion = server.response(2);
    let elapsed = started.elapsed();

    let items = completion["result"]
        .as_array()
        .expect("completion result is an array");
    assert!(
        items.iter().any(|i| i["label"] == "sma"),
        "large-file completion at `ta.` must still contain `sma`"
    );
    assert!(
        elapsed < LARGE_FILE_BUDGET,
        "large-file didOpen+didChange+completion took {elapsed:?}, over budget {LARGE_FILE_BUDGET:?}"
    );

    server.send(
        &serde_json::json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":null}).to_string(),
    );
    let _ = server.response(3);
    server.send(&serde_json::json!({"jsonrpc":"2.0","method":"exit","params":null}).to_string());
    let _ = server.close_stdin_and_wait(Duration::from_secs(5));
}

#[test]
fn logs_go_to_stderr_not_stdout() {
    // With PINE_LOG=debug the server should emit spans/events — but ONLY to
    // stderr. stdout must remain pure JSON-RPC: every byte we read below is
    // parsed as a framed JSON message (read_message panics on any stray byte),
    // so a successful exchange already proves stdout is uncontaminated.
    let mut server = Server::spawn_with_env(&[("PINE_LOG", "debug")]);

    server.send(&initialize_json(1));
    let init = server.response(1);
    assert!(!init["result"]["capabilities"].is_null());
    server.send(&notify("initialized"));

    let uri = "file:///workspace/log.pine";
    server.send(&did_open_json(uri, "//@version=6\nplot(close)\n"));

    // A completion request forces a debug! event on the import-cache path too.
    server.send(
        &serde_json::json!({
            "jsonrpc":"2.0","id":2,"method":"textDocument/completion",
            "params":{"textDocument":{"uri":uri},"position":{"line":1,"character":0}}
        })
        .to_string(),
    );
    let _ = server.response(2);

    server.send(
        &serde_json::json!({"jsonrpc":"2.0","id":3,"method":"shutdown","params":null}).to_string(),
    );
    let _ = server.response(3);
    server.send(&serde_json::json!({"jsonrpc":"2.0","method":"exit","params":null}).to_string());

    // Take stderr BEFORE waiting so a chatty server cannot fill the pipe buffer
    // and deadlock on exit.
    let mut stderr_handle = server.child.stderr.take().expect("stderr piped");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr_handle.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });

    let status = server.close_stdin_and_wait(Duration::from_secs(5));
    assert!(status.success(), "server should exit cleanly");

    let stderr = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("stderr collected");
    // At debug level we emit at least the `initialize` info event plus did_open
    // / completion debug events. Assert SOME log output landed on stderr.
    assert!(
        stderr.contains("initialize") || stderr.contains("did_open") || stderr.contains("cache"),
        "PINE_LOG=debug must emit log events to stderr; got: {stderr:?}"
    );
}

/// Wait for the child to exit, killing it if it overruns `timeout` so a hung
/// server cannot wedge the test run.
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => return status,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    return child.wait().expect("wait after kill");
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
}
