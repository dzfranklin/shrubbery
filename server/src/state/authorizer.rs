use serde_json::value::RawValue;
use std::collections::HashMap;
use std::fmt::Formatter;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::warn;

// TODO: Clean out expired tokens

#[derive(Clone)]
pub struct Authorizer(Arc<Mutex<Inner>>);

struct Inner {
    root_token: String,
    by_token: HashMap<String, Entry>,
    by_user: HashMap<String, Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub user: String,
    pub expiry: Instant,
    pub info: Option<serde_json::Value>,
}

impl Authorizer {
    pub fn new(root_token: String) -> Self {
        Self(Arc::new(Mutex::new(Inner {
            root_token,
            by_token: HashMap::new(),
            by_user: HashMap::new(),
        })))
    }

    pub fn random_root_token() -> String {
        format!("shrubtoken1:ROOT{}", random_alphanum())
    }

    pub fn mint_token(&self, entry: Entry) -> String {
        let user = entry.user.clone();
        let token = format!("shrubtoken1:{}", random_alphanum());
        let mut inner = self.0.lock().unwrap();
        inner.by_token.insert(token.clone(), entry);
        inner.by_user.entry(user).or_default().push(token.clone());
        token
    }

    pub fn authenticate(&self, token: &str) -> Option<Entry> {
        let inner = self.0.lock().unwrap();
        if inner.root_token == token {
            return Some(Entry {
                user: "root".to_string(),
                expiry: Instant::now() + Duration::from_secs(60 * 60 * 24 * 365 * 100),
                info: None,
            });
        }
        let entry = inner.by_token.get(token)?.clone();
        if entry.expiry < Instant::now() {
            return None;
        }
        Some(entry.clone())
    }

    pub fn revoke_tokens_for_user(&self, user: String) {
        if user == "root" {
            warn!("revoke_tokens_for_user has no effect on root");
            return;
        }
        let mut inner = self.0.lock().unwrap();
        if let Some(tokens) = inner.by_user.remove(&user) {
            for token in tokens {
                inner.by_token.remove(&token);
            }
        }
    }
}

fn random_alphanum() -> String {
    use rand::Rng;
    rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(30)
        .map(char::from)
        .collect()
}

impl std::fmt::Debug for Authorizer {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authorizer").finish_non_exhaustive()
    }
}
