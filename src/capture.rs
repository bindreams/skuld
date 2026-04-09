//! Per-test tracing event capture.
//!
//! Each test gets a dedicated [`tracing_subscriber::fmt`] subscriber whose
//! writer is an in-memory `Vec<u8>` buffer. The subscriber is installed via
//! [`tracing::subscriber::set_default`], which is strictly thread-local —
//! so two concurrent tests on two libtest-mimic worker threads cannot see
//! each other's events. On test pass, the buffer is discarded. On panic,
//! the runner dumps the buffer to stderr before propagating the panic.
//!
//! # Known limitation
//!
//! Events from tasks spawned on worker threads *other than* the test
//! thread (e.g. `tokio::spawn` onto a multi-thread runtime) are NOT
//! captured: those threads have no `set_default` subscriber installed.
//! If you need cross-thread capture, attach `.with_current_subscriber()`
//! at the spawn site.
//!
//! # `tracing-log` is deliberately off
//!
//! This module uses `tracing-subscriber` without the `tracing-log`
//! feature. Enabling it auto-installs a `LogTracer` that mutates
//! `log::max_level` globally, which has caused Windows CI timeout
//! regressions in downstream projects (bindreams/hole#147). Only
//! `tracing::` events are captured — `log::` crate events are dropped.

use std::io;
use std::sync::{Arc, Mutex, MutexGuard};

use tracing_subscriber::fmt::MakeWriter;

/// Per-test capture buffer. Cloneable so the runner can retain a handle
/// after moving the writer into the subscriber.
#[derive(Clone, Default)]
pub(crate) struct CaptureBuffer(Arc<Mutex<Vec<u8>>>);

impl CaptureBuffer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Snapshot the current buffer contents. Callers should use this only
    /// after the associated subscriber has been dropped, to avoid racing
    /// with in-flight `make_writer` locks.
    pub(crate) fn snapshot(&self) -> Vec<u8> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Build the [`MakeWriter`] adapter for this buffer. Consumes a clone
    /// of the inner `Arc` so the returned writer is independent of the
    /// buffer handle the runner retains.
    pub(crate) fn make_writer(&self) -> CaptureWriter {
        CaptureWriter(Arc::clone(&self.0))
    }
}

pub(crate) struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

/// RAII guard returned by [`CaptureWriter::make_writer`]. Holds the buffer
/// lock for the duration of one subscriber write call; drops it when the
/// fmt layer is done formatting a single event.
pub(crate) struct CaptureWriterGuard<'a>(MutexGuard<'a, Vec<u8>>);

impl io::Write for CaptureWriterGuard<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl<'a> MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriterGuard<'a>;
    fn make_writer(&'a self) -> Self::Writer {
        // Blocking lock; the subscriber serializes writes on a single
        // subscriber anyway. Poisoned locks are handled by unwrapping
        // into the inner — the buffer's consistency doesn't matter if
        // a test has already panicked.
        CaptureWriterGuard(self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}
