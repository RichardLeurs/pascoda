use crate::client::ApiKey;

/// Where the API key lives: one generic password in the macOS Keychain.
#[derive(Debug, Clone)]
pub struct Keystore {
    pub service: String,
    pub account: String,
    /// An older service name to move the entry from on first load (see `load`).
    pub legacy_service: Option<String>,
}

impl Keystore {
    /// Service `Pascoda`, account `anthropic`.
    pub fn app() -> Self {
        Keystore {
            service: "Pascoda".into(),
            account: "anthropic".into(),
            legacy_service: Some(LEGACY_SERVICE.into()),
        }
    }
    /// Service `Pascoda`, account `openai` (the OpenAI-compatible provider).
    pub fn openai() -> Self {
        Keystore {
            service: "Pascoda".into(),
            account: "openai".into(),
            legacy_service: Some(LEGACY_SERVICE.into()),
        }
    }
    pub fn new(service: &str, account: &str) -> Self {
        Keystore {
            service: service.into(),
            account: account.into(),
            legacy_service: None,
        }
    }
    /// Also look in `service` when this one has no entry (tests use throwaway names).
    pub fn with_legacy(mut self, service: &str) -> Self {
        self.legacy_service = Some(service.into());
        self
    }
}

/// The Keychain service name before the app was renamed.
const LEGACY_SERVICE: &str = "Pascora";

#[cfg(target_os = "macos")]
impl Keystore {
    pub fn save(&self, key: &ApiKey) -> Result<(), String> {
        security_framework::passwords::set_generic_password(
            &self.service,
            &self.account,
            key.expose().as_bytes(),
        )
        .map_err(|e| format!("Keychain: {e}"))
    }

    /// `Ok(None)` when no key is stored. When this service has no entry but the legacy service
    /// has, the key is copied to this service, the legacy item is deleted, and the key returned.
    pub fn load(&self) -> Result<Option<ApiKey>, String> {
        if let Some(key) = self.load_from(&self.service)? {
            return Ok(Some(key));
        }
        let Some(legacy) = self.legacy_service.as_deref() else {
            return Ok(None);
        };
        let Some(key) = self.load_from(legacy).ok().flatten() else {
            return Ok(None);
        };
        // Only remove the old item once the new one is safely stored.
        if self.save(&key).is_ok() {
            let _ = security_framework::passwords::delete_generic_password(legacy, &self.account);
        }
        Ok(Some(key))
    }

    fn load_from(&self, service: &str) -> Result<Option<ApiKey>, String> {
        use security_framework::passwords::get_generic_password;
        match get_generic_password(service, &self.account) {
            Ok(bytes) => Ok(String::from_utf8(bytes)
                .ok()
                .map(ApiKey::new)
                .filter(|k| !k.is_empty())),
            // errSecItemNotFound
            Err(e) if e.code() == -25300 => Ok(None),
            Err(e) => Err(format!("Keychain: {e}")),
        }
    }

    /// Removing a missing key is not an error.
    pub fn delete(&self) -> Result<(), String> {
        match security_framework::passwords::delete_generic_password(&self.service, &self.account) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == -25300 => Ok(()),
            Err(e) => Err(format!("Keychain: {e}")),
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl Keystore {
    pub fn save(&self, _key: &ApiKey) -> Result<(), String> {
        Err("the Keychain is only available on macOS".into())
    }
    pub fn load(&self) -> Result<Option<ApiKey>, String> {
        Ok(None)
    }
    pub fn delete(&self) -> Result<(), String> {
        Ok(())
    }
}

/// The Keychain entry, else `ANTHROPIC_API_KEY`.
pub fn resolve_key(store: &Keystore) -> Option<ApiKey> {
    if let Ok(Some(k)) = store.load() {
        return Some(k);
    }
    std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .map(ApiKey::new)
        .filter(|k| !k.is_empty())
}

/// The OpenAI Keychain entry, else `OPENAI_API_KEY`.
pub fn resolve_openai_key(store: &Keystore) -> Option<ApiKey> {
    if let Ok(Some(k)) = store.load() {
        return Some(k);
    }
    std::env::var("OPENAI_API_KEY")
        .ok()
        .map(ApiKey::new)
        .filter(|k| !k.is_empty())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_in_a_throwaway_service() {
        let svc = format!(
            "PascodaTest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let store = Keystore::new(&svc, "test");
        assert!(store.load().unwrap().is_none());
        store.save(&ApiKey::new("dummy-not-a-key")).unwrap();
        let got = store.load().unwrap().unwrap();
        assert_eq!(got.expose(), "dummy-not-a-key");
        store.save(&ApiKey::new("second")).unwrap(); // overwrite
        assert_eq!(store.load().unwrap().unwrap().expose(), "second");
        store.delete().unwrap();
        assert!(store.load().unwrap().is_none());
        store.delete().unwrap();
    }

    #[test]
    fn the_openai_entry_is_separate_from_the_anthropic_one() {
        let (a, o) = (Keystore::app(), Keystore::openai());
        assert_eq!(
            (o.service.as_str(), o.account.as_str()),
            ("Pascoda", "openai")
        );
        assert_eq!(a.service, o.service);
        assert_ne!(a.account, o.account);
        // The same account name in a throwaway service, so the real entry is never touched.
        let svc = format!("PascodaTest-openai-{}", std::process::id());
        let store = Keystore::new(&svc, &o.account);
        assert!(store.load().unwrap().is_none());
        store.save(&ApiKey::new("sk-dummy")).unwrap();
        assert_eq!(store.load().unwrap().unwrap().expose(), "sk-dummy");
        store.delete().unwrap();
        assert!(store.load().unwrap().is_none());
    }

    fn throwaway(tag: &str) -> String {
        format!(
            "PascodaTest-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    #[test]
    fn a_legacy_entry_moves_to_the_new_service_on_load() {
        let (old, new) = (throwaway("old"), throwaway("new"));
        let legacy = Keystore::new(&old, "acct");
        legacy.save(&ApiKey::new("sk-legacy")).unwrap();
        let store = Keystore::new(&new, "acct").with_legacy(&old);
        assert_eq!(store.load().unwrap().unwrap().expose(), "sk-legacy");
        // Copied to the new service, gone from the old one.
        assert!(legacy.load().unwrap().is_none());
        assert_eq!(
            Keystore::new(&new, "acct")
                .load()
                .unwrap()
                .unwrap()
                .expose(),
            "sk-legacy"
        );
        store.delete().unwrap();
    }

    #[test]
    fn the_new_entry_wins_and_the_legacy_one_stays() {
        let (old, new) = (throwaway("old2"), throwaway("new2"));
        let legacy = Keystore::new(&old, "acct");
        legacy.save(&ApiKey::new("sk-old")).unwrap();
        let store = Keystore::new(&new, "acct").with_legacy(&old);
        store.save(&ApiKey::new("sk-new")).unwrap();
        assert_eq!(store.load().unwrap().unwrap().expose(), "sk-new");
        assert_eq!(legacy.load().unwrap().unwrap().expose(), "sk-old");
        legacy.delete().unwrap();
        store.delete().unwrap();
    }

    #[test]
    fn nothing_anywhere_is_none_and_the_key_is_never_printed() {
        let (old, new) = (throwaway("old3"), throwaway("new3"));
        let store = Keystore::new(&new, "acct").with_legacy(&old);
        assert!(store.load().unwrap().is_none());
        let key = ApiKey::new("sk-secret-value");
        assert!(!format!("{key:?}").contains("sk-secret-value"));
        assert!(!format!("{store:?}").contains("sk-secret-value"));
    }
}
