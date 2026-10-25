//! Agent chat on Android: the HTTP the API backends send, through Android's own HTTP stack
//! (HttpURLConnection: the system's trusted CAs and proxy), and the API keys, kept encrypted with
//! a key in the Android Keystore (MainActivity.secretGet/secretSet/secretDelete).

use solidcraft_ui_egui::agent_chat::backend::{HttpRequest, Secrets};

use crate::bridge;

/// As the desktop: no redirects (a key must not follow one elsewhere), at most 16 MB back.
pub fn agent_http(r: &HttpRequest) -> Result<(u16, Vec<u8>), String> {
    bridge::http(r.method, &r.url, &r.headers, r.body.as_deref(), r.timeout_s.clamp(1, 900))
}

/// API keys, one per agent id, AES-GCM encrypted with a non-exportable Keystore key.
pub struct Keystore;

impl Secrets for Keystore {
    fn get(&self, id: &str) -> Result<Option<String>, String> {
        bridge::secret_get(id)
    }
    fn set(&self, id: &str, secret: &str) -> Result<(), String> {
        bridge::secret_set(id, Some(secret))
    }
    fn delete(&self, id: &str) -> Result<(), String> {
        bridge::secret_set(id, None)
    }
}