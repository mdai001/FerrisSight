# Bounded RTSP session comparison

The existing `ferrissight::media::minute::record_minutes` mode opens a fresh RTSP
session for each UTC minute. The optional
`ferrissight::media::persistent::record_persistent` mode keeps a session across
UTC boundaries and rotates MP4 files independently, at the first safe keyframe
in the next logical minute. Both reuse the same generic Retina receive path,
H.264-only remuxer, adaptive keepalive policy, bounded queue and teardown.

Persistent mode is limited to 600 seconds. On a stalled or failed RTSP session,
it finalizes usable video, closes the session and retries with delays of
0, 2, 5, 10 and then 30 seconds. Thirty seconds of healthy received video reset
that backoff. Stale-session cleanup is bounded and precedes a new setup.
Storage, parameter and timestamp errors stop the run; corrupted staging files
are never promoted. UTC minute boundaries do not reconnect a healthy session.
Every reconnect uses a fresh timestamp anchor and a new independently playable
fragment. There is no merging, backfilling, transcoding or gap repair.

Build `cargo build --example rtsp_record`. The credential-safe stdin input accepts
`persistent_sessions: true`; it is mutually exclusive with `minute_sessions`.
The legacy bounded interval recorder remains unchanged. This comparison mode
adds no daemon, UI, retention, cloud upload or vendor-specific behavior.

## Validation method

Both modes are observed sequentially for exactly 600 seconds each, using Profile 0
and RTSP-over-TCP. Each has a 590-second recording budget and a final ten-second
reserve for file finalization and teardown. An external watchdog enforces the
600-second cap; no run is extended. Coverage is reported both against the
590-second recording budget and against the entire 600-second observation.
The reserve is not classified as a camera failure. Early terminal interruptions
still count as missing media through the requested recording budget.

Every completed MP4 is decoded independently with local FFmpeg, without uploading
media or retaining decoder output. Decoded frame counts are compared with each
safe sidecar. Coverage unions gateway receive/RTP-derived intervals clipped to
the requested window, avoiding double-counting overlap. It is estimated media
time, not an exact sensor-capture or missing-frame measurement. Continuous
intervals merge adjacent file boundaries within one millisecond; reconnect gaps
remain visible. First and last natural-minute windows may be partial.

Process RSS and FD counts are sampled locally approximately every two seconds.
Other camera clients were requested to be closed but their absence was not
independently confirmed. This is one ordered A-then-B comparison, with no crossover
or long-term endurance qualification.

## Observed results

| Metric | A: session per UTC minute | B: persistent sessions |
| --- | ---: | ---: |
| Observation hard cap / elapsed observation | 600 / 600 s | 600 / 600 s |
| Requested recording budget | 590 s | 590 s |
| Recorder elapsed, including cleanup | 558.537 s | 590.158 s |
| Expected UTC minute windows | 11 | 11 |
| UTC windows touched by recorded media | 11 | 11 |
| Logical UTC minutes containing completed files | 11 | 10 |
| MP4 files | 11 | 11 |
| Estimated media coverage of recording budget | 92.915% | 74.726% |
| Estimated media coverage of entire observation | 91.366% | 73.481% |
| Estimated media seconds | 548.196 | 440.886 |
| Estimated missing recording-budget seconds | 41.804 | 149.114 |
| Missing observation time including drain reserve | 51.804 s | 159.114 s |
| Video stalls | 1 | 3 |
| Setup attempts / successes | 11 / 11 | 10 / 4 |
| Setup failures | 0 | 6 |
| Scheduled minute-session refresh attempts | 10 | 0 |
| Unscheduled reconnect attempts | 0 | 9 |
| Teardown failures among successful setups | 0 / 11 | 2 / 4 |
| Stale-cleanup wait timeouts | 0 | 2 |
| Segment duration min / median / max | 4.878 / 59.596 / 59.797 s | 11.024 / 40.088 / 61.468 s |
| Independent full decode successes | 11 / 11 | 11 / 11 |
| Decoded frame counts matching sidecars | 11 / 11 | 11 / 11 |
| Total decoded frames | 8,205 | 6,599 |
| Adjacent file gaps min / median / max | 0.212 / 0.419 / 0.539 s | 0 / 0 / 69.662 s |
| Longest continuous estimated media interval | 59.797 s | 206.117 s |
| RSS min / max | 10,484 / 11,096 KiB | 7,288 / 10,872 KiB |
| RSS after first 60 s, min / max | 10,832 / 11,096 KiB | 8,304 / 10,872 KiB |
| First / last observation-minute median RSS | 10,872 / 11,096 KiB | 8,304 / 9,216 KiB |
| FD min / max | 9 / 11 | 9 / 11 |
| Resource samples | 260 | 295 |
| Longest resource sampling gap | 40.574 s | 2.010 s |
| Leftover `.partial` files | 0 | 0 |
| Forced process termination | No | No |

