//! Typed failures raised while reading or validating configuration.

use std::fmt;

/// One reason a loaded configuration was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Operator-readable description of what was wrong and with which key.
    pub message: String,
}

impl Violation {
    /// Builds a violation from any displayable description.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "- {}", self.message)
    }
}

/// Every reason a loaded configuration was rejected, collected together.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Violations(Vec<Violation>);

impl Violations {
    /// Records one more reason.
    pub fn push(&mut self, message: impl Into<String>) {
        self.0.push(Violation::new(message));
    }

    /// Number of recorded reasons.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether anything was rejected at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterates the recorded reasons.
    pub fn iter(&self) -> std::slice::Iter<'_, Violation> {
        self.0.iter()
    }
}

impl fmt::Display for Violations {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for violation in &self.0 {
            writeln!(f, "{violation}")?;
        }
        Ok(())
    }
}

/// Why configuration loading failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The environment could not be shaped into the declared configuration tree.
    #[error("the configuration could not be read: {0}")]
    Source(#[from] figment::Error),
    /// The configuration parsed but violated its own rules; every violation is reported together.
    #[error("the configuration was rejected:\n{}", violations)]
    Invalid {
        /// All collected violations.
        violations: Violations,
    },
}

impl ConfigError {
    /// Value-free operator text describing the rejection.
    #[must_use]
    pub fn report(&self) -> String {
        match self {
            Self::Source(source) => format!("configuration could not be read: {source}"),
            Self::Invalid { violations } => format!("configuration rejected:\n{violations}"),
        }
    }

    /// The process exit status this class of failure maps to: `EX_CONFIG` (78) for every
    /// configuration rejection.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Source(_) | Self::Invalid { .. } => 78,
        }
    }
}
