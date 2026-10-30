//! Messages for the Log panel, from any thread. Also written to stderr. An
//! app with a log of its own (text9000's has sources and times) sends the
//! core's lines there with [`set_sink`].

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

static SINK: OnceLock<fn(bool, &str)> = OnceLock::new();

/// Sends every line to `sink` (true marks errors) instead of keeping it here.
pub fn set_sink(sink: fn(bool, &str)) {
    let _ = SINK.set(sink);
}

const KEEP: usize = 500;

static LINES: Mutex<VecDeque<(bool, String)>> = Mutex::new(VecDeque::new());

pub fn info(text: impl Into<String>) {
    push(false, text.into());
}

pub fn error(text: impl Into<String>) {
    push(true, text.into());
}

fn push(error: bool, text: String) {
    if let Some(sink) = SINK.get() {
        return sink(error, &text);
    }
    eprintln!("{}: {text}", crate::app().name);
    let mut lines = LINES.lock().unwrap_or_else(|e| e.into_inner());
    if lines.len() >= KEEP {
        lines.pop_front();
    }
    lines.push_back((error, text));
}

/// Calls `f` with each line, oldest first; true marks errors.
pub fn each(mut f: impl FnMut(bool, &str)) {
    for (e, t) in LINES.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        f(*e, t);
    }
}
