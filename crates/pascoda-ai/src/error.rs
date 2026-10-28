use std::fmt;

/// Failure of an API call or an agent turn. Never contains the API key.
#[derive(Debug, Clone, PartialEq)]
pub enum AiError {
    /// 401/403: missing, wrong or unauthorised key.
    Auth(String),
    /// 429. `retry_after` in seconds when the server said so.
    RateLimited {
        retry_after: Option<u64>,
        message: String,
    },
    /// 529 or an `overloaded_error` event.
    Overloaded(String),
    Api {
        status: u16,
        kind: String,
        message: String,
    },
    Network(String),
    Protocol(String),
    /// The Claude Code CLI is not signed in (subscription provider).
    NotLoggedIn,
    /// The Claude Code CLI is missing, died or reported an error (subscription provider). The
    /// text is ready to show.
    Cli(String),
    /// The provider is not set up (for example no model name); the text is ready to show.
    Config(String),
    Cancelled,
}

impl fmt::Display for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AiError::Auth(m) => write!(
                f,
                "The API key was rejected ({m}). Check the key in Settings."
            ),
            AiError::RateLimited {
                retry_after,
                message,
            } => match retry_after {
                Some(s) => write!(f, "Rate limit reached, try again in {s} s ({message})."),
                None => write!(f, "Rate limit reached, try again shortly ({message})."),
            },
            AiError::Overloaded(m) => write!(
                f,
                "The API is overloaded right now, try again in a moment ({m})."
            ),
            AiError::Api {
                status,
                kind,
                message,
            } => write!(f, "API error {status} {kind}: {message}"),
            AiError::Network(m) => write!(f, "Network error: {m}"),
            AiError::Protocol(m) => write!(f, "Unexpected response: {m}"),
            AiError::NotLoggedIn => write!(
                f,
                "Claude Code is not logged in. Open a terminal, run `claude` once, type /login and sign in with your Claude account, then send your message again."
            ),
            AiError::Cli(m) | AiError::Config(m) => write!(f, "{m}"),
            AiError::Cancelled => write!(f, "Cancelled"),
        }
    }
}

impl std::error::Error for AiError {}

/// Map an API error `type` (HTTP body or SSE `error` event) to an error.
pub(crate) fn from_api(
    status: u16,
    kind: &str,
    message: &str,
    retry_after: Option<u64>,
) -> AiError {
    match (status, kind) {
        (401 | 403, _) | (_, "authentication_error" | "permission_error") => {
            AiError::Auth(message.to_string())
        }
        (429, _) | (_, "rate_limit_error") => AiError::RateLimited {
            retry_after,
            message: message.to_string(),
        },
        (529, _) | (_, "overloaded_error") => AiError::Overloaded(message.to_string()),
        _ => AiError::Api {
            status,
            kind: kind.to_string(),
            message: message.to_string(),
        },
    }
}
