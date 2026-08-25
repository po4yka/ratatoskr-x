//! The provider token-endpoint adapter over `reqwest`.
//!
//! Provider HTTP shapes stay inside this module: nothing upstream sees a raw
//! provider struct, an unparsed body, or a token value.

use std::time::Duration;

use crate::error::FlowError;

/// How long one provider token/revocation request may take end to end.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The largest provider body this adapter will read, guarding against runaway responses.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// A successful provider token response, reduced to the fields the flows use.
#[derive(Debug, Clone)]
pub(crate) struct TokenResponse {
    /// The returned bearer access token.
    pub access_token: String,
    /// The returned refresh token; empty when the grant carries none.
    pub refresh_token: String,
    /// The access-token lifetime in seconds.
    pub expires_in: u64,
    /// The granted scope string, when the provider echoes one.
    pub scope: Option<String>,
}

/// Posts RFC 6749 / RFC 7009 requests to the configured provider endpoints.
#[derive(Clone)]
pub struct TokenClient {
    http: reqwest::Client,
    token_url: String,
    revocation_url: String,
    client_id: Option<String>,
    client_secret: Option<String>,
}

impl std::fmt::Debug for TokenClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenClient([redacted client secret])")
    }
}

impl TokenClient {
    /// Builds a client from the OAuth section of the configuration.
    ///
    /// # Errors
    /// When the underlying HTTP client cannot be built.
    pub fn new(oauth: &x_core::config::OauthConfig) -> Result<Self, FlowError> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| FlowError::Transport)?;
        Ok(Self {
            http,
            token_url: oauth.token_url.clone(),
            revocation_url: oauth.revocation_url.clone(),
            client_id: oauth.client_id.clone(),
            client_secret: oauth.client_secret.clone(),
        })
    }

    /// Exchanges one authorization code for tokens with the stored verifier.
    ///
    /// # Errors
    /// [`FlowError::Transport`] when the endpoint cannot be reached at all;
    /// [`FlowError::UpstreamInvalidation`] when a completed provider answer rejects
    /// the grant; [`FlowError::MalformedProviderResponse`] for unusable bodies.
    pub(crate) async fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
        code_verifier: &str,
    ) -> Result<TokenResponse, FlowError> {
        let fields = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", code_verifier),
        ];
        self.post_token_form(&fields).await
    }

    /// Refreshes tokens by presenting a current refresh token.
    ///
    /// # Errors
    /// See [`TokenClient::exchange_code`].
    pub(crate) async fn refresh_token(
        &self,
        refresh_token: &str,
    ) -> Result<TokenResponse, FlowError> {
        let fields = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ];
        self.post_token_form(&fields).await
    }

    /// Posts an RFC 7009 revocation for one token value.
    ///
    /// # Errors
    /// Only transport failures surface here: a completed response confirms the
    /// notification regardless of status, per the revocation flow's best-effort rule.
    pub(crate) async fn revoke(&self, token: &str) -> Result<(), FlowError> {
        let fields = [("token", token)];
        let _ = self.post_form_raw(&self.revocation_url, &fields).await?;
        Ok(())
    }

    /// Posts one authenticated form body to the token endpoint and parses a token
    /// response out of a successful answer.
    async fn post_token_form(&self, fields: &[(&str, &str)]) -> Result<TokenResponse, FlowError> {
        let (status, body) = self.post_form_raw(&self.token_url, fields).await?;
        if status.is_success() {
            return parse_token_response(&body);
        }
        // A completed provider answer that rejects this credential is an
        // invalidation: the provider demonstrably answered against the grant.
        // Only a request that never completed is [`FlowError::Transport`].
        Err(FlowError::UpstreamInvalidation)
    }

    /// Posts one form-encoded body with HTTP Basic client authentication when a
    /// secret is configured, returning `(status, body)` for classification.
    async fn post_form_raw(
        &self,
        url: &str,
        fields: &[(&str, &str)],
    ) -> Result<(reqwest::StatusCode, String), FlowError> {
        let request = self
            .http
            .post(url)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(urlencoded(fields));
        let mut request = request;
        if let (Some(id), Some(secret)) = (&self.client_id, &self.client_secret) {
            request = request.basic_auth(id, Some(secret));
        }
        let response = request.send().await.map_err(|_| FlowError::Transport)?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|len| len > MAX_BODY_BYTES as u64)
        {
            return Err(FlowError::MalformedProviderResponse);
        }
        let bytes = response.bytes().await.map_err(|_| FlowError::Transport)?;
        if bytes.len() > MAX_BODY_BYTES {
            return Err(FlowError::MalformedProviderResponse);
        }
        Ok((status, String::from_utf8_lossy(bytes.as_ref()).into_owned()))
    }
}

/// Percent-encodes one `application/x-www-form-urlencoded` component, keeping
/// RFC 3986 unreserved characters as-is.
fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(char::from(byte));
            }
            other => {
                let _ = std::fmt::Write::write_fmt(&mut encoded, format_args!("%{other:02X}"));
            }
        }
    }
    encoded
}

