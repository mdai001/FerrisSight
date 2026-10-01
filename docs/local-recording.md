# Phase 1B local recording

The `rtsp_record` example records one H.264 video stream for a bounded period
(default 120 seconds, maximum 300). It uses the same structured endpoint and
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
no automatic reconnect. Storage errors leave the affected file with `.partial`,
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

## Validation and current live blocker

The initial live Profile 0 test verified H.264 at 2560 × 1440, approximately
20 FPS. Eight finalized files (7.90–15.00 seconds) each decoded independently
with matching recorder/decoder frame counts and an initial keyframe. Explicit
eight-second graceful shutdown and forced TCP disconnect both finalized readable
accepted prefixes. No incomplete files remained after these tests. One run had a
100 ms source timestamp interval; it was preserved rather than filled or hidden.

The requested two-minute live run could not complete: the peer accepted OPTIONS,
then responded to SET_PARAMETER with status 400 and the previous request's CSeq.
Retina 0.4.20 rejected that unexpected response after roughly 15 seconds. No
camera-specific workaround or dependency fork was added. Full-length live runs
and live 30-second boundaries remain unverified until generic RTSP keepalive
compatibility is addressed. `clean_disconnect` means Retina's session-group
cleanup completed; a peer closing the connection can satisfy that without a
TEARDOWN acknowledgement.

A generated synthetic H.264 source supplied 65.05 seconds of media time over
accelerated local RTSP. It produced 30.00, 30.00 and 5.05 second segments with
600, 600 and 101 frames. All three independently decoded at 64 × 64, started
on keyframes, had 50 ms intervals, and matched contiguous source tick boundaries.
This validates segmentation independently of the live device's keepalive issue.

Official documentation confirms RTSP/ONVIF has no viewing time limit, but does
not specify keepalive method behavior: [TP-Link setup FAQ](https://www.tp-link.com/us/support/faq/2680/)
and [RTSP/ONVIF common questions](https://www.tp-link.com/gr/support/faq/4465/).
The observed stop is a protocol interoperability issue, not a documented 15-second
limit. A configurable generic keepalive policy in the RTSP dependency is a possible
follow-up; its behavior on this peer still needs validation.
