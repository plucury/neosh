use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionCacheEntry {
    pub session_id: Uuid,
    #[serde(default)]
    pub ssh_target: String,
    pub resume_token: String,
    pub resume_token_expires_at: u64,
    pub quic_addr: String,
    pub cert_fingerprint: String,
    pub updated_at: u64,
}

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("io error: {0}")]
    Io(String),
    #[error("json error: {0}")]
    Json(String),
}

pub struct SessionCache {
    root: PathBuf,
}

impl SessionCache {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn default_root() -> PathBuf {
        let base = std::env::var("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                PathBuf::from(home).join(".cache")
            });
        base.join("neosh").join("sessions")
    }

    pub fn put(&self, entry: &SessionCacheEntry) -> Result<(), CacheError> {
        fs::create_dir_all(&self.root).map_err(|e| CacheError::Io(e.to_string()))?;
        let path = self.root.join(format!("{}.json", entry.session_id));
        let content = serde_json::to_vec_pretty(entry).map_err(|e| CacheError::Json(e.to_string()))?;
        fs::write(path, content).map_err(|e| CacheError::Io(e.to_string()))
    }

    pub fn get(&self, session_id: Uuid) -> Result<SessionCacheEntry, CacheError> {
        let path = self.root.join(format!("{}.json", session_id));
        let data = fs::read(path).map_err(|e| CacheError::Io(e.to_string()))?;
        serde_json::from_slice(&data).map_err(|e| CacheError::Json(e.to_string()))
    }

    pub fn delete(&self, session_id: Uuid) -> Result<(), CacheError> {
        let path = self.root.join(format!("{}.json", session_id));
        if path.exists() {
            fs::remove_file(path).map_err(|e| CacheError::Io(e.to_string()))?;
        }
        Ok(())
    }
}

pub fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_round_trip() {
        let tmp = std::env::temp_dir().join(format!("neosh-cache-test-{}", Uuid::new_v4()));
        let cache = SessionCache::new(tmp.clone());
        let sid = Uuid::new_v4();

        let entry = SessionCacheEntry {
            session_id: sid,
            ssh_target: "user@example.com".into(),
            resume_token: "r".into(),
            resume_token_expires_at: now_epoch_seconds() + 3600,
            quic_addr: "127.0.0.1:30001".into(),
            cert_fingerprint:
                "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            updated_at: now_epoch_seconds(),
        };

        cache.put(&entry).unwrap();
        let loaded = cache.get(sid).unwrap();
        assert_eq!(loaded, entry);
        cache.delete(sid).unwrap();
        fs::remove_dir_all(tmp).ok();
    }
}
