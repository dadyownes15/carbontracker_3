//! [Sink]: the port every output implements, and [deliver], which hands it
//! each item.

use std::io;

use carbontracker::TrackerEvent;
use carbontracker_providers::ProbeProgress;

use crate::domain::item::{Block, Item, Outcome, Started};

/// Anything a run writes to. Called on the writer thread, never the engine's
/// or the caller's, so a sink may block on I/O. A sink that errs is finished
/// and dropped; the others go on.
pub trait Sink: Send {
    fn probing(&mut self, _step: &ProbeProgress) -> io::Result<()> {
        Ok(())
    }

    /// Before the first event: the run's header and catalog.
    fn start(&mut self, started: &Started) -> io::Result<()>;

    /// Every engine event, in order.
    fn event(&mut self, event: &TrackerEvent) -> io::Result<()>;

    /// Events were lost here.
    fn gap(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn outcome(&mut self, _outcome: &Outcome) -> io::Result<()> {
        Ok(())
    }

    fn warning(&mut self, _text: &str) -> io::Result<()> {
        Ok(())
    }

    fn error(&mut self, _text: &str) -> io::Result<()> {
        Ok(())
    }

    fn block(&mut self, _block: &dyn Block) -> io::Result<()> {
        Ok(())
    }

    /// Nothing more is coming: flush and close.
    fn finish(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// For warnings: which sink failed.
    fn name(&self) -> &str;
}

/// Hands `item` to `sink`'s method for its kind.
pub fn deliver(sink: &mut (impl Sink + ?Sized), item: &Item) -> io::Result<()> {
    match item {
        Item::Probing(step) => sink.probing(step),
        Item::Started(started) => sink.start(started),
        Item::Event(event) => sink.event(event),
        Item::Gap => sink.gap(),
        Item::Outcome(outcome) => sink.outcome(outcome),
        Item::Warning(text) => sink.warning(text),
        Item::Error(text) => sink.error(text),
        Item::Block(block) => sink.block(block.as_ref()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::domain::item::Layout;

    /// Which method each item reached.
    #[derive(Default)]
    struct Calls(Vec<&'static str>);

    impl Sink for Calls {
        fn probing(&mut self, _: &ProbeProgress) -> io::Result<()> {
            self.0.push("probing");
            Ok(())
        }
        fn start(&mut self, _: &Started) -> io::Result<()> {
            self.0.push("start");
            Ok(())
        }
        fn event(&mut self, _: &TrackerEvent) -> io::Result<()> {
            self.0.push("event");
            Ok(())
        }
        fn gap(&mut self) -> io::Result<()> {
            self.0.push("gap");
            Ok(())
        }
        fn outcome(&mut self, _: &Outcome) -> io::Result<()> {
            self.0.push("outcome");
            Ok(())
        }
        fn warning(&mut self, _: &str) -> io::Result<()> {
            self.0.push("warning");
            Ok(())
        }
        fn error(&mut self, _: &str) -> io::Result<()> {
            self.0.push("error");
            Ok(())
        }
        fn block(&mut self, _: &dyn Block) -> io::Result<()> {
            self.0.push("block");
            Ok(())
        }
        fn name(&self) -> &str {
            "calls"
        }
    }

    #[derive(Debug)]
    struct Line;

    impl Block for Line {
        fn lines(&self, _: &Layout) -> Vec<String> {
            vec!["a line".to_owned()]
        }
    }

    /// AC/RUN-3
    #[test]
    fn each_item_reaches_the_method_for_its_kind() {
        let mut sink = Calls::default();
        let items = [
            Item::Probing(ProbeProgress::Probing { candidate: "rapl" }),
            Item::Gap,
            Item::Outcome(Outcome::Completed { epochs_done: None }),
            Item::Warning("a warning".to_owned()),
            Item::Error("an error".to_owned()),
            Item::Block(Arc::new(Line)),
        ];
        for item in &items {
            deliver(&mut sink, item).unwrap();
        }
        assert_eq!(sink.0, ["probing", "gap", "outcome", "warning", "error", "block"]);
    }
}
