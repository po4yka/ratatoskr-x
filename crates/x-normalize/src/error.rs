//! Typed failures produced while normalizing provider payloads.

use std::fmt;

/// Everything that can stop an envelope from normalizing.
///
/// Variants carry the offending object and member so callers can decide
/// retry, tombstone, or quarantine policy above this layer.
#[derive(Debug)]
#[non_exhaustive]
pub enum NormalizeError {
    /// A documented required member is missing from an object.
    MissingMember {
        /// Which payload object lacks the member (for example `post`).
        object: &'static str,
        /// The absent member name.
        member: &'static str,
    },
    /// A member is present but does not parse into its normalized type.
    InvalidMember {
        /// Which payload object carries the bad member.
        object: &'static str,
        /// The unparseable member name.
        member: &'static str,
    },
    /// A post's author id has no matching user object in the same envelope.
    UnresolvedAuthor {
        /// Provider id of the post whose author could not be resolved.
        post_provider_id: String,
    },
}

impl fmt::Display for NormalizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NormalizeError::MissingMember { object, member } => {
                write!(
                    f,
                    "payload object `{object}` is missing required member `{member}`"
                )
            }
            NormalizeError::InvalidMember { object, member } => {
                write!(
                    f,
                    "payload object `{object}` has member `{member}` that does not parse"
                )
            }
            NormalizeError::UnresolvedAuthor { post_provider_id } => {
                write!(
                    f,
                    "post `{post_provider_id}` has no matching author user object"
                )
            }
        }
    }
}

impl std::error::Error for NormalizeError {}
