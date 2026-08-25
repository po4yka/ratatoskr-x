## Purpose

Defines how an internal user connects an X account through the official OAuth 2.0 Authorization Code flow with PKCE: one-time authorization intents, the callback validation matrix, code exchange, minimized scopes with auditing, encrypted credential storage at rest, rotation-aware refresh with reuse detection, and revocation that scrubs secret material.

## ADDED Requirements

### Requirement: Authorization intent issuance

Requesting a connection SHALL generate a fresh `state`, a nonce, and an RFC 7636 PKCE verifier whose S256 challenge is the base64url-encoded SHA-256 digest of the verifier, SHALL persist exactly one unconsumed intent record bound to the requesting internal user with a creation time and an expiry, and SHALL return an authorization URL carrying the client identifier, redirect URI, `response_type=code`, the challenge with method `S256`, the state, and exactly the configured read scope set. The verifier SHALL NOT appear in the authorization URL.

#### Scenario: Authorization URL carries minimized scopes and S256 challenge

- **WHEN** an internal user requests a read-only connection and the authorization URL is built from the issued intent
- **THEN** the URL query carries `response_type=code`, the challenge matching the SHA-256/base64url transformation of the stored verifier, the state value, and a scope list equal to the configured read set joined in request order

#### Scenario: Verifier stays out of the authorization URL

- **WHEN** the authorization URL for a freshly issued intent is inspected
- **THEN** neither the verifier nor any transformation of it other than the published challenge appears anywhere in the URL

#### Scenario: Challenge matches the RFC 7636 derivation vector

- **WHEN** the challenge is derived for the RFC 7636 appendix test verifier
- **THEN** the result equals the appendix test challenge string

### Requirement: Callback state matrix

A callback presenting an authorization code and a state SHALL be resolved against persisted intents so that four outcomes are distinct: an unknown state is rejected as unmatchable; a known but expired intent is rejected as expired and cannot be consumed; a known, unexpired, unconsumed intent is accepted exactly once, returning its internal-user binding and verifier; and a second presentation of the same accepted state is rejected as a replay. Only the accepting outcome may proceed to exchange.

#### Scenario: Unknown state is unmatchable

- **WHEN** a callback arrives with a state value that matches no persisted intent
- **THEN** resolution fails with the unmatched-state outcome and no intent record changes

#### Scenario: Expired intent is rejected and never consumed

- **WHEN** a callback presents the state of an intent whose expiry has passed
- **THEN** resolution fails with the expired outcome and the intent remains unusable for any later callback

#### Scenario: Valid callback is accepted once with its binding

- **WHEN** a callback presents the state of a live unconsumed intent belonging to the requesting user
- **THEN** resolution succeeds once, exposes the intent's internal-user binding and verifier, and marks the intent consumed

#### Scenario: Replayed state is rejected

- **WHEN** the same state is presented again after its intent was already consumed
- **THEN** resolution fails with the replayed-state outcome and no further acceptance is possible for that intent

### Requirement: Code exchange and credential activation

Exchanging an accepted intent SHALL send the authorization code with the stored verifier to the configured provider token endpoint, and on a successful response SHALL persist the returned access and refresh tokens only in encrypted form together with the exact granted scope list and the returned expiry, and SHALL mark the account connected. A token response that omits the scope parameter SHALL be treated as granting exactly the requested scopes, per RFC 6749. Provider HTTP shapes remain behind the service boundary.

#### Scenario: Successful exchange stores encrypted tokens and granted scopes

- **WHEN** the provider accepts the code and verifier and returns tokens, an expiry, and a scope list
- **THEN** the stored credential payload decrypts to the returned token pair under the configured key, the recorded granted scope list equals the provider's response verbatim, the expiry is recorded, and the account reads as connected

#### Scenario: Scope-less response means granted equals requested

- **WHEN** the provider accepts the exchange and returns tokens without a scope parameter
- **THEN** the connection activates with the requested read set recorded as granted, without any downgrade refusal

### Requirement: Scope minimization and downgrade refusal

The read-only connection SHALL request only the configured minimal read scope set, which never includes write scopes. When the provider grants a scope set that omits any requested read scope, the connection SHALL be refused with an error naming the missing scopes and SHALL NOT activate a credential. The exact granted scopes SHALL be recorded whenever a grant is observed.

#### Scenario: Downgraded grant refuses activation

- **WHEN** the provider's grant omits at least one requested read scope
- **THEN** the connection fails with a typed downgrade error naming every missing scope, no active credential exists for the account, and the granted scopes that were observed are still auditable

#### Scenario: Read connection never requests write scopes

- **WHEN** the authorization URL for a default read-only connection is built
- **THEN** the requested scope list equals the minimal read set and contains no bookmark-write scope

### Requirement: Encrypted credential storage at rest

Credential payloads SHALL be sealed into a self-describing encrypted envelope with a leading format-version marker using AES-256-GCM under a key taken from configuration, and SHALL use a fresh random nonce per sealing. Each envelope SHALL bind one owner and one purpose as authenticated data — the account for credential envelopes, the requesting internal user for authorization-intent verifier envelopes — so an envelope only opens when the opener supplies the same owner and purpose from trusted context. The key SHALL never be written to the database or logs. Decryption SHALL reject tampered ciphertexts, foreign keys, envelopes presented under a different owner or purpose, envelopes with an unrecognized format version, and absent or malformed configuration keys with typed errors.

