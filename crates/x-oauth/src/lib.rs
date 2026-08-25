//! OAuth 2.0 Authorization Code with PKCE for the ratatoskr-x bounded context:
//! authorization intents, the callback state matrix, encrypted credential
//! envelopes, rotation-aware refresh, revocation, and scope auditing.

pub mod callback;
pub mod cipher;
pub mod clock;
pub mod error;
pub mod intent;
pub mod payload;
pub mod pkce;
pub mod scope;
pub mod service;
pub mod token_client;
