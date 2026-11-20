//! [Projection]: state a frontend derives from a run's items.

use std::time::Instant;

use crate::domain::item::{Block, Item};
use crate::domain::record;

/// State derived from a run's items: updated on the forwarder thread from
/// each item, read by the frontend through a [View](crate::View).
pub trait Projection: Send + 'static {
    /// Updates itself from one item, and returns blocks for the console,
    /// which the writer gets just before the item. No I/O.
    fn project(&mut self, item: &Item) -> Vec<Box<dyn Block>>;

    /// When to be woken with no item; `None` waits for the next item.
    fn wake_at(&self) -> Option<Instant> {
        None
    }

    /// Woken at [Projection::wake_at] with no item; returns blocks for the
    /// console. No I/O.
    fn wake(&mut self) -> Vec<Box<dyn Block>> {
        Vec::new()
    }

    /// Adds to the record's `run` line; called once, before the run starts.
    fn describe(&self, _header: &mut record::Header) {}
}

/// No projection.
impl Projection for () {
    fn project(&mut self, _: &Item) -> Vec<Box<dyn Block>> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC/RUN-20
    #[test]
    fn no_projection_adds_nothing() {
        assert!(().project(&Item::Gap).is_empty());
        assert!(().project(&Item::Warning("a sink failed".to_owned())).is_empty());
    }
}