#### Scenario: Round trip preserves the plaintext

- **WHEN** a credential payload is sealed under a key and then opened with the same key and account binding
- **THEN** the opened payload equals the original byte for byte

#### Scenario: Tampered ciphertext is rejected

- **WHEN** any byte of a sealed envelope is flipped and the envelope is opened
- **THEN** opening fails with an authentication error and no plaintext is produced

#### Scenario: Wrong key is rejected

- **WHEN** a sealed envelope is opened under a different valid key
- **THEN** opening fails with an authentication error

#### Scenario: Envelope does not survive owner relocation

- **WHEN** a sealed envelope is opened with its original key but attributed to a different account than it was sealed for, or an intent-verifier envelope is attributed to a different internal user
- **THEN** opening fails with the binding error

#### Scenario: Purpose confusion is rejected

- **WHEN** an intent-verifier envelope is opened expecting the credential purpose, or a credential envelope is opened expecting the intent-verifier purpose
- **THEN** opening fails with the binding error even though owner and key match

#### Scenario: Unknown envelope version is refused

- **WHEN** an envelope whose leading format marker names no implemented version is opened
- **THEN** opening fails with a typed version error and no decryption attempt is made

#### Scenario: Missing or malformed key is a typed configuration failure

- **WHEN** sealing or opening is attempted without a configured key, or with a configured value that is not 32 bytes of valid base64url
- **THEN** the operation fails with a typed key error naming the problem, and no fallback key is generated

### Requirement: Refresh with rotation and reuse detection

Refreshing credentials SHALL be serialized per account so that concurrent refresh attempts cannot race: each attempt classifies the presented token against the currently stored refresh token and the retained retired-token hash while holding exclusive access to the credential row, calls the provider, and swaps in the returned access and refresh tokens atomically before releasing it. The hash of the immediately retired refresh token SHALL be retained after rotation. Presenting a refresh token whose hash equals the retained retired hash SHALL be treated as reuse: the credential family is revoked, secret material is scrubbed, and the account requires reauthorization. Presenting a refresh token that matches neither the current nor the retired token SHALL be refused without disturbing the stored credential. A refresh attempt against a credential that is not active SHALL be refused locally without any provider contact. An upstream rejection of the current refresh token SHALL mark the account as requiring reauthorization, while transport-level failures SHALL leave stored state untouched.

#### Scenario: Refresh rotates both tokens atomically

- **WHEN** a refresh succeeds against the provider for the currently stored refresh token
- **THEN** the stored payload decrypts to the newly returned token pair, the retired token's hash is recorded, and no intermediate state exposes either the old access token alone or a mismatched token pair

#### Scenario: Concurrent refreshes serialize without false revocation

- **WHEN** two refreshes for one account run concurrently
- **THEN** both complete against the provider in serialized order using the token current at their turn, and neither attempt produces a reuse classification, family revocation, or reauth-required state

#### Scenario: Retired refresh token reuse revokes the family

- **WHEN** a refresh presents the retired refresh token after a successful rotation
- **THEN** the attempt is refused as reuse, the stored credential payload is scrubbed, the credential status becomes revoked, and the account requires reauthorization

#### Scenario: Unknown stale token is refused without revocation

- **WHEN** a refresh presents a token that matches neither the stored nor the retired refresh token
- **THEN** the attempt is refused as stale, and the stored credential remains active and unchanged

#### Scenario: Inactive credential refuses refresh without provider contact

- **WHEN** a refresh is attempted for an account whose credential status is revoked or expired
- **THEN** the attempt is refused locally with the typed inactive error and the provider receives no request

#### Scenario: Upstream invalidation is distinct from transient failure

- **WHEN** the provider answers a refresh with an invalid-grant rejection versus when the call fails at the transport level
- **THEN** the invalid-grant answer marks the account as requiring reauthorization while leaving the payload stored, whereas the transport failure returns a typed transient error and changes no stored state

### Requirement: Revocation scrubs material

Revoking a connection SHALL notify the provider's revocation endpoint with the current token, and regardless of whether the provider confirms, SHALL empty the stored credential payload, set the credential status to revoked, and mark the account revoked. Revoking an already-revoked or absent connection SHALL succeed idempotently.

#### Scenario: Revocation scrubs local secret material

- **WHEN** an active connection is revoked while the provider confirms the revocation
- **THEN** the stored credential payload holds no recoverable token bytes, the credential status reads revoked, and the account reads revoked

#### Scenario: Revocation is idempotent

- **WHEN** revocation runs again for an already-revoked connection, or for an account holding no credential
- **THEN** the operation reports success without contacting the provider again and leaves state consistent

### Requirement: Credential secrecy in diagnostics

Typed OAuth errors, their rendered operator messages, and the debug renderings of credential payload types SHALL NOT contain token values, verifier values, or decrypted payload contents. A diagnostic rendering of any credential-flow value can be checked against the secret inputs that produced it.

#### Scenario: Error renderings exclude secret inputs

- **WHEN** any credential-flow operation fails with secret values in play and the resulting error is rendered for operators
- **THEN** none of the secret input strings appear in the rendering

#### Scenario: Payload debug rendering excludes token material

- **WHEN** a decrypted credential or intent-verifier value is formatted with its `Debug` implementation while holding known marker secrets
- **THEN** none of the marker secrets appear in the rendered output
