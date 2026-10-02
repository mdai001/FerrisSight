use chrono::DateTime;
use ferrissight::{
    core::CameraId,
    recording_source::{journal::*, UtcRange},
    storage::RecordingId,
};
use rusqlite::Connection;
use uuid::Uuid;
fn key() -> ImportKey {
    ImportKey::from_content(
        Uuid::nil(),
        CameraId::generate(),
        UtcRange::new(
            DateTime::from_timestamp(0, 0).unwrap(),
            DateTime::from_timestamp(60, 0).unwrap(),
        )
        .unwrap(),
        [7; 32],
    )
}
#[test]
fn pending_is_resumable_and_completed_dedup_survives_local_expiry() {
    let mut journal = ImportJournal::new(Connection::open_in_memory().unwrap()).unwrap();
    let key = key();
    let first = journal.reserve(&key).unwrap();
    assert!(first.recordings.is_none());
    assert_eq!(
        journal.reserve(&key).unwrap().operation_id,
        first.operation_id
    );
    let ids = [RecordingId::generate()];
    journal.complete(&key, first.operation_id, &ids).unwrap();
    journal.complete(&key, first.operation_id, &ids).unwrap();
    assert_eq!(journal.reserve(&key).unwrap().recordings.unwrap(), ids);
    assert!(journal
        .complete(&key, first.operation_id, &[RecordingId::generate()])
        .is_err());
    assert!(journal.complete(&key, Uuid::new_v4(), &ids).is_err());
    assert!(journal.complete(&key, first.operation_id, &[]).is_err());
}
#[test]
fn publication_checkpoint_recovers_both_crash_windows() {
    let root = std::env::temp_dir().join(format!("ferrissight-import-test-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("journal.sqlite3");
    let key = key();
    let journal = ImportJournal::new(Connection::open(&path).unwrap()).unwrap();
    let first = journal.reserve(&key).unwrap();
    drop(journal);
    let mut journal = ImportJournal::new(Connection::open(&path).unwrap()).unwrap();
    assert_eq!(
        journal.reserve(&key).unwrap().operation_id,
        first.operation_id
    );
    // Simulated validated publisher receipt, not an actual MP4 or completed import.
    let ids = [RecordingId::generate()];
    std::fs::write(
        root.join("receipt.partial"),
        serde_json::to_vec(&ids).unwrap(),
    )
    .unwrap();
    std::fs::rename(root.join("receipt.partial"), root.join("receipt.json")).unwrap();
    drop(journal);
    journal = ImportJournal::new(Connection::open(&path).unwrap()).unwrap();
    let checkpoint = journal.reserve(&key).unwrap();
    assert!(checkpoint.recordings.is_none());
    let recovered: Vec<RecordingId> =
        serde_json::from_slice(&std::fs::read(root.join("receipt.json")).unwrap()).unwrap();
    journal
        .complete(&key, checkpoint.operation_id, &recovered)
        .unwrap();
    drop(journal);
    // Retaining the terminal journal suppresses re-import even when replicas expire.
    std::fs::remove_file(root.join("receipt.json")).unwrap();
    let journal = ImportJournal::new(Connection::open(&path).unwrap()).unwrap();
    assert_eq!(journal.reserve(&key).unwrap().recordings.unwrap(), ids);
    drop(journal);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn fingerprints_are_private_and_namespaced() {
    let camera = CameraId::generate();
    let utc = UtcRange::new(
        DateTime::from_timestamp(0, 0).unwrap(),
        DateTime::from_timestamp(60, 0).unwrap(),
    )
    .unwrap();
    let source = Uuid::new_v4();
    let first = ImportKey::from_content(source, camera, utc, [1; 32]);
    assert_eq!(format!("{first:?}"), "<redacted>");
    assert!(first == ImportKey::from_content(source, camera, utc, [1; 32]));
    assert!(first != ImportKey::from_content(source, camera, utc, [2; 32]));
    assert!(first != ImportKey::from_content(Uuid::new_v4(), camera, utc, [1; 32]));
    assert!(first != ImportKey::from_content(source, CameraId::generate(), utc, [1; 32]));
}
