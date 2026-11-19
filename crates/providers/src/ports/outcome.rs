//! [Outcome]: what one successful probe yields, the contract between the
//! chains and every candidate. A probe that cannot serve gives its reason
//! instead.

use carbontracker::{Provider, SourceDescription, SourceId};

/// What a probe yields: an [Outcome], or the reason it declined, shown to
/// the user.
pub(crate) type Probed<T = Outcome> = Result<T, String>;

/// What one successful probe yields: the sources it can serve, and the
/// provider that reads them.
pub(crate) struct Outcome {
    pub(crate) descriptions: Vec<(SourceId, SourceDescription)>,
    pub(crate) provider: Box<dyn Provider>,
}

impl Outcome {
    pub(crate) fn single(source: SourceId, description: SourceDescription, provider: impl Provider) -> Self {
        Self { descriptions: vec![(source, description)], provider: Box::new(provider) }
    }
}
