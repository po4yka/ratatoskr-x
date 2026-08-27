//! Explicit bookmark write-back admission and provider execution.

mod provider;
mod service;
mod types;

pub use provider::OfficialBookmarkProvider;
pub use service::BookmarkWritebackService;
pub use types::{
    BookmarkAction, BookmarkAdmissionRefusal, BookmarkDryRunOutcome, BookmarkDryRunResult,
    BookmarkMutationProvider, BookmarkProviderError, BookmarkProviderEvidence,
    BookmarkProviderSuccess, BookmarkWriteConsent, BookmarkWriteRequest, BookmarkWriteResult,
    BookmarkWriteStatus, BookmarkWritebackError, ConsentSurfaceId,
};
