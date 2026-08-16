use std::fmt::Write as _;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Non-2xx response. `body` is the first 300 chars of the response text.
    #[error("{provider} {status}: {body}")]
    Http {
        provider: &'static str,
        status: u16,
        body: String,
    },
    #[error("{provider}: {source}")]
    Transport {
        provider: &'static str,
        #[source]
        source: reqwest::Error,
    },
    /// Malformed or unexpected response payload (bad JSON, empty body, undecodable audio).
    #[error("{provider}: {message}")]
    Protocol {
        provider: &'static str,
        message: String,
    },
}

impl Error {
    pub(crate) fn protocol(provider: &'static str, message: impl Into<String>) -> Self {
        Error::Protocol {
            provider,
            message: message.into(),
        }
    }

    pub(crate) fn transport(provider: &'static str) -> impl FnOnce(reqwest::Error) -> Self {
        move |source| Error::Transport { provider, source }
    }

    /// Like `errors.ts` `ProviderError`: keep the first 300 chars of the body.
    pub(crate) fn http(provider: &'static str, status: u16, body: &str) -> Self {
        let mut short = String::new();
        for c in body.chars().take(300) {
            let _ = short.write_char(c);
        }
        Error::Http {
            provider,
            status,
            body: short,
        }
    }
}
