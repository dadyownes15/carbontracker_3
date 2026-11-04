//! What a span reports it did. The tracker announces it and keeps none of it.

/// One kind of work and how much of it: tokens, batches, steps.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct WorkEntry {
    pub kind: String,
    pub amount: f64,
}

/// The work reported on a span, in the order it was reported. Serializes as a
/// list of its entries.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Work(Vec<WorkEntry>);

impl Work {
    /// Work of a single kind.
    pub fn of(kind: impl Into<String>, amount: f64) -> Self {
        Self(vec![WorkEntry {
            kind: kind.into(),
            amount,
        }])
    }

    pub fn entries(&self) -> &[WorkEntry] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromIterator<(String, f64)> for Work {
    fn from_iter<I: IntoIterator<Item = (String, f64)>>(pairs: I) -> Self {
        Self(
            pairs
                .into_iter()
                .map(|(kind, amount)| WorkEntry { kind, amount })
                .collect(),
        )
    }
}
