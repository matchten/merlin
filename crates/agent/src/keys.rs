//! API keys. An environment variable wins (development, CI); otherwise the OS keychain (macOS
//! Keychain, Windows Credential Manager, the Linux kernel keyring), one entry per provider.
//!
//! Keys never go into preferences, UI state, project files or logs: scripts, agents and the control
//! channel can read all of those.

use crate::config::ProviderKind;
#[cfg(not(target_arch = "wasm32"))]
use crate::{Error, Result};

/// Keychain service name.
pub const SERVICE: &str = "EffectCraft Agent";

/// Where a provider's key comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    Env,
    Keychain,
    Missing,
}

/// The key for `provider`, and where it came from. A keychain failure reads as missing (the
/// panel offers to enter the key again) and is logged without the key.
pub fn get(provider: ProviderKind) -> (Option<String>, KeySource) {
    if let Some(k) = std::env::var(provider.env_var()).ok().filter(|k| !k.trim().is_empty()) {
        return (Some(k), KeySource::Env);
    }
    match keychain_get(provider) {
        Some(k) => (Some(k), KeySource::Keychain),
        None => (None, KeySource::Missing),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn entry(provider: ProviderKind) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, provider.id()).map_err(|e| Error::Keychain(e.to_string()))
}

#[cfg(not(target_arch = "wasm32"))]
fn keychain_get(provider: ProviderKind) -> Option<String> {
    match entry(provider).ok()?.get_password() {
        Ok(k) if !k.trim().is_empty() => Some(k),
        Ok(_) | Err(keyring::Error::NoEntry) => None,
        Err(e) => {
            log::warn!("agent: cannot read the {} key from the keychain: {e}", provider.id());
            None
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn keychain_get(_: ProviderKind) -> Option<String> {
    None
}

/// Store `key` for `provider` in the keychain.
#[cfg(not(target_arch = "wasm32"))]
pub fn set(provider: ProviderKind, key: &str) -> Result<()> {
    let key = key.trim();
    if key.is_empty() {
        return remove(provider);
    }
    entry(provider)?.set_password(key).map_err(|e| Error::Keychain(e.to_string()))
}

/// Remove `provider`'s key from the keychain (no error when there was none).
#[cfg(not(target_arch = "wasm32"))]
pub fn remove(provider: ProviderKind) -> Result<()> {
    match entry(provider)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(Error::Keychain(e.to_string())),
    }
}

/// `sk-ant-…wxyz` for display: never the whole key.
pub fn masked(key: &str) -> String {
    let k = key.trim();
    let n = k.chars().count();
    if n <= 8 {
        return "••••".into();
    }
    let head: String = k.chars().take(6).collect();
    let tail: String = k.chars().skip(n - 4).collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masking_hides_the_key() {
        assert_eq!(masked("sk-ant-api03-abcdefghijklmnop-wxyz"), "sk-ant…wxyz");
        assert_eq!(masked("short"), "••••");
        assert_eq!(masked("ééééééééééé"), "éééééé…éééé");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn keychain_round_trip_with_mock_store() {
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        // The mock store keeps nothing between entries; exercise set/remove error paths only.
        assert!(set(ProviderKind::Custom, "  ").is_ok());
        assert!(remove(ProviderKind::Custom).is_ok());
    }
}
