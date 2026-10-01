# Phase 1A RTSP probing

`ferrissight_media::probe::probe` accepts a structured `StreamEndpoint` and bounded
`ProbeOptions`. It uses Retina for RTSP authentication, RTP demultiplexing and
codec parameter parsing. Credentials are passed separately from a credential-free
URL. It supports no recording, transcoding, playback UI or cloud integration.

Default observation lasts ten seconds, with five-second connection/teardown
limits and a three-second video stall threshold. RTSP uses interleaved TCP.
A stall remains a failure even if audio or RTCP packets continue arriving.
Normal completion drops the session and waits for the client's TEARDOWN result.
The caller's Tokio runtime must stay alive until this wait finishes. Caller
cancellation drops the session and initiates teardown but cannot await it; callers
needing confirmed disconnect should allow the bounded probe to finish.

The report's resolution comes from codec parameters associated with received video
frames, not ONVIF configuration. Declared FPS comes from SDP or codec timing when
available; observed FPS counts received frame intervals over RTP media time. Audio
track codecs come from SDP, while audio frame counts establish actual receipt.
Readable means video frames continued for the bounded window without a protocol
error or a stall. This is compressed-frame readability, not full image decoding.
Missing or unknown properties remain absent/Unknown. Dependency errors and raw
transport metadata never enter reports. Library callers must filter dependency
logs; the example disables logs completely.

Build the runner with:

```sh
cargo build -p ferrissight-media --example rtsp_probe
```

The runner accepts exactly two source objects through an anonymous stdin pipe,
using separate `scheme`, `host`, `port`, `path`, and credential fields. Do not put
private input in shell commands, persisted JSON files, terminal transcripts or
committed examples. A local protected helper may authenticate through ONVIF,
call GetProfiles/GetStreamUri, strip URL userinfo, and feed the runner's stdin.
The existing Windows credential-store/ONVIF helper stays in ignored internal-doc;
that host-specific convenience is not part of the portable Rust API.

The runner tests each profile individually, then opens a fresh connection once
for each reconnect check, then reads both profiles concurrently. Only safe JSON
reports are printed. Nonzero exit means at least one read or teardown failed.
There is no automatic production reconnect loop or vendor-specific behavior.
