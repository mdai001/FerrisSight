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
