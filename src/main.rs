//! mechfight/1 bot entry point: JSON lines on stdin, JSON lines on stdout.
//!
//! A reader thread hands over every batch of complete lines it gets from one read; the main loop
//! folds all of them into memory and answers only the newest observation (protocol.md section 2).

use mechbot::brain::Brain;
use mechbot::map::Map;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;

const NAME: &str = "BingBU";

fn send(line: &Value) {
    let out = std::io::stdout();
    let mut h = out.lock();
    if writeln!(h, "{}", line).is_err() || h.flush().is_err() {
        std::process::exit(0);
    }
}

fn main() {
    let (tx, rx) = mpsc::channel::<Vec<String>>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut h = stdin.lock();
        let mut buf: Vec<u8> = Vec::with_capacity(1 << 17);
        let mut chunk = vec![0u8; 1 << 16];
        loop {
            match h.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    let mut lines = Vec::new();
                    let mut start = 0;
                    for i in 0..buf.len() {
                        if buf[i] == b'\n' {
                            if i > start {
                                lines.push(String::from_utf8_lossy(&buf[start..i]).into_owned());
                            }
                            start = i + 1;
                        }
                    }
                    buf.drain(..start);
                    if !lines.is_empty() && tx.send(lines).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });

    let mut brain: Option<Brain> = None;
    loop {
        let mut batch = match rx.recv() {
            Ok(b) => b,
            Err(_) => break, // stdin closed
        };
        while let Ok(more) = rx.try_recv() {
            batch.extend(more);
        }
        let msgs: Vec<Value> = batch.iter().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect();
        let newest = msgs.iter().rposition(|v| v["type"].as_str() == Some("obs"));
        for (i, v) in msgs.iter().enumerate() {
            match v["type"].as_str() {
                Some("hello") => {
                    let b = catch_unwind(AssertUnwindSafe(|| Brain::new(v))).unwrap_or_else(|_| Brain::with_map(Map::flat(), v));
                    brain = Some(b);
                    send(&json!({"type": "ready", "name": NAME}));
                }
                Some("obs") => {
                    let b = match brain.as_mut() {
                        Some(b) => b,
                        None => {
                            if Some(i) == newest {
                                send(&json!({"tick": v["tick"].clone()}));
                            }
                            continue;
                        }
                    };
                    if Some(i) == newest {
                        let out = catch_unwind(AssertUnwindSafe(|| b.decide(v))).unwrap_or_else(|_| json!({"tick": v["tick"].clone()}));
                        send(&out);
                    } else {
                        let _ = catch_unwind(AssertUnwindSafe(|| b.ingest(v)));
                    }
                }
                Some("end") => {
                    let _ = std::io::stdout().flush();
                    std::process::exit(0);
                }
                _ => {}
            }
        }
    }
}
