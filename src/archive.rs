//! Verbatim zstd archive writer (Task 3). Stub for the scaffold.

/// Which write path produced a generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A full-tree sweep captured a new-or-changed transcript.
    Sweep,
    /// The PreCompact hook snapshotted a transcript before compaction.
    Precompact,
}

impl Kind {
    /// Filename tag for this kind.
    pub fn tag(self) -> &'static str {
        match self {
            Kind::Sweep => "sweep",
            Kind::Precompact => "precompact",
        }
    }
}
