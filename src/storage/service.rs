//! Periodic filesystem reconciliation runs independently of RTSP and HTTP tasks.
use super::local::{LocalStore, StorageConfig, StorageError, StorageStatus};
use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

#[derive(Clone)]
pub struct StorageService {
    status: Arc<RwLock<StorageStatus>>,
}
impl StorageService {
    pub fn snapshot(&self) -> StorageStatus {
        self.status
            .read()
            .map(|s| s.clone())
            .unwrap_or(StorageStatus {
                degraded: true,
                ..Default::default()
            })
    }
    pub fn unavailable(config: &StorageConfig) -> Self {
        Self {
            status: Arc::new(RwLock::new(StorageStatus {
                configured_limit_bytes: config.max_storage_bytes,
                upload_enabled: config.upload_enabled,
                ..Default::default()
            })),
        }
    }
    /// Startup and transient database failures are retried; recording has no dependency on
    /// this task or its result. A single awaited blocking worker prevents task accumulation.
    pub fn start(
        root: PathBuf,
        config: StorageConfig,
        mut shutdown: watch::Receiver<bool>,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let service = Self::unavailable(&config);
        let state = service.clone();
        let task = tokio::spawn(async move {
            let mut store = None;
            loop {
                if *shutdown.borrow() {
                    break;
                }
                let root = root.clone();
                let config = config.clone();
                let work = tokio::task::spawn_blocking(move || {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .ok()
                        .and_then(|d| i64::try_from(d.as_millis()).ok())
                        .unwrap_or(0);
                    let mut current = match store {
                        Some(s) => s,
                        None => match LocalStore::open(&root, config, now) {
                            Ok(s) => s,
                            Err(_) => return (None, None),
                        },
                    };
                    match current.maintain(now) {
                        Ok(status) => (Some(current), Some(status)),
                        Err(StorageError::Database | StorageError::Io) => (None, None),
                        Err(_) => (Some(current), None),
                    }
                })
                .await;
                match work {
                    Ok((next, Some(status))) => {
                        store = next;
                        if let Ok(mut s) = state.status.write() {
                            *s = status;
                        }
                    }
                    Ok((next, None)) => {
                        store = next;
                        if let Ok(mut s) = state.status.write() {
                            s.available = false;
                            s.degraded = true;
                        }
                    }
                    Err(_) => {
                        store = None;
                        if let Ok(mut s) = state.status.write() {
                            s.available = false;
                            s.degraded = true;
                        }
                    }
                }
                tokio::select! {
                    _=shutdown.changed()=>{break;},
                    _=tokio::time::sleep(Duration::from_secs(5))=>{},
                }
            }
        });
        (service, task)
    }
}