Both modes produced H.264 at 2560 x 1440. Weighted frame rates are approximately
15 FPS; all decoded frame counts match stored sample counts. No transcoding was
used for recording. There was no observed persistent FD accumulation. RSS changed
within the ranges above; this short sample does not establish leak-free endurance.

A ended its last window early after a stall. It finalizes that window's valid
media and does not retry within the same minute, so it exited before the remaining
requested budget elapsed. Missing-time estimates include that unrecorded tail.
Ten A windows completed normally and one was interrupted. B recovered three stalled
sessions; six additional setup attempts failed. Its final session completed and
acknowledged teardown. Earlier two B teardown failures remain counted, even though
later sessions recovered.

B's within-session keyframe boundaries have zero estimated gaps and can extend
slightly past their logical UTC boundary. This explains the 61.468-second maximum,
and why media touches eleven windows while files belong to ten logical minutes.
A file/window presence never implies complete coverage. Adjacent-file gap statistics
exclude the initial and terminal missing intervals; the total missing-time metric
includes them.

A's executable was replaced during preparation of B, requiring a supplemental
resource monitor. Both monitors were combined; the resulting 40.574-second sampling
gap is reported explicitly. A's media recording was unaffected. B had continuous
approximately two-second sampling. Neither run was repeated or extended.

Successful-setup latency min / median / max was 33 / 43 / 64 ms for A and
38 / 47.5 / 1,240 ms for B. Teardown latency was 0 / 0 / 2,872 ms for A and
0 / 3,918.5 / 5,001 ms for B. Cleanup timeouts and backoff contribute to B's longer
recovery gaps. Setup failures and RTSP responses are reported only through fixed
redacted categories, never transport details.

## Architecture decision and limits

B was not clearly better in this bounded comparison: it lost 107.31 more seconds
of estimated media and had more stalls, setup failures and teardown failures.
Keep the existing minute-session mode and retain persistent recording as an
explicit experimental option. Longer endurance testing is deferred, not performed
as part of this task. This comparison does not establish either model as an
unconditionally reliable default for all cameras.

Persistent UTC segmentation itself worked: independently playable files span
multiple minutes without forcing a session reconnect or losing boundary samples.
Synthetic tests cover calendar rollover, delayed initial keyframes and recovery
without resetting the overall deadline. Camera slot contention and other clients
were not controlled independently, so the ordered runs do not establish the root
cause of the observed RTSP failures.

The existing MP4 restrictions remain: monotonic H.264 with PTS equal to DTS, no
audio recording, last-sample duration estimated from the previous/declared interval,
and gateway receive/RTP-derived UTC rather than authoritative sensor capture UTC.
Transient intra-session wall-clock excursions are not exhaustively detected.
MP4 and JSON publication is not jointly atomic; MP4 is the final commit marker.
Hardening against unrelated out-of-band metadata writers remains future work.

## Privacy audit

Public source, synthetic tests, report fields and package contents were reviewed.
No endpoints, credentials, local paths, account or hardware identifiers, raw ONVIF
responses or recordings are committed. The 32-entry package listing excludes local
agent guidance, internal work, clips and private diagnostics. Local credentials
stay in the existing OS store and anonymous stdin flow. External review received
public code only. Future protected production credentials, retention and binary
release audits remain deferred.
