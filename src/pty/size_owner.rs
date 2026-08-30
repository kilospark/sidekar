//! Arbitration for the child PTY's window size between the local terminal and
//! remote viewers.
//!
//! A PTY has one size but can have two people looking at it. Paseo resolves
//! this with explicit claim/update intent: a `claim` takes ownership and
//! applies, an `update` only applies if the sender already owns the size. That
//! lets a viewer track its own window without fighting the other viewer for
//! control on every reflow.

/// Who last claimed the right to set the size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SizeOwner {
    /// The terminal sidekar was launched from.
    Local,
    /// A viewer attached over the relay tunnel.
    Remote,
}

/// Whether a resize takes ownership or merely refreshes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SizeIntent {
    Claim,
    Update,
}

impl SizeIntent {
    pub(crate) fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("update") => Self::Update,
            // Absent intent means claim, so a viewer that does not speak the
            // extension still gets its resize honored.
            _ => Self::Claim,
        }
    }
}

/// Tracks the current owner. The local terminal owns the size until a viewer
/// claims it, because that is where the session started.
#[derive(Debug)]
pub(crate) struct SizeOwnership {
    owner: SizeOwner,
}

impl SizeOwnership {
    pub(crate) fn new() -> Self {
        Self {
            owner: SizeOwner::Local,
        }
    }

    pub(crate) fn owner(&self) -> SizeOwner {
        self.owner
    }

    /// Returns true when the caller may apply the resize.
    pub(crate) fn apply(&mut self, owner: SizeOwner, intent: SizeIntent) -> bool {
        match intent {
            SizeIntent::Claim => {
                self.owner = owner;
                true
            }
            SizeIntent::Update => self.owner == owner,
        }
    }
}

impl Default for SizeOwnership {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
