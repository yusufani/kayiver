//! Shared clipboard between machines. Each side watches its own clipboard and
//! sends changes to the peer; text arriving from the peer is applied and
//! remembered so the watcher doesn't echo it straight back.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::platform;

/// Clipboard owners (especially Windows applications) are allowed to hold the
/// clipboard briefly while finishing a copy operation.  Do not treat that as a
/// missing value: retry before deciding that this change contains no text.
const READ_RETRIES: u32 = 6;
const READ_RETRY_DELAY: Duration = Duration::from_millis(75);

/// The last clipboard text we synced (sent or received). The watcher compares
/// against it so a value that came from the peer isn't broadcast back.
pub type ClipState = Arc<Mutex<String>>;

pub fn new_state() -> ClipState {
    Arc::new(Mutex::new(String::new()))
}

/// Poll the local clipboard; when the user changes it, hand the new text to
/// `send`. Runs forever on its own thread. Seeds with the current clipboard so
/// nothing is broadcast just because kayiver started.
pub fn watch(state: ClipState, send: impl Fn(String) + Send + 'static) {
    std::thread::Builder::new()
        .name("kayiver-clip".into())
        .spawn(move || {
            let mut file_path = platform::get_clipboard_file();
            if let Some(cur) = platform::get_clipboard() {
                if let Some(payload) = crate::engine::quickshare::detect_payload(Some(&cur), file_path.as_deref()) {
                    crate::engine::quickshare::engine().update_candidate(payload);
                }
                *state.lock().unwrap() = cur;
            } else if let Some(fp) = &file_path {
                if let Some(payload) = crate::engine::quickshare::detect_payload(None, Some(fp)) {
                    crate::engine::quickshare::engine().update_candidate(payload);
                }
            }
            // Poll a cheap change counter; only read the whole clipboard when it
            // actually changed, so nothing heavy runs on the idle path.
            let mut last_seq = platform::clipboard_seq();
            loop {
                std::thread::sleep(Duration::from_millis(400));
                let seq = platform::clipboard_seq();
                if seq == last_seq {
                    continue;
                }
                // A clipboard can be momentarily unavailable just after its
                // sequence number changes.  Advancing `last_seq` before a
                // successful read used to permanently drop that copy.
                let mut cur = None;
                for attempt in 0..READ_RETRIES {
                    if let Some(text) = platform::get_clipboard() {
                        cur = Some(text);
                        break;
                    }
                    if attempt + 1 < READ_RETRIES {
                        std::thread::sleep(READ_RETRY_DELAY);
                    }
                }
                file_path = platform::get_clipboard_file();
                last_seq = seq;

                if let Some(payload) = crate::engine::quickshare::detect_payload(cur.as_deref(), file_path.as_deref()) {
                    crate::engine::quickshare::engine().update_candidate(payload);
                }

                let Some(cur) = cur else { continue };
                if cur.is_empty() {
                    continue;
                }
                let mut last = state.lock().unwrap();
                if *last != cur {
                    *last = cur.clone();
                    drop(last);
                    send(cur);
                }
            }
        })
        .ok();
}

/// Apply text received from the peer, remembering it so the watcher won't send
/// it back. No-op if it already matches what we last synced.
pub fn apply_remote(state: &ClipState, text: &str) {
    {
        let mut last = state.lock().unwrap();
        if *last == text {
            return;
        }
        *last = text.to_string();
    }
    platform::set_clipboard(text);
}
