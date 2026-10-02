use ferrissight::{
    core::CameraId,
    storage::{
        local::{LocalStore, RemoteObjectId, StorageConfig, StorageError},
        mp4::{H264Config, Mp4Segments, UtcMinuteTarget, VideoSample},
        service::StorageService,
    },
};
use std::{fs, path::PathBuf, time::Duration};
use uuid::Uuid;
struct Fixture {
    root: PathBuf,
    camera: CameraId,
}
impl Fixture {
    fn new() -> Self {
        Self {
            root: std::env::temp_dir().join(format!("ferrissight-storage-test-{}", Uuid::new_v4())),
            camera: CameraId::generate(),
        }
    }
    fn segment(&self, start: i64) -> String {
        let config = H264Config {
            width: 16,
            height: 16,
            sps: vec![0x67, 0x42, 0, 0x1e, 0xf4, 0x4b, 0x20],
            pps: vec![0x68, 0xce, 0x3c, 0x80],
            timescale: 1000,
        };
        let minute = start.div_euclid(60_000) * 60_000;
        let mut sink = Mp4Segments::new_utc_minute(
            &self.root,
            config,
            250,
            UtcMinuteTarget {
                camera_id: self.camera,
                window_start_unix_ms: minute,
                anchor_unix_ms: start,
                anchor_rtp_ticks: 0,
            },
        )
        .unwrap();
        for i in 0..4 {
            sink.push(VideoSample {
                timestamp: i * 250,
                keyframe: i == 0,
                data: vec![0, 0, 0, 2, if i == 0 { 0x65 } else { 0x41 }, 0x80],
            })
            .unwrap();
        }
        let (segments, _) = sink.finish().unwrap();
        let t = chrono::DateTime::from_timestamp_millis(minute).unwrap();
        format!(
            "camera-{}/{}/{}_{:03}.mp4",
            self.camera,
            t.format("%Y/%m/%d/%H"),
            t.format("%M"),
            segments[0].sequence
        )
    }
    fn config() -> StorageConfig {
        StorageConfig {
            retention_hours: 1,
            upload_enabled: true,
            ..Default::default()
        }
    }
    fn store(&self) -> LocalStore {
        LocalStore::open(&self.root, Self::config(), 0).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn upload_next(store: &mut LocalStore) {
    let job = store.claim_next(0).unwrap().unwrap();
    assert!(store.open_upload(&job).is_ok());
    store
        .uploaded(
            &job,
            RemoteObjectId::new("synthetic-object".into()).unwrap(),
        )
        .unwrap();
}
#[test]
fn finalized_enqueue_is_idempotent_and_survives_restart() {
    let f = Fixture::new();
    let path = f.segment(60_000);
    let mut store = f.store();
    let id = store.enqueue(&path).unwrap();
    assert_eq!(id, store.enqueue(&path).unwrap());
    assert_eq!(store.maintain(120_000).unwrap().pending_upload_count, 1);
    assert!(matches!(
        LocalStore::open(&f.root, Fixture::config(), 0),
        Err(StorageError::Busy)
    ));
    drop(store);
    let mut store = f.store();
    assert_eq!(store.enqueue(&path).unwrap(), id);
    let status = store.maintain(120_000).unwrap();
    assert_eq!(status.pending_upload_count, 1);
    assert_eq!(status.oldest_pending_age_seconds, Some(60));
}
#[test]
fn uploading_recovers_and_stale_completion_is_fenced() {
    let f = Fixture::new();
    let path = f.segment(0);
    let mut store = f.store();
    store.enqueue(&path).unwrap();
    let old = store.claim_next(0).unwrap().unwrap();
    drop(store);
    let mut store = f.store();
    assert_eq!(store.maintain(0).unwrap().retry_count, 1);
    let next = store.claim_next(0).unwrap().unwrap();
    assert_eq!(next.attempt, old.attempt + 1);
    assert!(matches!(
        store.uploaded(&old, RemoteObjectId::new("stale".into()).unwrap()),
        Err(StorageError::Transition)
    ));
    store.retry(&next, 1000).unwrap();
    assert!(store.claim_next(999).unwrap().is_none());
    let retry = store.claim_next(1000).unwrap().unwrap();
    store.fail(&retry).unwrap();
    assert_eq!(store.maintain(1000).unwrap().failed_count, 1);
}
#[test]
fn retention_uses_authoritative_end_time_and_protects_unuploaded() {
    let f = Fixture::new();
    let uploaded = f.segment(0);
    let pending = f.segment(60_000);
    let mut store = f.store();
    store.enqueue(&uploaded).unwrap();
    upload_next(&mut store);
    store.enqueue(&pending).unwrap();
    let status = store.maintain(7_200_000).unwrap();
    assert!(!f.root.join(&uploaded).exists());
    assert!(f.root.join(&pending).exists());
    assert_eq!(status.pending_upload_count, 1);
    // Directory date is old, but authoritative media end is fresh: must retain it.
    let fresh = f.segment(120_000);
    let metadata = f.root.join(&fresh).with_extension("json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&metadata).unwrap()).unwrap();
    value["utcTiming"]["first_frame_unix_ms"] = 7_199_000.into();
    value["utcTiming"]["end_frame_unix_ms"] = 7_200_000.into();
    fs::write(metadata, serde_json::to_vec(&value).unwrap()).unwrap();
    store.enqueue(&fresh).unwrap();
    upload_next(&mut store);
    upload_next(&mut store);
    store.maintain(7_200_000).unwrap();
    assert!(f.root.join(fresh).exists());
}
#[test]
fn capacity_deletes_oldest_uploaded_first_and_reports_protected_pressure() {
    let f = Fixture::new();
    let pending = f.segment(0);
    let old = f.segment(60_000);
    let newer = f.segment(120_000);
    let mut store = f.store();
    store.enqueue(&old).unwrap();
    upload_next(&mut store);
    store.enqueue(&newer).unwrap();
    upload_next(&mut store);
    store.enqueue(&pending).unwrap();
    let used = store.maintain(180_000).unwrap().used_bytes;
    store
        .set_config(StorageConfig {
            max_storage_bytes: used - 1,
            ..Fixture::config()
        })
        .unwrap();
    store.maintain(180_000).unwrap();
    assert!(!f.root.join(&old).exists());
    assert!(f.root.join(&newer).exists());
    assert!(f.root.join(&pending).exists());
    store
        .set_config(StorageConfig {
            max_storage_bytes: 1,
            ..Fixture::config()
        })
        .unwrap();
    let status = store.maintain(180_000).unwrap();
    assert!(status.storage_pressure);
    assert!(!f.root.join(newer).exists());
    assert!(f.root.join(pending).exists());
}
#[test]
fn explicit_eviction_preserves_uploading_and_all_partials() {
    let f = Fixture::new();
    let uploading = f.segment(0);
    let pending = f.segment(60_000);
    let mut store = f.store();
    store.enqueue(&uploading).unwrap();
    let job = store.claim_next(0).unwrap().unwrap();
    store.enqueue(&pending).unwrap();
    let partial = f.root.join(&pending).with_extension("mp4.partial");
    fs::write(&partial, b"synthetic active bytes").unwrap();
    store
        .set_config(StorageConfig {
            protect_unuploaded: false,
            max_storage_bytes: 1,
            ..Fixture::config()
        })
        .unwrap();
    let status = store.maintain(7_200_000).unwrap();
    assert!(!f.root.join(&pending).exists());
    assert!(f.root.join(&uploading).exists());
    assert!(partial.exists());
    assert!(store.open_upload(&job).is_ok());
    assert_eq!(status.deleted_unuploaded_count, 1);
    assert!(status.storage_pressure);
    assert!(store
        .enqueue(&pending.replace(".mp4", ".mp4.partial"))
        .is_err());
}
#[test]
fn normal_age_expiry_never_evicts_pending_even_when_capacity_eviction_enabled() {
    let f = Fixture::new();
    let path = f.segment(0);
    let mut store = f.store();
    store
        .set_config(StorageConfig {
            protect_unuploaded: false,
            ..Fixture::config()
        })
        .unwrap();
    store.maintain(7_200_000).unwrap();
    assert!(f.root.join(path).exists());
}
#[test]
fn deletion_intents_recover_each_filesystem_crash_point_and_clean_empty_directories() {
    for removed in 0..=2 {
        let f = Fixture::new();
        let path = f.segment(0);
        let mut store = f.store();
        let id = store.enqueue(&path).unwrap();
        upload_next(&mut store);
        drop(store);
        let db = rusqlite::Connection::open(f.root.join(".storage/queue.sqlite3")).unwrap();
        db.execute(
            "UPDATE recordings SET local_state='deleting' WHERE segment_id=?1",
            [id.to_string()],
        )
        .unwrap();
        drop(db);
        let full = f.root.join(&path);
        if removed >= 1 {
            fs::remove_file(&full).unwrap();
        }
        if removed >= 2 {
            fs::remove_file(full.with_extension("json")).unwrap();
        }
        let mut store = f.store();
        store.maintain(7_200_000).unwrap();
        assert!(!full.exists());
        assert!(!full.with_extension("json").exists());
        assert!(!full.parent().unwrap().exists());
        assert!(f.root.join(".storage").exists());
    }
}
#[test]
fn missed_finalize_event_and_legacy_metadata_reconcile_after_restart() {
    let f = Fixture::new();
    let path = f.segment(0);
    let meta = f.root.join(&path).with_extension("json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&meta).unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("segmentId");
    fs::write(meta, serde_json::to_vec(&value).unwrap()).unwrap();
    let mut store = f.store();
    assert_eq!(store.maintain(0).unwrap().pending_upload_count, 1);
    let first = store.enqueue(&path).unwrap();
    drop(store);
    let mut store = f.store();
    assert_eq!(store.enqueue(&path).unwrap(), first);
    assert_eq!(store.maintain(0).unwrap().pending_upload_count, 1);
}
#[test]
fn corrupt_database_does_not_affect_recording_or_finalized_bytes() {
    let f = Fixture::new();
    let first = f.segment(0);
    let original = fs::read(f.root.join(&first)).unwrap();
    fs::create_dir_all(f.root.join(".storage")).unwrap();
    fs::write(
        f.root.join(".storage/queue.sqlite3"),
        b"synthetic damaged database",
    )
    .unwrap();
    assert!(LocalStore::open(&f.root, Fixture::config(), 0).is_err());
    let next = f.segment(60_000);
    assert!(f.root.join(next).exists());
    assert_eq!(fs::read(f.root.join(first)).unwrap(), original);
}
#[test]
fn database_lock_failure_keeps_files_and_recovers_later() {
    let f = Fixture::new();
    let mut store = f.store();
    let path = f.segment(0);
    let db = rusqlite::Connection::open(f.root.join(".storage/queue.sqlite3")).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(store.enqueue(&path).is_err());
    assert!(f.root.join(&path).exists());
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(store.maintain(0).unwrap().pending_upload_count, 1);
}
#[cfg(unix)]
#[test]
fn symlinks_and_unsafe_paths_fail_closed_without_blocking_new_recordings() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let path = f.segment(0);
    let mut store = f.store();
    store.enqueue(&path).unwrap();
    upload_next(&mut store);
    let meta = f.root.join(&path).with_extension("json");
    let external = Fixture::new();
    fs::create_dir_all(&external.root).unwrap();
    let sentinel = external.root.join("sentinel");
    fs::write(&sentinel, b"preserve").unwrap();
    fs::remove_file(&meta).unwrap();
    symlink(&sentinel, &meta).unwrap();
    let result = store.maintain(7_200_000);
    assert!(result.is_err() || result.unwrap().degraded);
    assert_eq!(fs::read(&sentinel).unwrap(), b"preserve");
    assert!(f.root.join(&path).exists());
    assert!(store.enqueue("../outside.mp4").is_err());
    assert!(store.enqueue("/synthetic/file.mp4").is_err());
    let next = f.segment(60_000);
    assert!(f.root.join(next).exists());
}
#[tokio::test]
async fn background_database_failure_keeps_api_safe_and_recorder_independent() {
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;
    let f = Fixture::new();
    fs::create_dir_all(&f.root).unwrap();
    fs::write(f.root.join(".storage"), b"synthetic obstruction").unwrap();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let (service, task) = StorageService::start(f.root.clone(), Fixture::config(), rx);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let path = f.segment(0);
    assert!(f.root.join(path).exists());
    let response = ferrissight::server::app_with_storage(service)
        .oneshot(
            Request::builder()
                .uri("/api/v1/storage")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["available"], false);
    assert_eq!(
        value["configured_limit_bytes"],
        Fixture::config().max_storage_bytes
    );
    let text = String::from_utf8(body.to_vec()).unwrap();
    for forbidden in ["path", "camera_id", "host", "password", "token"] {
        assert!(!text.contains(forbidden));
    }
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}
#[test]
fn remote_locator_debug_is_redacted_and_upload_default_is_disabled() {
    let remote = RemoteObjectId::new("synthetic-object".into()).unwrap();
    assert_eq!(format!("{remote:?}"), "<redacted>");
    assert!(!StorageConfig::default().upload_enabled);
    let f = Fixture::new();
    let path = f.segment(0);
    let mut store = LocalStore::open(&f.root, StorageConfig::default(), 0).unwrap();
    store.enqueue(&path).unwrap();
    assert!(store.claim_next(0).unwrap().is_none());
}

#[test]
fn recorder_recreates_empty_utc_directory_removed_by_retention() {
    let f = Fixture::new();
    let old = f.segment(0);
    let mut store = f.store();
    store.enqueue(&old).unwrap();
    upload_next(&mut store);
    let config = H264Config {
        width: 16,
        height: 16,
        sps: vec![0x67, 0x42, 0, 0x1e, 0xf4, 0x4b, 0x20],
        pps: vec![0x68, 0xce, 0x3c, 0x80],
        timescale: 1000,
    };
    let mut sink = Mp4Segments::new_utc_minute(
        &f.root,
        config,
        250,
        UtcMinuteTarget {
            camera_id: f.camera,
            window_start_unix_ms: 60_000,
            anchor_unix_ms: 60_000,
            anchor_rtp_ticks: 0,
        },
    )
    .unwrap();
    store.maintain(7_200_000).unwrap();
    assert!(!f.root.join(&old).parent().unwrap().exists());
    sink.push(VideoSample {
        timestamp: 0,
        keyframe: true,
        data: vec![0, 0, 0, 1, 0x65],
    })
    .unwrap();
    assert_eq!(sink.finish().unwrap().0.len(), 1);
    assert_eq!(store.maintain(7_200_000).unwrap().pending_upload_count, 1);
}
#[test]
fn changed_finalized_content_is_not_evicted_as_previously_uploaded() {
    let f = Fixture::new();
    let path = f.segment(0);
    let mut store = f.store();
    store.enqueue(&path).unwrap();
    upload_next(&mut store);
    let meta = f.root.join(&path).with_extension("json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&meta).unwrap()).unwrap();
    value["segmentId"] = Uuid::new_v4().to_string().into();
    fs::write(meta, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(store.maintain(7_200_000).unwrap().degraded);
    assert!(f.root.join(path).exists());
}

#[test]
fn deletion_recovery_never_removes_new_segment_reusing_same_minute_filename() {
    let f = Fixture::new();
    let path = f.segment(0);
    let mut store = f.store();
    let id = store.enqueue(&path).unwrap();
    upload_next(&mut store);
    drop(store);
    let db = rusqlite::Connection::open(f.root.join(".storage/queue.sqlite3")).unwrap();
    db.execute(
        "UPDATE recordings SET local_state='deleting' WHERE segment_id=?1",
        [id.to_string()],
    )
    .unwrap();
    drop(db);
    fs::remove_file(f.root.join(&path)).unwrap();
    fs::remove_file(f.root.join(&path).with_extension("json")).unwrap();
    let new = f.segment(0);
    assert_eq!(new, path);
    let mut store = f.store();
    let status = store.maintain(7_200_000).unwrap();
    assert!(f.root.join(&new).exists());
    assert_eq!(status.pending_upload_count, 1);
    assert_ne!(store.enqueue(&new).unwrap(), id);
}

#[test]
fn restored_matching_recording_becomes_pending_without_duplicate_job() {
    let f = Fixture::new();
    let path = f.segment(0);
    let mut store = f.store();
    let id = store.enqueue(&path).unwrap();
    let bytes = fs::read(f.root.join(&path)).unwrap();
    fs::remove_file(f.root.join(&path)).unwrap();
    assert_eq!(store.maintain(0).unwrap().missing_recording_count, 1);
    fs::write(f.root.join(&path), bytes).unwrap();
    let status = store.maintain(0).unwrap();
    assert_eq!(status.missing_recording_count, 0);
    assert_eq!(status.pending_upload_count, 1);
    assert_eq!(store.enqueue(&path).unwrap(), id);
}
#[test]
fn failed_tombstone_commit_does_not_cause_extra_eviction_and_recovers() {
    let f = Fixture::new();
    let old = f.segment(0);
    let newer = f.segment(60_000);
    let mut store = f.store();
    store.enqueue(&old).unwrap();
    upload_next(&mut store);
    store.enqueue(&newer).unwrap();
    upload_next(&mut store);
    let used = store.maintain(120_000).unwrap().used_bytes;
    store
        .set_config(StorageConfig {
            max_storage_bytes: used - 1,
            ..Fixture::config()
        })
        .unwrap();
    let db = rusqlite::Connection::open(f.root.join(".storage/queue.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER synthetic_failure BEFORE UPDATE OF local_state ON recordings WHEN NEW.local_state='deleted' BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
    let status = store.maintain(120_000).unwrap();
    assert_eq!(status.cleanup_failures, 1);
    assert!(!status.storage_pressure);
    assert!(!f.root.join(old).exists());
    assert!(f.root.join(&newer).exists());
    db.execute_batch("DROP TRIGGER synthetic_failure").unwrap();
    drop(db);
    drop(store);
    let mut store = f.store();
    assert_eq!(store.maintain(120_000).unwrap().cleanup_failures, 0);
    assert!(f.root.join(newer).exists());
}
#[test]
fn eviction_orders_by_authoritative_end_for_overlapping_segments() {
    let f = Fixture::new();
    let longer = f.segment(0);
    let shorter = f.segment(60_000);
    let metadata = f.root.join(&longer).with_extension("json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&metadata).unwrap()).unwrap();
    value["utcTiming"]["end_frame_unix_ms"] = 120_000.into();
    fs::write(metadata, serde_json::to_vec(&value).unwrap()).unwrap();
    let mut store = f.store();
    store.enqueue(&longer).unwrap();
    upload_next(&mut store);
    store.enqueue(&shorter).unwrap();
    upload_next(&mut store);
    let used = store.maintain(180_000).unwrap().used_bytes;
    store
        .set_config(StorageConfig {
            max_storage_bytes: used - 1,
            ..Fixture::config()
        })
        .unwrap();
    store.maintain(180_000).unwrap();
    assert!(f.root.join(longer).exists());
    assert!(!f.root.join(shorter).exists());
}

#[test]
fn synthetic_crash_child() {
    let Some(root) = std::env::var_os("FERRISSIGHT_SYNTHETIC_CRASH_ROOT") else {
        return;
    };
    let mut store = LocalStore::open(&PathBuf::from(root), Fixture::config(), 0).unwrap();
    store.maintain(0).unwrap();
    assert!(store.claim_next(0).unwrap().is_some());
    use std::io::Write;
    println!("synthetic_ready");
    std::io::stdout().flush().unwrap();
    std::thread::sleep(Duration::from_secs(10));
    std::process::exit(1);
}
#[test]
fn killed_process_preserves_wal_queue_and_releases_owner_lock() {
    use std::io::BufRead;
    let f = Fixture::new();
    f.segment(0);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "synthetic_crash_child", "--nocapture"])
        .env("FERRISSIGHT_SYNTHETIC_CRASH_ROOT", &f.root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let output = std::io::BufReader::new(child.stdout.take().unwrap());
    let ready = output
        .lines()
        .take(10)
        .any(|line| line.is_ok_and(|s| s.contains("synthetic_ready")));
    if !ready {
        let _ = child.kill();
        let _ = child.wait();
        panic!("synthetic child did not become ready");
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let mut store = f.store();
    assert_eq!(store.maintain(0).unwrap().retry_count, 1);
    assert_eq!(store.claim_next(0).unwrap().unwrap().attempt, 2);
}

#[test]
fn malformed_completed_file_and_orphan_sidecar_are_not_queued() {
    let f = Fixture::new();
    let path = f.segment(0);
    fs::write(f.root.join(&path), b"synthetic incomplete media").unwrap();
    let mut store = f.store();
    let status = store.maintain(0).unwrap();
    assert_eq!(status.pending_upload_count, 0);
    assert_eq!(status.rejected_file_count, 1);
    fs::remove_file(f.root.join(path)).unwrap();
    assert_eq!(store.maintain(0).unwrap().pending_upload_count, 0);
}
#[tokio::test]
async fn background_reconciliation_populates_safe_storage_api() {
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;
    let f = Fixture::new();
    f.segment(0);
    let (stop, rx) = tokio::sync::watch::channel(false);
    let (service, task) = StorageService::start(f.root.clone(), Fixture::config(), rx);
    tokio::time::timeout(Duration::from_secs(3), async {
        while service.snapshot().pending_upload_count != 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let response = ferrissight::server::app_with_storage(service)
        .oneshot(
            Request::builder()
                .uri("/api/v1/storage")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["available"], true);
    assert_eq!(value["pending_upload_count"], 1);
    assert!(value["used_bytes"].as_u64().unwrap() > 0);
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}
