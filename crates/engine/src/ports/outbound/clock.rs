use crate::domain::Timestamp;

/// Tells the time.
///
/// The handle stamps every start, stop and mark with it, so a cut records when
/// the caller acted rather than when the tracker got round to it.
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}
