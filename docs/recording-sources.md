# Experimental recording sources

`recording_source` prepares vendor-neutral access to existing camera recordings,
separate from live RTSP ingestion and local/remote retention. It is not wired into
the gateway, recorder, upload queue or API. No source connects to a device yet.

`RecordingSource` exposes unknown/supported/unsupported capabilities, bounded UTC
range queries (up to 31 days and 256 results per page) with opaque pagination, and a cancellable `RecordingDownload`.
Ranges use generated CameraId, validated half-open UTC times, recording kind and
an optional redacted vendor locator. Date-based vendor queries must establish the
device timezone and clock correction before returning UTC ranges. Unknown recording
kinds stay unknown. Adapters must cap pages, validate cursor scope, detect repeating
cursors and bound the total query loop; the interfaces do not implement pagination.

The download boundary provides bounded media chunks, container/codec observations,
sticky cancellation and idle timeouts. Timeout/protocol errors remain errors on
later reads; only confirmed completion becomes EOF. Session implementations must
release sockets/tasks on drop and be cancellation-safe. The caller must also impose
an overall operation deadline: an idle timeout does not bound a steadily streaming
peer. Media bytes, vendor locators, credentials and tokens have redacted Debug;
none have automatic serialization. No network logging is implemented.

`TapoRecordingSource` is an inert skeleton: capabilities remain Unknown and list/open
return NotImplemented (or validation/cancellation errors). Its recording credentials
are distinct from ONVIF/RTSP credentials. Internal protocol types cover authentication
state, dates, UTC/day requests, local session construction, capability flags and
Download/Playback modes. Fallback permits only explicit UnsupportedMethod, once;
no numeric device error mapping is asserted. No C120-specific branch exists.

`RecordingImport` is a future demux/remux boundary, with explicit preserve-without-
transcoding or omit-audio policy. No importer is implemented. H.264 remux requires
verified sample framing, parameter sets, timestamps and keyframes. The existing
MP4 writer is video-only. G.711 audio cannot simply be passed to it; preserve an
appropriate source container or explicitly omit audio until support is validated.
Never silently transcode audio or equate a requested interval with actual coverage.

## Future deduplication contract

`ImportIdentity` carries a generated source-instance ID, CameraId and UTC range,
optional redacted vendor ID and optional content fingerprint. `ImportedRecording`
links the source to one or more generated RecordingIds and observed media bounds.
These types are preparation only: there is no persisted ledger or sync scheduler.

A future private ledger must separate candidates from completed imports. Camera
and source namespace plus normalized range and optional vendor ID identify a
candidate; source IDs/filenames alone do not prove immutable content. Reused IDs,
extended active clips and overlapping event ranges require reconciliation. Hash
validated content with an established hash implementation when available; do not
use a credential, network address, account or hardware ID in a fingerprint. Keep
fingerprints local and redacted. Only finalize the ledger after valid media is
atomically published, and recover publication/ledger crashes idempotently. A partial
or failed download must not suppress a later retry. Local or remote retention must
not cause an already imported source range to be downloaded again automatically;
keep minimal terminal evidence independently of replica file presence.

## Reference and limits

Protocol research used [pytapo at the pinned revision](https://github.com/JurajNyiri/pytapo/tree/a2f0fbd1fa4f4fc79e9fb5df4e9893ca55a3ba8c),
licensed under MIT. These Rust contracts are independently written; no upstream
implementation or fixtures are incorporated. Upstream default MP4 conversion
copies video but transcodes audio, so it is not adopted as the import implementation.
Detailed research, licensing, firmware evidence and the future five-minute device
probe plan are kept in ignored internal documentation. No real-device validation
has occurred for this feature. Synthetic tests validate contracts, not wire-protocol
compatibility, authentication, decryption, TS decoding or successful downloads.

## Offline protocol and import checks

Experimental helpers now implement bounded multipart framing, single-session
routing, allowlist date/range decoding, pagination budgets and an import checkpoint
journal. They are not connected to `TapoRecordingSource` or the production gateway.
There is still no HTTP client, authentication exchange, decryptor or native TS demuxer.

Framing caps headers at 8 KiB and media parts at 1 MiB. Duplicate headers, invalid
lengths, truncated frames, unknown sessions, sequence gaps and ambiguous completion
fail with fixed errors. Encrypted parts are explicitly rejected pending a validated
decryptor. The strict observed subset requires part CRLF delimiters and tagged
completion; real firmware may require a separately reviewed compatibility policy.
JSON routing and authoritative timestamp fields reject duplicate definitions.
Only verified UTC correction may be supplied to query decoding; recording kind
remains Unknown. Coincident time ranges are preserved because equal times do not
prove equal content. Empty results are supported. Cursor budgets bound processed
pages/results and detect repeats; a caller must enforce them and its deadline.

Downloads now enforce a monotonic total deadline (default 300 seconds, configurable
shorter) in addition to the idle timeout, including explicit cleanup. A steadily
sending peer cannot extend it. Authentication and other operations still need their
own future deadline-aware transport. EOF remains distinct from confirmed completion.

`ImportJournal` is an isolated prototype in a dedicated private SQLite database.
A SHA-256 content key includes generated source namespace, CameraId and exact UTC
bounds. Reserving it repeatedly returns the same generated operation ID; pending
work is never treated as complete. Completion stores generated RecordingIds and
rejects conflicting results. The caller must serialize workers, set private database
permissions, verify media and atomically publish a manifest carrying the operation ID
before marking completion. This is not an atomic filesystem/database transaction or
a production publisher. A future reconciler must examine the manifest after a crash.
The key is available after content hashing: avoiding the initial/repeated network
download still needs a verified source-candidate index and freshness policy. Retain
terminal import evidence independently of local/remote replica expiry.

Synthetic tests exercise byte splits, malformed limits, arbitrary inputs, wrong
sessions/sequences, ambiguous or repeated fields, abrupt EOF, cancellation, steady
traffic deadlines, empty/invalid queries, cursor repetition and journal restarts.
Journal restart tests use synthetic publication receipts; they do not claim a
complete media import transaction or physical power-loss qualification.

A separate bounded offline media check generated 90 seconds of 160x96 H.264 at
10 fps, without B-frames, with a one-second GOP. FFmpeg copied TS video into an
intermediate MP4; Rust read samples into FerrisSight's UTC MP4 writer. Starting at
UTC second 45 produced 15/60/15-second files with 150/600/150 frames. All three
independently decoded, and all 900 decoded frame hashes matched the source in order.
No partial files remained; inventory scan and restart both found exactly three jobs.
This verifies that synthetic video path, not a native Rust TS parser or Tapo download.
Audio was explicitly omitted. B-frame and other codec/container paths remain untested.

Three separate loopback-only lifecycle runs each completed 1,100 socket/parser
cycles, with 100 warm-up cycles. At sampled checkpoints, FD count stayed at four;
RSS was stable within each run (3,856 / 3,776 / 3,756 KiB respectively). These are
bounded diagnostic observations, not proof of leak freedom or endurance.
Standalone OpenSSL AES-128-CBC encryption/decryption passed the four-block
[NIST SP 800-38A F.2.1 vector](https://nvlpubs.nist.gov/nistpubs/legacy/sp/nistspecialpublication800-38a.pdf),
and a SHA-256 standard vector passed. These checks validate reference primitives,
not FerrisSight cryptography, padding, key derivation or authentication.
