//! Bounded persistent RTSP sessions with independently rotating UTC-minute MP4s.
use crate::core::{CameraId, StreamEndpoint};
use crate::media::minute::{clock_has_stepped, coverage, window_count};
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
pub struct PersistentRecordingOptions {
    pub duration: Duration,
    pub transport: Transport,
}
#[derive(Debug, Serialize)]
pub struct PersistentAttemptReport {
    pub started_after_seconds: f64,
    pub timing: SessionTiming,
    pub recording: Option<RecordingReport>,
    pub error: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct PersistentRunReport {
    pub requested_seconds: f64,
    pub elapsed_seconds: f64,
    pub target_start_unix_ms: i64,
    pub target_end_unix_ms: i64,
    pub expected_windows: u64,
    pub timing_measurements_valid: bool,
    pub setup_attempts: u64,
    pub setup_successes: u64,
    pub teardown_successes: u64,
    pub reconnect_attempts: u64,
    pub cleanup_wait_failures: u64,
    pub shutdown_requested: bool,
    pub missing_media_seconds: f64,
    pub adjacent_gap_seconds: Vec<f64>,
    pub attempts: Vec<PersistentAttemptReport>,
}
fn retry_delay(failures: usize) -> Duration {
    Duration::from_secs([0, 2, 5, 10, 30][failures.min(4)])
}
/// UTC boundaries rotate files only. Reconnect on unavailable/stalled sessions with
/// bounded backoff; each reconnect uses a fresh RTP anchor and independent fragment.
/// Storage/parameter/timestamp failures end the run rather than hiding corruption.
pub async fn record_persistent<F: Future<Output = ()>>(
    endpoint: &StreamEndpoint,
    camera_id: CameraId,
    directory: &Path,
    options: PersistentRecordingOptions,
    shutdown: F,
    mut on_attempt: impl FnMut(&PersistentAttemptReport),
) -> Result<PersistentRunReport, RecordError> {
    if options.duration.is_zero() || options.duration > Duration::from_secs(600) {
        return Err(RecordError::Configuration);
    }
    let started = Instant::now();
    let finish = started + options.duration;
    let start_utc = unix_millis()?;
    let end_utc = start_utc
        .checked_add(options.duration.as_millis() as i64)
        .ok_or(RecordError::Configuration)?;
    let mut report = PersistentRunReport {
        requested_seconds: options.duration.as_secs_f64(),
        elapsed_seconds: 0.0,
        target_start_unix_ms: start_utc,
        target_end_unix_ms: end_utc,
        expected_windows: window_count(start_utc, end_utc),
        timing_measurements_valid: true,
        setup_attempts: 0,
        setup_successes: 0,
        teardown_successes: 0,
        reconnect_attempts: 0,
        cleanup_wait_failures: 0,
        shutdown_requested: false,
        missing_media_seconds: 0.0,
        adjacent_gap_seconds: vec![],
        attempts: vec![],
    };
    let group = Arc::new(SessionGroup::default());
    let storage_slots = Arc::new(Semaphore::new(1));
    tokio::pin!(shutdown);
    let mut failures = 0;
    let mut delay = Duration::ZERO;
    while Instant::now() < finish {
        tokio::select! {
            _ = &mut shutdown => {report.shutdown_requested = true; break;}
            _ = tokio::time::sleep_until((Instant::now() + delay).min(finish)) => {}
        }
        if Instant::now() >= finish {
            break;
        }
        let status = group.stale_sessions();
        let cleaned = tokio::select! {
            _ = &mut shutdown => {report.shutdown_requested = true; break;}
            result = timeout(finish.saturating_duration_since(Instant::now()).min(Duration::from_secs(5)), group.await_stale_sessions(&status)) => result.is_ok()
        };
        if !cleaned {
            report.cleanup_wait_failures += 1;
            delay = retry_delay(failures);
            failures = failures.saturating_add(1);
            continue;
        }
        if Instant::now() >= finish {
            break;
        }
        let utc = unix_millis()?;
        report.timing_measurements_valid &=
            !clock_has_stepped(utc, start_utc + started.elapsed().as_millis() as i64);
        report.reconnect_attempts += u64::from(report.setup_attempts > 0);
        report.setup_attempts += 1;
        let attempt_started = started.elapsed().as_secs_f64();
        let attempt = record_attempt(
            endpoint,
            camera_id,
            directory,
            RecordingOptions {
                duration: finish.saturating_duration_since(Instant::now()),
                segment_seconds: 60,
            },
            Some(MinuteContext {
                window_start_unix_ms: utc.div_euclid(60_000) * 60_000,
                deadline: finish,
                transport: options.transport.clone(),
                group: group.clone(),
                storage_slots: storage_slots.clone(),
                rotate_utc_minutes: true,
            }),
            &mut shutdown,
        )
        .await;
        report.setup_successes += u64::from(attempt.timing.setup_succeeded);
        report.teardown_successes +=
            u64::from(attempt.timing.setup_succeeded && attempt.timing.clean_teardown);
        // Reset after 30 seconds of healthy received media, not merely a successful setup.
        if matches!((attempt.timing.first_received_unix_ms, attempt.timing.last_received_unix_ms),
            (Some(first), Some(last)) if last.saturating_sub(first) >= 30_000)
        {
            failures = 0;
        }
        let mut row = PersistentAttemptReport {
            started_after_seconds: attempt_started,
            timing: attempt.timing,
            recording: None,
            error: None,
        };
        let retry = match attempt.result {
            Ok(recording) => {
                report.shutdown_requested |= recording.end == RecordingEnd::Shutdown;
                let retry = matches!(
                    recording.end,
                    RecordingEnd::Stalled
                        | RecordingEnd::Disconnected
                        | RecordingEnd::ProtocolError
                );
                row.recording = Some(recording);
                retry
            }
            Err(error) => {
                let retry = matches!(
                    error,
                    RecordError::Connection
                        | RecordError::Timeout
                        | RecordError::NoFrames(
                            RecordingEnd::Stalled
                                | RecordingEnd::Disconnected
                                | RecordingEnd::ProtocolError
                        )
                );
                row.error = Some(error.to_string());
                retry
            }
        };
        on_attempt(&row);
        report.attempts.push(row);
        if !retry || report.shutdown_requested {
            break;
        }
        delay = retry_delay(failures);
        failures = failures.saturating_add(1);
    }
    report.elapsed_seconds = started.elapsed().as_secs_f64();
    report.timing_measurements_valid &= !clock_has_stepped(
        unix_millis()?,
        start_utc + started.elapsed().as_millis() as i64,
    );
    let mut ranges = report
        .attempts
        .iter()
        .filter_map(|a| a.recording.as_ref())
        .flat_map(|r| &r.segments)
        .filter_map(|s| {
            s.utc_timing
                .as_ref()
                .map(|t| (t.first_frame_unix_ms, t.end_frame_unix_ms))
        })
        .collect::<Vec<_>>();
    (report.missing_media_seconds, report.adjacent_gap_seconds) =
        coverage(start_utc, end_utc, &mut ranges);
    Ok(report)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_progression_is_bounded_and_resettable() {
        assert_eq!(
            (0..7).map(|i| retry_delay(i).as_secs()).collect::<Vec<_>>(),
            vec![0, 2, 5, 10, 30, 30, 30]
        );
        assert_eq!(retry_delay(usize::MAX), Duration::from_secs(30));
    }
}
