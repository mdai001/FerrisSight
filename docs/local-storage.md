# Local retention and durable upload queue

Phase 2A adds an independent storage coordinator to `ferrissight serve`. It scans
finalized UTC recordings at startup and approximately every five seconds, indexes
missed files and applies retention. The RTSP recorder never calls SQLite, waits
for an uploader or waits for maintenance. Run the gateway and bounded recorder
against the same recordings root; the standalone recording example continues to
work without the gateway, and a later scan repairs its missed enqueue events.

## Configuration and safe status

Defaults are 72 hours, 100 GiB (`107374182400` bytes), protection of unuploaded
recordings enabled, and uploads disabled. The root defaults to `recordings`.

```sh
cargo run -- serve --retention-hours 72 --max-storage-bytes 107374182400
```

`--data-dir` selects the local recording root. `--allow-unuploaded-eviction`
explicitly permits dropping unuploaded files under capacity pressure. It never
permits deletion of active uploads or partial files. `--upload-enabled` reserves
future uploader control; Phase 2A performs no cloud requests, even when enabled.
The serializable `StorageConfig` and validated `LocalStore::set_config` provide
an in-process control boundary for a future authenticated mobile API. CLI settings
are supplied on startup; there is no writable unauthenticated configuration API
or automatic TOML loading in this phase.

`GET /api/v1/storage` exposes only aggregate availability/degradation, media bytes,
configured limit, pressure, pending/uploading/retry/failed counts, oldest unfinished
upload age, missing recordings, rejected files, cleanup failures, evicted unuploaded
recordings and the upload-enabled setting. Unknown/stale information is marked
unavailable when maintenance cannot complete. No path, CameraId, remote locator,
network value or raw error is returned. Existing health/camera responses remain safe.

## Queue and identity

