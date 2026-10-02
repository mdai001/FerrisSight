//! UTC natural-minute fault isolation. No endpoint or dependency diagnostics in reports.
use crate::core::{CameraId, StreamEndpoint};
use crate::media::recording::{
    record_attempt, unix_millis, MinuteContext, RecordError, RecordingEnd, RecordingOptions,
    RecordingReport, SessionTiming,
};
use retina::client::{SessionGroup, Transport};
use serde::Serialize;
use std::{future::Future, path::Path, sync::Arc, time::Duration};
use tokio::{
    sync::Semaphore,
    time::{timeout, Instant},
};

#[derive(Clone)]
pub struct MinuteRecordingOptions {
    pub duration: Duration,
    pub transport: Transport,
}
impl Default for MinuteRecordingOptions {
    fn default() -> Self {
        Self {
            duration: Duration::from_secs(1800),
            transport: Transport::default(),
        }
    }
}
#[derive(Debug, Serialize)]
pub struct MinuteWindowReport {
    pub logical_minute_start_unix_ms: i64,
    pub target_start_unix_ms: i64,
    pub target_end_unix_ms: i64,
    pub late_start_millis: u64,
    pub cleanup_wait_millis: u64,
    pub timing: SessionTiming,
    pub successful: bool,
    pub error: Option<String>,
    pub recording: Option<RecordingReport>,
}
#[derive(Debug, Serialize)]
pub struct MinuteRunReport {
    pub requested_seconds: f64,
    pub elapsed_seconds: f64,
    pub expected_windows: u64,
    pub successful_windows: u64,
    pub failed_windows: u64,
    pub attempted_windows: u64,
    pub unattempted_windows: u64,
    pub timing_measurements_valid: bool,
    pub setup_attempts: u64,
    pub setup_successes: u64,
    pub teardown_successes: u64,
    pub clock_step_events: u64,
    pub shutdown_requested: bool,
    /// Estimate on a gateway receive/RTP timeline; not sensor capture accuracy.
    pub missing_media_seconds: f64,
    /// Signed boundary delta: negative values explicitly mean overlap.
    pub adjacent_gap_seconds: Vec<f64>,
    pub windows: Vec<MinuteWindowReport>,
}
fn minute_floor(ms: i64) -> i64 {
    ms.div_euclid(60_000) * 60_000
}
fn clock_has_stepped(actual_ms: i64, projected_ms: i64) -> bool {
    actual_ms.abs_diff(projected_ms) > 1000
}
fn window_count(start: i64, end: i64) -> u64 {
    if end <= start {
        0
    } else {
        ((minute_floor(end - 1) - minute_floor(start)) / 60_000 + 1) as u64
    }
}
fn coverage(start: i64, end: i64, ranges: &mut [(i64, i64)]) -> (f64, Vec<f64>) {
    ranges.sort_unstable();
    let gaps = ranges
        .windows(2)
        .map(|p| (p[1].0 - p[0].1) as f64 / 1000.0)
        .collect();
    let mut covered = 0;
    let mut last = start;
    for &(a, b) in ranges.iter() {
        let a = a.max(start).max(last);
        let b = b.min(end);
        if b > a {
            covered += b - a;
            last = b;
        }
    }
    (((end - start - covered).max(0)) as f64 / 1000.0, gaps)
}
/// Each attempted UTC window is reported, including zero-yield failures. One setup per window.
/// Expected counts assume a stable wall clock; clock-step reports invalidate UTC coverage.
/// Each window uses a monotonic deadline derived from gateway UTC. Wall clock steps
/// resynchronize the next window; aggregate UTC coverage is invalidated after a step.
pub async fn record_minutes<F: Future<Output = ()>>(
    endpoint: &StreamEndpoint,
    camera_id: CameraId,
    directory: &Path,
    options: MinuteRecordingOptions,
    shutdown: F,
    mut on_window: impl FnMut(&MinuteWindowReport),
) -> Result<MinuteRunReport, RecordError> {
    if options.duration < Duration::from_secs(1) || options.duration > Duration::from_secs(3600) {
        return Err(RecordError::Configuration);
    }
    let started = Instant::now();
    let start_utc = unix_millis()?;
    let end_utc = start_utc
        .checked_add(options.duration.as_millis() as i64)
        .ok_or(RecordError::Configuration)?;
    let expected = window_count(start_utc, end_utc);
    let group = Arc::new(SessionGroup::default());
    let storage_slots = Arc::new(Semaphore::new(1));
    let finish = started + options.duration;
    let mut report = MinuteRunReport {
        requested_seconds: options.duration.as_secs_f64(),
        elapsed_seconds: 0.0,
        expected_windows: expected,
        successful_windows: 0,
        failed_windows: 0,
        attempted_windows: 0,
        unattempted_windows: 0,
        timing_measurements_valid: true,
        setup_attempts: 0,
        setup_successes: 0,
        teardown_successes: 0,
        clock_step_events: 0,
        shutdown_requested: false,
        missing_media_seconds: 0.0,
        adjacent_gap_seconds: vec![],
        windows: vec![],
    };
    tokio::pin!(shutdown);
    let mut next_begin = started;
    let mut clock_offset = 0_i64;
    while next_begin < finish {
        tokio::select! {
            _ = &mut shutdown => {report.shutdown_requested=true; break;}
            _ = tokio::time::sleep_until(next_begin) => {}
        }
        let now = Instant::now();
        if now >= finish {
            break;
        }
        let utc_now = unix_millis()?;
        let projected_utc = start_utc + started.elapsed().as_millis() as i64 + clock_offset;
        if clock_has_stepped(utc_now, projected_utc) {
            report.clock_step_events += 1;
            report.timing_measurements_valid = false;
            clock_offset = utc_now - (start_utc + started.elapsed().as_millis() as i64);
        }
        let minute = minute_floor(utc_now);
        let target_start = if report.windows.is_empty() {
            utc_now
        } else {
            minute
        };
        let deadline =
            (now + Duration::from_millis((minute + 60_000 - utc_now) as u64 + 1)).min(finish);
        let target_end = utc_now + deadline.saturating_duration_since(now).as_millis() as i64;
        let mut window = MinuteWindowReport {
            logical_minute_start_unix_ms: minute,
            target_start_unix_ms: target_start,
            target_end_unix_ms: target_end,
            late_start_millis: (utc_now - target_start).max(0) as u64,
            cleanup_wait_millis: 0,
            timing: SessionTiming::default(),
            successful: false,
            error: None,
            recording: None,
        };
        let cleanup_started = Instant::now();
        let status = group.stale_sessions();
        let cleanup = tokio::select! {
            _ = &mut shutdown => {report.shutdown_requested=true; break;}
            r=timeout(deadline.saturating_duration_since(Instant::now()).min(Duration::from_secs(20)),group.await_stale_sessions(&status)) => r.is_ok()
        };
        window.cleanup_wait_millis = cleanup_started.elapsed().as_millis() as u64;
        if !cleanup {
            window.error = Some("previous session cleanup pending".into());
        } else if Instant::now() >= deadline {
            window.error = Some("minute window elapsed before setup".into());
        } else {
            report.setup_attempts += 1;
            let attempt = record_attempt(
                endpoint,
                camera_id,
                directory,
                RecordingOptions {
                    duration: deadline.saturating_duration_since(Instant::now()),
                    segment_seconds: 60,
                },
                Some(MinuteContext {
                    window_start_unix_ms: minute,
                    deadline,
                    transport: options.transport.clone(),
                    group: group.clone(),
                    storage_slots: storage_slots.clone(),
                }),
                &mut shutdown,
            )
            .await;
            window.timing = attempt.timing;
            report.setup_successes += u64::from(window.timing.setup_succeeded);
            report.teardown_successes +=
                u64::from(window.timing.setup_succeeded && window.timing.clean_teardown);
            match attempt.result {
                Ok(recording) => {
                    if recording.end == RecordingEnd::Shutdown {
                        report.shutdown_requested = true;
                    }
                    window.successful = recording.end == RecordingEnd::Completed
                        && recording.clean_disconnect
                        && !recording.segments.is_empty();
                    window.recording = Some(recording);
                }
                Err(error) => window.error = Some(error.to_string()),
            }
        }
        report.successful_windows += u64::from(window.successful);
        on_window(&window);
        report.windows.push(window);
        next_begin = deadline;
        if report.shutdown_requested {
            break;
        }
    }
    report.elapsed_seconds = started.elapsed().as_secs_f64();
    report.attempted_windows = report.windows.len() as u64;
    report.expected_windows = report.expected_windows.max(report.attempted_windows);
    report.failed_windows = report.attempted_windows - report.successful_windows;
    report.unattempted_windows = report.expected_windows - report.attempted_windows;
    let mut ranges = report
        .windows
        .iter()
        .filter_map(|w| w.recording.as_ref())
        .flat_map(|r| &r.segments)
        .filter_map(|s| {
            s.utc_timing
                .as_ref()
                .map(|u| (u.first_frame_unix_ms, u.end_frame_unix_ms))
        })
        .collect::<Vec<_>>();
    let (missing, gaps) = coverage(start_utc, end_utc, &mut ranges);
    report.missing_media_seconds = missing;
    report.adjacent_gap_seconds = gaps;
    Ok(report)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clock_steps_are_detected_in_both_directions_without_integer_overflow() {
        assert!(!clock_has_stepped(1000, 2000));
        assert!(clock_has_stepped(1000, 2001));
        assert!(clock_has_stepped(2001, 1000));
        assert!(clock_has_stepped(i64::MIN, i64::MAX));
    }
    #[test]
    fn natural_windows_include_partial_minutes_without_extra_exact_boundary() {
        assert_eq!(window_count(59_000, 121_000), 3);
        assert_eq!(window_count(60_000, 120_000), 1);
        assert_eq!(window_count(59_000, 119_000), 2);
        assert_eq!(minute_floor(86_399_999), 86_340_000);
    }
    #[test]
    fn missing_time_includes_failed_windows_and_does_not_double_count_overlap() {
        let (missing, gaps) = coverage(
            0,
            180_000,
            &mut [(2_000, 59_000), (62_000, 90_000), (88_000, 100_000)],
        );
        assert_eq!(missing, 85.0);
        assert_eq!(gaps, vec![3.0, -2.0]);
    }
}
