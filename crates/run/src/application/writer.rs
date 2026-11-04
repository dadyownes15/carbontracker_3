//! The writer: a thread that hands each item to every sink, in order. Its
//! queue is unbounded, so a slow sink delays the others but never loses an
//! item.

use std::sync::mpsc;
use std::thread;

use tokio::sync::oneshot;

use crate::domain::item::Item;
use crate::domain::presentation;
use crate::ports::{Sink, deliver};

enum WriterMessage {
    Item(Item),
    /// The last message: finish every sink, then answer.
    Close {
        done: oneshot::Sender<()>,
    },
}

/// Sends to the writer; sending never waits.
pub(crate) struct WriterHandle {
    sender: mpsc::Sender<WriterMessage>,
}

impl WriterHandle {
    pub fn new(sinks: Vec<Box<dyn Sink>>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let actor = WriterActor {
            receiver,
            sinks,
            warnings: Vec::new(),
        };
        thread::Builder::new()
            .name("carbontracker-writer".to_owned())
            .spawn(move || run_writer(actor))
            .expect("a thread can be spawned");
        Self { sender }
    }

    pub fn item(&self, item: Item) {
        let _ = self.sender.send(WriterMessage::Item(item));
    }

    /// Finishes every sink, then sends on `done`.
    pub fn close(self, done: oneshot::Sender<()>) {
        let _ = self.sender.send(WriterMessage::Close { done });
    }

    /// [Self::close], waiting for the answer.
    pub fn close_and_wait(self) {
        let (done, written) = oneshot::channel();
        self.close(done);
        let _ = written.blocking_recv();
    }
}

struct WriterActor {
    receiver: mpsc::Receiver<WriterMessage>,
    sinks: Vec<Box<dyn Sink>>,
    warnings: Vec<String>,
}

impl WriterActor {
    /// Hands `item` to every sink. A sink that fails is finished, dropped and
    /// named in a warning.
    fn hand(&mut self, item: &Item) {
        let mut index = 0;
        while index < self.sinks.len() {
            match deliver(self.sinks[index].as_mut(), item) {
                Ok(()) => index += 1,
                Err(error) => {
                    let mut sink = self.sinks.remove(index);
                    // It may hold a partial file worth flushing.
                    let _ = sink.finish();
                    self.warnings
                        .push(presentation::sink_failed(sink.name(), &error));
                }
            }
        }
    }

    /// Tells the sinks left which ones failed, then finishes them all.
    fn finish(&mut self) {
        for warning in std::mem::take(&mut self.warnings) {
            self.hand(&Item::Warning(warning));
        }
        for mut sink in self.sinks.drain(..) {
            if let Err(error) = sink.finish() {
                // No sink is left to tell.
                eprintln!(
                    "{}",
                    presentation::prefixed(&presentation::warning(&presentation::sink_failed(
                        sink.name(),
                        &error
                    )))
                );
            }
        }
    }
}

fn run_writer(mut actor: WriterActor) {
    while let Ok(message) = actor.receiver.recv() {
        match message {
            WriterMessage::Item(item) => actor.hand(&item),
            WriterMessage::Close { done } => {
                actor.finish();
                let _ = done.send(());
                return;
            }
        }
    }
    // Every handle dropped without Close.
    actor.finish();
}
