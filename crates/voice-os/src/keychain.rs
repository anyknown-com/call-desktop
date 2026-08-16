//! Secrets in the OS credential store (macOS Keychain, Secret Service, Windows Credential
//! Manager). All entries live under the service name [`SERVICE`] unless the caller passes another.

use crate::{Error, Result};

pub const SERVICE: &str = "com.anyknown.voice";

fn entry(service: &str, account: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(service, account).map_err(|e| Error::Keychain(e.to_string()))
}

pub fn get(service: &str, account: &str) -> Result<Option<String>> {
    match entry(service, account)?.get_password() {
        Ok(s) => Ok(Some(s)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(Error::Keychain(e.to_string())),
    }
}

pub fn set(service: &str, account: &str, secret: &str) -> Result<()> {
    entry(service, account)?
        .set_password(secret)
        .map_err(|e| Error::Keychain(e.to_string()))
}

pub fn delete(service: &str, account: &str) -> Result<()> {
    match entry(service, account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(Error::Keychain(e.to_string())),
    }
}
