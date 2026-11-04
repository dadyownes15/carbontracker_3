//! The contract between the chains and the candidates: a probe yields an
//! [Outcome], sources and the engine's `Provider` service that reads them.

mod outcome;

pub(crate) use outcome::Outcome;
