//! Turns Tauri's fire-and-forget `eval` into a request/response call: each call carries an id the page quotes back over IPC, so a control route reports what the page actually did rather than answering 200 for having queued a script.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::Duration;

/// What the page reported back about one dispatched call.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Bridge {
    seq: AtomicU64,
    waiting: Mutex<HashMap<u64, mpsc::Sender<Outcome>>>,
}

/// A dispatched call that has not been answered yet.
pub struct Pending<'a> {
    bridge: &'a Bridge,
    id: u64,
    replies: Receiver<Outcome>,
    settled: bool,
}

#[derive(Debug)]
pub enum WaitError {
    Rejected(String),
    Timeout(Duration),
    Disconnected,
}

impl Bridge {
    /// Reserves an id and a slot for its answer; the caller embeds [`Pending::id`] in the script it evaluates.
    pub fn dispatch(&self) -> Pending<'_> {
        let id = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let (sender, replies) = mpsc::channel();
        self.waiting
            .lock()
            .expect("bridge mutex poisoned")
            .insert(id, sender);
        Pending { bridge: self, id, replies, settled: false }
    }

    /// Called from the page's IPC command; an unknown id is an answer whose caller gave up, so it is dropped.
    pub fn settle(&self, id: u64, outcome: Outcome) {
        if let Some(sender) = self
            .waiting
            .lock()
            .expect("bridge mutex poisoned")
            .remove(&id)
        {
            let _ = sender.send(outcome);
        }
    }

    fn forget(&self, id: u64) {
        self.waiting
            .lock()
            .expect("bridge mutex poisoned")
            .remove(&id);
    }
}

impl Pending<'_> {
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Waits for the page's answer while preserving why it failed.
    pub fn wait(mut self, timeout: Duration) -> Result<(), WaitError> {
        let result = match self.replies.recv_timeout(timeout) {
            Ok(outcome) if outcome.ok => Ok(()),
            Ok(outcome) => Err(WaitError::Rejected(
                outcome
                    .error
                    .unwrap_or_else(|| "the page rejected the request".to_string()),
            )),
            Err(RecvTimeoutError::Timeout) => Err(WaitError::Timeout(timeout)),
            Err(RecvTimeoutError::Disconnected) => Err(WaitError::Disconnected),
        };
        self.settled = true;
        self.bridge.forget(self.id);
        result
    }
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        // The script may never have been evaluated, in which case nothing will ever answer this id.
        if !self.settled {
            self.bridge.forget(self.id);
        }
    }
}
