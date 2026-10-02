# Phase 1C: UTC minute-session recording

This is a bounded interoperability experiment, not an always-on recording daemon.
The gateway creates a fresh generic RTSP session for each UTC natural-minute
window. It remuxes compressed H.264 into one independently playable MP4 without
transcoding or audio. Camera models and vendor identifiers do not influence policy.

## Lifecycle

```text
UTC minute window -> DESCRIBE/SETUP/PLAY -> first IDR -> H.264 MP4
                 -> finalize local files -> bounded teardown -> next fresh session
```

The first and last windows may be partial. Setup and initial keyframe acquisition
consume window time; no exact 60-second duration is promised. The minute deadline
includes setup. Every completed file starts at a random-access frame; ending a
file does not require an IDR because it does not depend on a subsequent file.
The final sample duration uses the last measured RTP interval. Source timestamp
intervals are preserved, with each MP4 normalized to zero. Existing PTS=DTS,
monotonic-timestamp and fixed-H.264-parameter restrictions remain in force.

One setup attempt is made per window. A stall, RTSP error or loss finalizes the
accepted prefix when safe, marks that window unsuccessful, then waits for the
next natural minute. Failure does not extend the old stream into the next minute.
Setup has a five-second limit, video stalls a three-second limit, and teardown a
five-second wait. A shared Retina session group waits for stale cleanup before
new setup, with a bounded wait; pending cleanup is an explicit failed window.
Successful cleanup means Retina confirms the session is removed/inactive, rather
than claiming every peer necessarily returns a particular TEARDOWN status.

Media ingestion uses a bounded channel and blocking storage worker. Minute-mode
finalization waits up to five seconds. One storage-worker permit per bounded run
prevents accumulating new workers if an old filesystem operation remains stuck.
Rust cannot forcibly cancel blocking filesystem I/O: a hung worker may finish
later, and process shutdown can still depend on that I/O returning. This is a
limit of the experiment, not a crash-recovery or storage-process isolation claim.

## UTC layout and metadata

```text
recordings/
  camera-<FerrisSight-generated-id>/
    YYYY/MM/DD/HH/
      MM_000.mp4
      MM_000.json
      MM_001.mp4
      MM_001.json
```

Sequence allocation uses exclusive staging creation and skips existing completed
or partial files. It never intentionally replaces an existing completed file.
Data is written through `.partial` files, finalized and synced. UTC metadata is
published before the MP4; atomic MP4 rename is the completed-unit marker. The two
renames are not a filesystem transaction: a crash may leave orphan metadata or
staging files, but MP4 is published last. No retention or crash-recovery scan is
implemented here. Unix file creation requests owner-only permissions; filesystem
ACL behavior still depends on the host filesystem.

The JSON sidecar contains only a generated CameraId, codec/resolution, sequence,
frame count, RTP timing and UTC timing. UTC fields identify the logical minute,
first media time and exclusive end media time. The basis is explicitly
`gateway_receive_anchor_plus_rtp_elapsed`: the gateway observes the first frame's
receive time, then maps RTP elapsed ticks to UTC. These are authoritative gateway
recording observations, not a claim of precise sensor capture time, NTP-synchronized
camera clocks or accurate cross-camera synchronization. The caller must retain
its generated CameraId when reopening an already registered camera; the bounded
example generates one test identity per invocation.

Wall clock is checked against a monotonic projection at each window. A step over
one second resynchronizes future natural-minute windows and marks aggregate UTC
coverage invalid. Existing files are immutable; repeated UTC labels allocate
another sequence. Skipped wall-clock labels during a forward clock step are not
fabricated as recorded intervals. Expected-window counts and missing-time metrics
are meaningful for a stable clock; clock events must be reviewed separately.

## Bounded runner and privacy

Build `cargo build --example rtsp_record`. The example accepts
runtime endpoint components and protected credentials through an anonymous stdin
pipe, with `minute_sessions: true` and `duration_seconds` between 1 and 3600.
`profile_index` is a synthetic report label supplied by the local ONVIF adapter.
Default transport is Retina's RTSP-over-TCP; optional `udp: true` is an experimental
isolation setting, not the default. ONVIF provisioning/profile selection still
uses the existing local adapter. Credentials never become arguments or input files.

Dependency logs and panic details are suppressed. Per-window and final reports
contain fixed error categories, safe timing/media properties and generated IDs;
no addresses, URLs, raw responses, device identifiers, accounts or paths. Recordings,
sidecars, test helpers and working metrics remain local and Git-ignored. No Drive,
remote access, UI, retention, AI, telemetry or transcoding is added.

## Validation method

The bounded real-camera experiment observes at least 30 minutes of wall time.
Expected windows include first/last partial windows. Reports distinguish attempted,
unattempted, successful and failed windows, setup successes and clean cleanup.
A successful window reaches its deadline, finalizes nonempty media and cleans up;
a readable prefix from an early stall is not counted as a successful window.

Every completed MP4 is decoded independently by a separate local decoder into a
null sink, checking first keyframe and matching frame count against its sidecar.
Reports give per-file duration/frame counts, setup/cleanup latency, signed adjacent
timeline deltas, and total uncovered requested time. Negative boundary deltas mean
overlap. Missing-time estimates union and clip media intervals against the entire
requested observation horizon, including zero-yield windows, setup/keyframe delays
and outages. They are gateway/RTP estimates, not exact sensor-frame-loss counts.
RSS and file descriptors are sampled from the recorder process every five seconds;
startup samples are distinguished from steady-state behavior. Decoder processes
are separate from the monitored recorder. Temporary raw decoder output is never
logged or persisted.

## Real-camera results

