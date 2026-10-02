//! Typed API failures and request-parameter parsing — a port of upstream
//! `src/ui-server/api/respond.ts`.
//!
//! Every `/api/` outcome is JSON: `{error, code, hint?}` with the status the code
//! maps to. Parameters are validated, never clamped: a value out of range is a
//! 400 that says so, because clamping would answer a different question.

use serde_json::{Value, json};

/// The error vocabulary of the wire. Each maps to exactly one HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorCode {
    BadRequest,
    NotFound,
    Refused,
    NoIndex,
    IndexUnusable,
    Internal,
}

impl ApiErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadRequest => "bad-request",
            Self::NotFound => "not-found",
            Self::Refused => "refused",
            Self::NoIndex => "no-index",
            Self::IndexUnusable => "index-unusable",
            Self::Internal => "internal",
        }
    }

    pub fn status(self) -> u16 {
        match self {
            Self::BadRequest => 400,
            Self::NotFound => 404,
            Self::Refused => 403,
            Self::NoIndex | Self::IndexUnusable => 503,
            Self::Internal => 500,
        }
    }
}

/// A failure the API answers as `{error, code, hint?}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub code: ApiErrorCode,
    pub message: String,
    /// Optional second line: what the user can do about it.
    pub hint: Option<String>,
}

impl ApiError {
    pub fn new(code: ApiErrorCode, message: impl Into<String>, hint: Option<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::BadRequest, message, None)
    }

    pub fn not_found(message: impl Into<String>, hint: Option<&str>) -> Self {
        Self::new(ApiErrorCode::NotFound, message, hint.map(str::to_string))
    }

    pub fn refused(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::Refused, message, None)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::Internal, message, None)
    }

    /// The same failure with a second line saying what to do about it.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn status(&self) -> u16 {
        self.code.status()
    }

    pub fn body(&self) -> Value {
        let mut body = json!({ "error": self.message, "code": self.code.as_str() });
        if let Some(hint) = &self.hint {
            body["hint"] = Value::String(hint.clone());
        }
        body
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

impl From<rusqlite::Error> for ApiError {
    fn from(err: rusqlite::Error) -> Self {
        ApiError::internal(err.to_string())
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

/// Request bodies above this are refused (64 KB, counted in bytes).
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Text parameters above this many characters are refused.
pub const MAX_QUERY_LENGTH: usize = 2_000;

/// A parsed query string, keeping repeated keys in order (`id=a&id=b`), with
/// `URLSearchParams` decoding (`+` is a space).
#[derive(Debug, Clone, Default)]
pub struct Query {
    pairs: Vec<(String, String)>,
}

impl Query {
    pub fn parse(raw: Option<&str>) -> Self {
        let pairs = raw
            .map(|q| {
                form_urlencoded::parse(q.as_bytes())
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect()
            })
            .unwrap_or_default();
        Self { pairs }
    }

    /// The first value of `name`, like `URLSearchParams.get`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Every value of `name`, in order, like `URLSearchParams.getAll`.
    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.pairs
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    pub fn has(&self, name: &str) -> bool {
        self.pairs.iter().any(|(k, _)| k == name)
    }

    /// A required parameter: missing or blank is a 400.
    pub fn required(&self, name: &str) -> ApiResult<&str> {
        match self.get(name) {
            Some(raw) if !raw.trim().is_empty() => Ok(raw),
            _ => Err(missing(name)),
        }
    }

    /// A whole number in `[min, max]`; blank or absent takes `default` (a 400 when
    /// there is none). Never clamps.
    pub fn int(&self, name: &str, min: i64, max: i64, default: Option<i64>) -> ApiResult<i64> {
        let raw = match self.get(name) {
            Some(raw) if !raw.trim().is_empty() => raw,
            _ => return default.ok_or_else(|| missing(name)),
        };
        match parse_js_integer(raw) {
            Some(value) if (min..=max).contains(&value) => Ok(value),
            _ => Err(ApiError::bad_request(format!(
                "Parameter \"{name}\" must be a whole number between {min} and {max} (got \"{raw}\")."
            ))),
        }
    }

    /// A required, length-bounded text parameter.
    pub fn text(&self, name: &str) -> ApiResult<&str> {
        let raw = self.required(name)?;
        bound_length(raw, name)
    }

    /// A text parameter that must be present but may be empty (`?q=`).
    pub fn optional_text(&self, name: &str) -> ApiResult<&str> {
        match self.get(name) {
            Some(raw) => bound_length(raw, name),
            None => Err(missing(name)),
        }
    }

    /// `name=1` — the flag form the viewer sends.
    pub fn flag(&self, name: &str) -> bool {
        self.get(name) == Some("1")
    }
}

fn missing(name: &str) -> ApiError {
    ApiError::bad_request(format!("Missing required parameter \"{name}\"."))
}

fn bound_length<'a>(raw: &'a str, name: &str) -> ApiResult<&'a str> {
    if raw.encode_utf16().count() > MAX_QUERY_LENGTH {
        return Err(ApiError::bad_request(format!(
            "Parameter \"{name}\" is too long (max {MAX_QUERY_LENGTH} characters)."
        )));
    }
    Ok(raw)
}

/// `Number(raw)` followed by `Number.isInteger`, restricted to what a query can
/// sensibly carry: optional surrounding whitespace, an optional sign, decimal
/// digits, and an optional all-zero fraction or exponent form JavaScript accepts
/// (`"4.0"`, `"1e1"`).
fn parse_js_integer(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value: f64 = trimmed.parse().ok()?;
    if !value.is_finite() || value.fract() != 0.0 || value.abs() > 9_007_199_254_740_991.0 {
        return None;
    }
    Some(value as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_are_validated_not_clamped() {
        let q = Query::parse(Some("limit=5&big=999&bad=2.5&neg=-1&blank=&exp=1e1"));
        assert_eq!(q.int("limit", 1, 200, Some(60)).unwrap(), 5);
        assert_eq!(q.int("absent", 1, 200, Some(60)).unwrap(), 60);
        assert_eq!(q.int("blank", 1, 200, Some(60)).unwrap(), 60);
        assert_eq!(q.int("exp", 1, 200, None).unwrap(), 10);
        assert!(q.int("big", 1, 200, Some(60)).is_err());
        assert!(q.int("bad", 1, 200, Some(60)).is_err());
        assert!(q.int("neg", 1, 200, Some(60)).is_err());
        assert!(q.int("absent", 1, 200, None).is_err());
    }

    #[test]
    fn repeated_keys_keep_their_order_and_plus_is_a_space() {
        let q = Query::parse(Some("id=a&id=b&q=how+does+x"));
        assert_eq!(q.get_all("id"), vec!["a", "b"]);
        assert_eq!(q.get("q"), Some("how does x"));
    }

    #[test]
    fn text_is_bounded() {
        let long = "x".repeat(MAX_QUERY_LENGTH + 1);
        let q = Query::parse(Some(&format!("q={long}&e=")));
        assert!(q.text("q").is_err());
        assert_eq!(q.optional_text("e").unwrap(), "");
        assert!(
            q.text("e").is_err(),
            "a required text parameter cannot be blank"
        );
    }

    #[test]
    fn error_bodies_carry_code_and_hint() {
        let err = ApiError::not_found("No symbol.", Some("Search by name."));
        assert_eq!(err.status(), 404);
        assert_eq!(err.body()["code"], "not-found");
        assert_eq!(err.body()["hint"], "Search by name.");
        assert!(ApiError::bad_request("x").body().get("hint").is_none());
    }
}