SQLite is stored inside the root's ignored `.storage` directory. One coordinator
owns an OS file lock; a second instance cannot reset live work. SQLite uses WAL,
`FULL` synchronization, short transactions and a 250 ms busy timeout. Local
filesystems with working SQLite locks and sync semantics are required; network
shares are not supported. See the official [SQLite WAL durability documentation](https://www.sqlite.org/wal.html).
New database/lock/media files request owner-only Unix permissions; other platforms
require suitable local filesystem ACLs.

The `recordings` table contains:

| Field | Purpose |
| --- | --- |
| `segment_id` | Stable application UUID; primary key and future upload idempotency key |
| `camera_id` | FerrisSight-generated CameraId |
| `relative_path` | Validated generated UTC path relative to the recording root |
| `start_ms`, `end_ms` | Authoritative sidecar UTC media estimates |
| `size_bytes` | Finalized MP4 size |
| `state` | `pending`, `uploading`, `retry_wait`, `uploaded`, `failed` |
| `attempt_count`, `next_retry_ms` | Persistent attempts and eligibility time |
| `remote_object_id` | Optional opaque provider locator; never exposed in status/debug |
| `local_state` | `present`, `deleting`, `deleted`, `missing` |

New sidecars include a random `segmentId`; MP4 contents remain immutable. Legacy
sidecars receive a deterministic UUIDv5 from their generated relative path, media
timestamps and size. UUIDv5 is an identity fallback, not an integrity checksum.
Re-enqueue is idempotent, and an active-path uniqueness constraint prevents two
live jobs for one file. Tombstones preserve the completed deletion identity when
a minute filename is reused by a new segment. Matching restored files become
managed again without creating another logical job.

```text
pending → uploading → uploaded
              ├────→ retry_wait → uploading
              └────→ failed
```

Claims use an immediate transaction. Completion/retry/failure must match both the
uploading state and attempt number, so stale completions cannot overwrite a newer
attempt. Restart converts interrupted uploading jobs to retry-wait. A future
backend must use the segment UUID for remote idempotency: a local database cannot
atomically commit a remote upload and its local acknowledgment. This is durable
at-least-once work, not a promise of exactly-once cloud effects.

The minimal `UploadBackend` receives safe job metadata and an already-open file.
It returns an opaque remote locator or a fixed retryable/permanent error. No Google
schema, OAuth implementation, provider SDK or uploader executor is included.

## Retention and deletion

Age uses authoritative `end_ms`, not the directory date or filesystem modification
time. Uploaded files beyond the age limit are eligible. Unuploaded files remain
protected from age expiry even when explicit capacity eviction is enabled.
At or above the configured byte limit, delete the oldest uploaded files first,
ordered by end time. Only the explicit eviction option allows pending/retry/failed
files to be deleted after uploaded candidates. Uploading files are always protected.

The limit is an asynchronous cleanup threshold, not a filesystem quota. If protected
files, active media or unclassified artifacts prevent reclamation, pressure stays
visible and recording is not stopped by maintenance. Physical disk exhaustion can
still make recording writes fail. The byte estimate includes ordinary recording-root
files such as metadata and partials, excludes the control database, and does not
traverse symlinks or unexpectedly deep directory trees. No stale partial is removed
without a future reliable ownership/staleness mechanism.

Deletion commits a `deleting` intent, removes the completed MP4 marker, removes its
JSON sidecar, syncs the directory on Unix, prunes empty UTC calendar directories,
and commits the deleted tombstone. The root and camera namespace are preserved.
Each step can be retried after a crash. Recovery checks segment identity before
unlinking: an old deletion intent cannot remove a new segment reusing the filename.
Reclaimed bytes are measured even when later cleanup fails, avoiding extra eviction.

The recorder recreates an empty UTC directory if retention removed it before its
first keyframe. A directory containing active partials cannot be pruned. Existing
media is never modified to record queue state. A malformed header, sidecar, changed
identity, unsafe path or symlink is rejected conservatively and is not reclaimed
as previously uploaded content.

## Recovery and validation

Startup finishes deletion intents, reconciles valid MP4/sidecar pairs, repairs
missing enqueue events and reports externally missing recordings. It ignores
partials and orphan sidecars as upload sources. Database failure does not delete
or modify finalized media. The background task retries opening failed storage and
marks its snapshot unavailable; recording has no dependency on its success.

Synthetic tests cover persistence, idempotence, legacy IDs, lock contention,
interrupted-upload recovery and stale attempt fencing; age and capacity ordering;
protected/unprotected/uploading cases; unsafe paths/partials; every deletion crash
point; same-name replacement after a crash; directory recreation during recording;
restored files; database locks/corruption; failed tombstone commits; safe API status;
and recovery after forcibly killing a separate process before SQLite cleanup.
Validation passed: formatting, strict workspace Clippy, 47 tests and the workspace
build. Two gateway startup/shutdown cycles on the workspace filesystem also passed,
including safe status responses and a SQLite integrity check. The package listing
contained 36 entries and no private storage or internal artifacts. No real-camera
recordings were used for this phase. These bounded tests do not substitute for
physical power-loss qualification.

## Future independent remote retention

This section specifies a future contract, not implemented cloud behavior. Local
and remote copies are independently governed replicas of one logical segment.
For example, local retention can be 72 hours while a remote backend retains 30 days.
Local expiry must not schedule remote deletion; remote expiry must not remove a
still-retained local copy. Explicit user-requested deletion of both replicas would
be a separate operation with its own authorization.

Both policies use authoritative capture `end_ms` as their age anchor (the last
media boundary of the segment), preserving `start_ms` for timeline queries:
`expires_at_ms = end_ms + retention_duration_ms`. Provider creation/upload times,
filenames and folder names never determine age. Delayed uploads retain the original
capture time. A backlog item already expired under an enabled remote policy is
skipped for that backend without deleting its local copy. Do not upload and
immediately delete it in a recurring loop. Unknown/invalid capture time fails
closed for deletion and is reported as a safe status category. Evaluate expiry only
with a trusted UTC clock; suspend destructive work after an unexplained wall-clock
jump or inconsistent capture time until reconciled. Retry delays use monotonic
time while the process runs. Timezone/DST changes do not change UTC age.

The current v1 index already retains segment ID, CameraId, capture start/end,
size, upload result and remote object ID after local deletion; only `local_state`
changes. A regression test verifies this across restart after a delayed upload.
These rows are the migration source, not a complete remote lifecycle schema.
Before implementing cloud retention, migrate to separate remote replica records:

| Future field | Purpose |
| --- | --- |
| `segment_id`, `backend_instance_id` | Unique replica key; backend instance is a generated local ID, not an account identifier |
| `remote_object_id` | Stable opaque object ID returned by the backend |
| ownership provenance | Durable proof of FerrisSight creation or explicit adoption, bound to backend scope |
| remote state | `present`, `delete_pending`, `deleting`, `retry_wait`, `deleted`, `failed` |
| deletion attempt, next retry, revision | Durable scheduling and fencing against stale completions |
| policy revision, deletion reason | Recheck policy changes and distinguish retention from explicit user deletion |

The logical segment retains authoritative capture timestamps independently of
local file presence. Upload history is not remote presence: successful upload
remains historical fact after remote expiry. Keep a terminal remote tombstone so
reconciliation does not re-upload deliberately expired media or repeatedly delete
it. Index compaction must preserve ownership/identity and must not discard records
while any replica or upload/deletion work is still live. Restored v1 databases
cannot safely infer a backend scope for old locators; leave remote deletion disabled
until their association and ownership have been verified.

A future backend-neutral operation is conceptually
`async fn delete(&self, remote_object_id: &RemoteObjectId) -> Result<(), DeleteError>`.
The backend instance binds the destination scope; the retention layer checks the
managed replica record before calling it. Never infer ownership from matching
names, scan unrelated user files or delete by a name search. Lost local ownership
records must not trigger broad cloud discovery/deletion. Any recovery enumeration
must be limited to an explicitly managed scope and verify ownership evidence.

Remote deletion uses a durable, bounded asynchronous worker with its own retry
budget, independent of recording and upload work. Transient errors use backoff;
authentication/permission errors pause work for that backend without claiming the
object was deleted. A verified absent object is idempotent success, but an ambiguous
not-found response that could mean loss of access requires reconciliation. Commit
intent before calling the provider, then persist completion. A crash after provider
success may require one more idempotent attempt; exactly-once remote effects are
not assumed. Remote recording objects are immutable: never overwrite or reassign
a locator while deletion is pending or uncertain. A replacement upload needs a
new remote identity. Providers whose locators can be reused require a conditional
version/generation delete extension; disable automatic deletion when safe identity
cannot be guaranteed. A local revision fence alone cannot cancel a remote request.
Recheck current policy and replica revision before issuing deletion;
a policy change cannot undo a provider deletion already in flight.

Track FerrisSight-created UTC folders by returned IDs and creation provenance.
Prune only empty managed folders after child jobs finish, coordinate with uploads,
and use a provider-safe non-recursive empty-folder operation where available. If
emptiness cannot be guaranteed against concurrent writers, skip folder cleanup.
Never recursively remove the user-selected backup root or user-created folders.

Future authenticated settings should separate local keep duration and maximum
bytes from each backend's backup-enabled flag and optional remote keep duration.
A null remote duration disables automatic remote expiry; a positive duration enables
it explicitly. Enabling backup alone must not silently enable remote deletion.
For example: local keep 72 hours / maximum 100 GiB, Drive backup enabled / remote
keep 30 days. Validate positive durations and overflow-safe arithmetic, and persist
policy revisions. These are design fields, not current CLI/TOML options; no mobile
UI is included. Decommissioning a backend requires an explicit choice to finish
cleanup or abandon management without deleting its remote objects. Exhausted or
permanent failures remain visible and stop automatic retries; do not label them
deleted or discard ownership records merely to compact the database. Terminal
compaction and minimal tombstone retention need a separate bounded storage design.
Remote IDs, scope handles and provider errors remain redacted;
status APIs expose only safe aggregate work counts and failure categories.

Future validation must cover delayed uploads, both directions of replica lifecycle
independence, ownership rejection, two backend scopes, interrupted deletion/restart,
stale completions, retry backoff, ambiguous missing objects, policy changes,
no re-upload after expiry, remote locator reuse, clock jumps, backend decommissioning,
and empty-folder/upload races.

## Phase 2B decisions

Google Drive remains a future backend. Minimum OAuth scopes, OS/protected token
storage, remote idempotency/reconciliation, resumable uploads, retry policy, remote
object deletion and authenticated runtime configuration remain future work. Optional
SHA-256 hashing should run after finalization in background work, never on the RTSP
receive path. Full-file bit-rot verification, legacy-flat-file migration, database
backup/repair tooling, tombstone compaction, stale-partial ownership and hostile
local-writer race hardening are also deferred. Storage roots are trusted local
application directories, not writable by untrusted processes.
