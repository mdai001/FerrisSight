# Phase 1B local recording

The `rtsp_record` example records one H.264 video stream for a bounded period
(default 120 seconds, maximum 600). It uses the same structured endpoint and
Retina RTSP-over-TCP path as probing. Profile selection and ONVIF authentication,
GetProfiles and GetStreamUri remain in the local integration helper; the Rust
ONVIF adapter is still unimplemented. There is no camera-model logic in recording.

Build with `cargo build -p ferrissight-media --example rtsp_record`. The runner
accepts one source through an anonymous stdin pipe using the same source schema
as the probing example. Optional top-level `duration_seconds` and
`shutdown_after_seconds` control a bounded run and graceful timed shutdown.
Use an OS credential store/local credential prompt and do not put actual endpoint
or credential values into shell history, committed configuration or input files.
Ctrl-C and Unix SIGTERM also request graceful finalization. Outputs are safe JSON reports only.

The media layer sends compressed length-prefixed H.264 samples through a bounded
queue to a blocking storage worker. The storage layer uses the established `mp4`
crate to remux those samples without decoding or transcoding. Audio is omitted.
Files are written in the runner's `recordings/` working-directory subdirectory,
which is ignored by Git. A generated recording UUID plus sequence identifies
files; reports use a generated CameraId and media properties only. No camera
names, network addresses, absolute paths or account details enter filenames or
container metadata. New Unix files request owner-only permissions; effective
permissions on Windows-mounted filesystems also depend on Windows ACLs.

Each segment begins at an IDR/random-access frame. The first frames before an IDR
are deliberately skipped and counted. A segment closes at the first IDR at or
after 30 seconds of source media time, so duration depends on the camera's GOP.
No frame is dropped at a normal boundary. A one-frame buffer derives sample
duration from the next RTP timestamp, including across boundaries. Source 90 kHz
intervals are preserved, and each file's timeline is normalized to zero. Reports
retain source elapsed tick boundaries for continuity checks. The final frame uses
the latest measured interval, or a declared/default interval for a one-frame file.
No wall-clock capture time or audio/video synchronization is claimed.

This first version requires monotonically increasing presentation timestamps and
assumes PTS equals DTS. B-frame reordering, parameter changes and invalid timestamp
jumps are unsupported and fail explicitly. RTP loss ends the run before accepting
the damaged frame. Disconnect, protocol error or a three-second video stall ends
the run and finalizes the accepted prefix; the report marks the failure. There is
a wrapper permits one reconnect with a one-second backoff. Each connection has
independent files and a new source timestamp origin; failed attempts remain explicit
in the report. Packet loss, invalid timestamps and storage errors are not retried. Storage errors leave the affected file with `.partial`,
never a completed `.mp4` suffix. Previously finalized files remain usable.

Graceful shutdown drains accepted frames, writes MP4 indexes, syncs the file and
renames it from `.partial` to `.mp4`, then awaits bounded RTSP teardown. Abrupt
process death, dropping the recording future, power loss and storage failure are
not graceful shutdown; incomplete files may remain. This is conventional MP4,
not fragmented MP4 or crash-recoverable storage. No retention, indexing, uploads,
telemetry or continuous recording service is introduced.

Validate each completed file independently with a local decoder, using an output
null sink rather than retaining decoded frames. Compare decoded counts with
segment reports, verify keyframe starts and adjacent source timestamp boundaries.
Do not publish clips or raw decoder diagnostics, which may include local paths.

## Generic keepalive compatibility

FerrisSight uses a local, narrowly adapted Retina 0.4.20 dependency because the
upstream API does not expose keepalive method selection. Production sources and
Apache-2.0 licensing are retained; upstream recorded-device fixtures are excluded.
Upstream `Auto` behavior remains available and unchanged. FerrisSight opts into
`Adaptive`: initially OPTIONS, then prefer advertised GET_PARAMETER over
SET_PARAMETER. A parameter-method rejection (400, 405, 451 or 501) permanently
downgrades that connection to OPTIONS. A response must match the pending CSeq;
only a rejected parameter keepalive may echo the immediately preceding successful
keepalive's CSeq. Missing/unknown CSeq, malformed success, OPTIONS rejection,
authentication errors and other protocol errors remain fatal. Pending-response
timeouts remain bounded. OPTIONS-only mode is available to validate peers that
incorrectly advertise parameter methods. Reports contain counters and method enums.

Session failures finalize the accepted prefix and allow one bounded reconnect.
There is no continuity claim across reconnects. Graceful shutdown waits up to
five seconds for Retina's explicit teardown policy; unsuccessful cleanup is
reported rather than treated as a successful run.

Reference implementations differ: Retina selects SET_PARAMETER ahead of
GET_PARAMETER after OPTIONS advertises support and rejects wrong CSeq; Moonfire
uses Retina and retries failed streams; go2rtc periodically sends OPTIONS and its
streaming receive loop does not apply Retina's strict keepalive CSeq/status checks.
FerrisSight retains strict checks outside the narrow rejection downgrade.
See [Retina](https://github.com/scottlamb/retina),
[Moonfire streamer](https://github.com/scottlamb/moonfire-nvr/blob/master/server/src/streamer.rs)
and [go2rtc RTSP connection](https://github.com/AlexxIT/go2rtc/blob/master/pkg/rtsp/conn.go).

Synthetic integration scenarios cover fallback, continued media delivery,
permanent downgrade despite repeated advertisements and strict error rejection.
Live validation remains incomplete. A rejected SET_PARAMETER with the previous
successful OPTIONS CSeq caused exactly one malformed-response downgrade; media
continued on OPTIONS until a later stall. OPTIONS-only reached 68.615 seconds
of media before stalling. GET_PARAMETER received successful responses without
fallback; a 125-second bounded run recovered once and ended cleanly, but its
longest uninterrupted session contained 93.404 seconds of media. A subsequent
five-minute attempt stalled after 46.368 seconds and reconnection timed out.
No uninterrupted two-minute or successful five-minute result is claimed.

The GET_PARAMETER bounded run produced four independently decodable H.264
2560 × 1440 files: 24.053 seconds/360 frames, then 30.266/453, 32.070/480 and
31.068/465 after reconnect. All segment frame counts matched independent decode;
within each session source tick boundaries were contiguous, with approximately
15 FPS and no unexpected boundary loss. Timelines remain separate across reconnect.
Both sessions acknowledged clean teardown. A safe local protocol-timing check
confirmed correct 200 responses to GET_PARAMETER before media packets stopped;
the later TEARDOWN received no response. This observation does not establish
whether the remaining cause lies in the peer, transport or another client.
Those stalls are a separate unresolved reliability limit, not evidence that
successful keepalive responses guarantee uninterrupted media.

A generated synthetic H.264 source supplied 65.05 seconds of media time over
accelerated local RTSP. It produced 30.00, 30.00 and 5.05 second segments with
600, 600 and 101 frames. All three independently decoded at 64 × 64, started
on keyframes, had 50 ms intervals, and matched contiguous source tick boundaries.
This validates segmentation independently of the live device's keepalive issue.

Official documentation confirms RTSP/ONVIF has no viewing time limit, but does
not specify keepalive method behavior: [TP-Link setup FAQ](https://www.tp-link.com/us/support/faq/2680/)
and [RTSP/ONVIF common questions](https://www.tp-link.com/gr/support/faq/4465/).
The original SET_PARAMETER failure is a protocol interoperability issue, not a
documented viewing limit. Later stalls must be evaluated separately from that
specific rejection.
