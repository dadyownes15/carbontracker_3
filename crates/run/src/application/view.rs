//! [View]: a run's projection, shared by the forwarder, which updates it,
//! and the frontend, which reads it.

use std::sync::{Arc, Mutex, MutexGuard};

/// A projection, shared by the forwarder, which updates it, and the
/// frontend, which reads it.
pub struct View<P>(Arc<Mutex<P>>);

impl<P> Clone for View<P> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<P> View<P> {
    pub(crate) fn new(projection: P) -> Self {
        Self(Arc::new(Mutex::new(projection)))
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, P> {
        self.0
            .lock()
            .expect("a projection never panics while locked")
    }

    /// Reads the projection under its lock. Keep `read` short, and never
    /// call into Python or do I/O in it: the forwarder may be waiting.
    pub fn read<R>(&self, read: impl FnOnce(&P) -> R) -> R {
        read(&self.lock())
    }
}
