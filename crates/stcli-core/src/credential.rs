use std::time::Duration;

use keyring::{Entry, Error};
use thiserror::Error;

const SERVICE_NAME: &str = "stcli";
const KEYRING_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CredentialError {
    #[error("credential was not found")]
    Missing,
    #[error("Credential Store operation failed: {0}")]
    Store(String),
}

pub trait CredentialResolver: Send + Sync {
    fn get(&self, key: &str) -> Result<String, CredentialError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCredentialStore;

impl CredentialResolver for SystemCredentialStore {
    fn get(&self, key: &str) -> Result<String, CredentialError> {
        get_credential(key)
    }
}

pub fn get_credential(key: &str) -> Result<String, CredentialError> {
    let key = key.to_owned();
    call_with_timeout(KEYRING_TIMEOUT, move || get_from_entry(&entry(&key)?))
}

pub fn set_credential(key: &str, secret: &str) -> Result<(), CredentialError> {
    let (key, secret) = (key.to_owned(), secret.to_owned());
    call_with_timeout(KEYRING_TIMEOUT, move || {
        set_on_entry(&entry(&key)?, &secret)
    })
}

pub fn delete_credential(key: &str) -> Result<(), CredentialError> {
    let key = key.to_owned();
    call_with_timeout(KEYRING_TIMEOUT, move || delete_from_entry(&entry(&key)?))
}

fn call_with_timeout<T: Send + 'static>(
    timeout: Duration,
    operation: impl FnOnce() -> Result<T, CredentialError> + Send + 'static,
) -> Result<T, CredentialError> {
    // The OS keyring is not trusted to be prompt: a stalled Secret Service
    // would otherwise block the calling thread indefinitely.
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(operation());
    });
    receiver.recv_timeout(timeout).unwrap_or_else(|_| {
        Err(CredentialError::Store(format!(
            "Credential Store did not respond within {} seconds; the system keyring or Secret Service may be locked or stalled. Use `api_key_env` to supply the key via an environment variable instead.",
            timeout.as_secs()
        )))
    })
}

fn get_from_entry(entry: &Entry) -> Result<String, CredentialError> {
    entry.get_password().map_err(map_keyring_error)
}

fn set_on_entry(entry: &Entry, secret: &str) -> Result<(), CredentialError> {
    entry.set_password(secret).map_err(map_keyring_error)
}

fn delete_from_entry(entry: &Entry) -> Result<(), CredentialError> {
    entry.delete_credential().map_err(map_keyring_error)
}

fn entry(key: &str) -> Result<Entry, CredentialError> {
    Entry::new(SERVICE_NAME, key).map_err(map_keyring_error)
}

fn map_keyring_error(error: Error) -> CredentialError {
    match error {
        Error::NoEntry => CredentialError::Missing,
        error => CredentialError::Store(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_namespace_supports_set_get_and_delete() {
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        let entry = Entry::new(SERVICE_NAME, "stcli-credential-round-trip").unwrap();

        set_on_entry(&entry, "secret").unwrap();
        assert_eq!(get_from_entry(&entry).unwrap(), "secret");
        delete_from_entry(&entry).unwrap();
        assert_eq!(get_from_entry(&entry), Err(CredentialError::Missing));
    }

    #[test]
    fn stalled_keyring_operation_times_out_with_store_error() {
        let (release, stalled) = std::sync::mpsc::channel::<()>();
        let result = call_with_timeout(Duration::from_millis(50), move || {
            // Simulates a keyring daemon that never answers.
            let _ = stalled.recv();
            Ok("never".to_owned())
        });
        drop(release);
        assert!(matches!(
            result,
            Err(CredentialError::Store(message))
                if message.contains("did not respond within")
                    && message.contains("api_key_env")
        ));
    }
}
