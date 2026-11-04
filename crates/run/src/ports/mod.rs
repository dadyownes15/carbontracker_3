//! The traits the run drives: where it writes ([Sink]) and what it derives
//! ([Projection]).

mod projection;
mod sink;

pub use projection::Projection;
pub use sink::{Sink, deliver};
