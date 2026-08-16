//! Shared HTTP helpers.

use reqwest::Response;

use crate::{Error, Result};

/// Port of `throwIfNotOk`: pass through 2xx, otherwise read the body into an `Error::Http`.
pub(crate) async fn check_status(provider: &'static str, res: Response) -> Result<Response> {
    let status = res.status();
    if status.is_success() {
        return Ok(res);
    }
    let reason = status.canonical_reason().unwrap_or("").to_string();
    let body = res.text().await.unwrap_or_default();
    let body = if body.is_empty() { reason } else { body };
    Err(Error::http(provider, status.as_u16(), &body))
}