The bounded Profile 0 test ran for **1800.101 seconds** (30 minutes), spanning
31 UTC windows: 29 natural full-minute targets plus first/last partial targets
of 50.910 and 9.085 seconds. All 31 windows were attempted. Window success means
reaching its deadline, publishing nonempty media and confirming cleanup; it does
not mean 60 seconds of uninterrupted coverage.

| Metric | Observed result |
| --- | --- |
| Successful / unsuccessful windows | 16 / 15 |
| Failure categories | 13 media stalls; 2 setup failures |
| Windows with finalized media | 29 / 31 |
| Setup successes | 29 / 31 (93.55%) |
| Cleanup successes after setup | 24 / 29 (82.76%) |
| Independent MP4 decode | 29 / 29; all first keyframes and frame counts matched |
| Media | H.264, 2560 × 1440; weighted observed rate 14.947 FPS |
| Total finalized frames | 17,829 |
| File duration min / median / max | 3.475 / 58.060 / 59.931 seconds |
| Total source-media duration | 1192.839 seconds |
| Uncovered requested time estimate | 607.269 seconds (33.74%) |
| All adjacent gaps min / median / max | 0.331 / 0.762 / 90.647 seconds |
| Normal completed-window transitions min / median / max | 0.331 / 0.411 / 0.788 seconds |
| Successful setup min / median / max | 27 / 36 / 370 ms |
| Failed setup durations | 5001 and 2599 ms |
| Cleanup wait min / median / max | 0 / 0 / 5001 ms |
| Leftover partials at completion | 0 |
| Clock-step events | 0 |

The total RTP duration differs slightly from UTC coverage because coverage is
clipped to the observation horizon and the final sample uses an inferred interval.
Missing-time estimates cover inter-file gaps and zero-yield windows; they do not
claim exact counts of sensor frames that were never transmitted. Maximum observed
in-file timestamp interval was 0.200 seconds, preserved without interpolation.

The process was sampled 361 times at five-second intervals. After the first
60 seconds, RSS was 8512–9524 KiB (median 9156 KiB), and FD samples were 9–11
(median 11). Mean RSS in the early steady-state period was 9005.5 KiB versus
9408.0 KiB in the final period, a 402.5 KiB increase. FD counts did not accumulate;
the RSS change was small over this trial, but this is not proof against longer-term
leaks. The last ten windows had 10 successful setups versus 9 in the first ten,
and 6 successful windows versus 3 in the first ten. No progressive loss of camera
availability was observed. An additional ten-second post-test session established
in 51 ms, recorded 149 frames / 9.955 seconds and cleaned up successfully.

There were 13 observed transitions from an unsuccessful window to a later window
that again produced media. This demonstrates automatic recovery without poisoning
subsequent attempts, including recovery after setup failures. It does not identify
why the peer stopped media or establish that another client was responsible.
Other camera clients could not be independently confirmed absent.

A short Profile 1 comparison ran 125.208 seconds over three windows. Setup and
cleanup succeeded three times; two windows completed and one stalled. All three
640 × 360 H.264 files decoded independently, totaling 1398 frames, with durations
10.089, 59.865 and 23.452 seconds. TCP was already the existing/default transport,
so there was no distinct “TCP versus current” configuration to compare. UDP was
not needed for this bounded comparison. No vendor app was opened deliberately
by the test, but exclusivity remains unverified.

**Assessment:** UTC minutes work as immutable storage and fault-accounting units.
The bounded-session experiment continued and recovered throughout 30 minutes,
without accumulating FD handles or permanently losing camera access. It did not
eliminate frequent stalls or cleanup failures. Keep forced per-minute RTSP restart
as an explicit experimental mode rather than declaring it a validated default.
Default-session-policy selection needs a controlled client-isolation comparison
and a bounded retry strategy within a minute. The current implementation retries
at the next UTC window; configurable seconds-based backoff, fanout and health APIs
remain separate work. There is no gap repair or merging across unavailable periods.

### Per-window files

A zero-frame row is an explicitly failed setup, not an omitted observation.

| Window | Outcome | File duration (s) | Frames |
| --- | --- | ---: | ---: |
| 1 | Stalled | 46.568 | 697 |
| 2 | Completed | 58.060 | 867 |
| 3 | Stalled | 9.755 | 146 |
| 4 | Stalled | 29.665 | 444 |
| 5 | Completed | 59.864 | 896 |
| 6 | Completed | 59.530 | 891 |
| 7 | Stalled | 24.855 | 372 |
| 8 | Stalled | 29.730 | 445 |
| 9 | Setup timeout | 0.000 | 0 |
| 10 | Stalled | 5.345 | 80 |
| 11 | Stalled | 29.397 | 440 |
| 12 | Setup failed | 0.000 | 0 |
| 13 | Completed | 59.895 | 874 |
| 14 | Completed | 59.597 | 892 |
| 15 | Completed | 59.664 | 893 |
| 16 | Completed | 59.261 | 887 |
| 17 | Stalled | 13.363 | 200 |
| 18 | Completed | 59.931 | 897 |
| 19 | Stalled | 9.889 | 148 |
| 20 | Completed | 59.931 | 897 |
| 21 | Completed | 59.597 | 892 |
| 22 | Completed | 59.597 | 892 |
| 23 | Stalled | 28.596 | 428 |
| 24 | Stalled | 29.799 | 446 |
| 25 | Completed | 59.864 | 896 |
| 26 | Completed | 59.664 | 893 |
| 27 | Stalled | 3.475 | 52 |
| 28 | Stalled | 29.799 | 446 |
| 29 | Completed | 59.865 | 896 |
| 30 | Completed | 59.530 | 891 |
| 31 | Completed | 8.753 | 131 |