/// Serializes field pairs into one urlencoded request body.
fn urlencoded(fields: &[(&str, &str)]) -> String {
    let mut body = String::new();
    for (key, value) in fields {
        if !body.is_empty() {
            body.push('&');
        }
        body.push_str(&percent_encode(key));
        body.push('=');
        body.push_str(&percent_encode(value));
    }
    body
}

/// Parses a successful token-response body into its used fields.
fn parse_token_response(body: &str) -> Result<TokenResponse, FlowError> {
    let access_token =
        extract_string_member(body, "access_token")?.ok_or(FlowError::MalformedProviderResponse)?;
    let expires_in =
        extract_number_member(body, "expires_in")?.ok_or(FlowError::MalformedProviderResponse)?;
    let refresh_token = extract_string_member(body, "refresh_token")?.unwrap_or_default();
    Ok(TokenResponse {
        access_token,
        refresh_token,
        expires_in,
        scope: extract_string_member(body, "scope")?,
    })
}

/// Splits a flat JSON object body into its top-level `(key, raw value)` members.
fn split_top_level_members(body: &str) -> Result<Vec<(&str, &str)>, FlowError> {
    let malformed = || FlowError::MalformedProviderResponse;
    let trimmed = body.trim();
    let inner = trimmed
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .ok_or_else(malformed)?;
    let mut members = Vec::new();
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut start = 0usize;
    for (index, character) in inner.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '{' | '[' => depth = depth.saturating_add(1),
            '}' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                members.push(inner.get(start..index).ok_or_else(malformed)?);
                start = index + 1;
            }
            _ => {}
        }
    }
    if in_string {
        return Err(malformed());
    }
    members.push(inner.get(start..).ok_or_else(malformed)?);

    let mut pairs = Vec::with_capacity(members.len());
    for member in members {
        let member_trimmed = member.trim();
        if member_trimmed.is_empty() {
            continue;
        }
        let separator = member_trimmed.find(':').ok_or_else(malformed)?;
        let key = member_trimmed
            .get(..separator)
            .ok_or_else(malformed)?
            .trim();
        let value = member_trimmed.get(separator + 1..).ok_or_else(malformed)?;
        let key = key
            .strip_prefix('"')
            .and_then(|k| k.strip_suffix('"'))
            .ok_or_else(malformed)?;
        pairs.push((key, value.trim()));
    }
    Ok(pairs)
}

/// Extracts one string-valued top-level member, decoding JSON escapes.
fn extract_string_member(body: &str, key: &str) -> Result<Option<String>, FlowError> {
    let malformed = || FlowError::MalformedProviderResponse;
    let found = split_top_level_members(body)?
        .into_iter()
        .find(|(candidate, _)| *candidate == key)
        .map(|(_, value)| value);
    let Some(raw) = found else {
        return Ok(None);
    };
    let mut characters = raw.chars();
    if characters.next() != Some('"') {
        return Err(malformed());
    }
    let mut decoded = String::new();
    let mut pending_unicode: Option<u16> = None;
    while let Some(character) = characters.next() {
        match character {
            '"' => {
                return if characters.next().is_none() {
                    Ok(Some(decoded))
                } else {
                    Err(malformed())
                };
            }
            '\\' => {
                let escape = characters.next().ok_or_else(malformed)?;
                match escape {
                    '"' => decoded.push('"'),
                    '\\' => decoded.push('\\'),
                    '/' => decoded.push('/'),
                    'b' => decoded.push('\u{0008}'),
                    'f' => decoded.push('\u{000C}'),
                    'n' => decoded.push('\n'),
                    'r' => decoded.push('\r'),
                    't' => decoded.push('\t'),
                    'u' => {
                        let hex: String = characters.by_ref().take(4).collect();
                        let unit = u16::from_str_radix(&hex, 16).map_err(|_| malformed())?;
                        if (0xD800..0xDC00).contains(&unit) {
                            pending_unicode = Some(unit);
                        } else if (0xDC00..0xE000).contains(&unit) {
                            let high = pending_unicode.take().ok_or_else(malformed)?;
                            let combined = 0x10000
                                + ((u32::from(high) - 0xD800) << 10)
                                + (u32::from(unit) - 0xDC00);
                            decoded.push(char::from_u32(combined).ok_or_else(malformed)?);
                        } else {
                            decoded.push(char::from_u32(u32::from(unit)).ok_or_else(malformed)?);
                        }
                    }
                    _ => return Err(malformed()),
                }
            }
            plain => decoded.push(plain),
        }
    }
    Err(malformed())
}

/// Extracts one number-valued top-level member as a `u64`.
fn extract_number_member(body: &str, key: &str) -> Result<Option<u64>, FlowError> {
    let malformed = || FlowError::MalformedProviderResponse;
    let found = split_top_level_members(body)?
        .into_iter()
        .find(|(candidate, _)| *candidate == key)
        .map(|(_, value)| value);
    let Some(raw) = found else {
        return Ok(None);
    };
    if !raw.bytes().all(|byte| byte.is_ascii_digit()) || raw.is_empty() {
        return Err(malformed());
    }
    raw.parse::<u64>().map(Some).map_err(|_| malformed())
}
