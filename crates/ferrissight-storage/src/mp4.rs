//! Video-only H.264 remuxing. No codec decoding, filesystem paths or source identity in reports.
use crate::{CameraId, RecordingId};
use bytes::Bytes;
use mp4::{AvcConfig, Mp4Config, Mp4Sample, Mp4Writer, TrackConfig};
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

#[derive(Clone, PartialEq, Eq)]
pub struct H264Config {
    pub width: u16,
    pub height: u16,
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
    pub timescale: u32,
}
/// MP4 length-prefixed NAL units, in decode order. This minimal sink requires PTS = DTS.
pub struct VideoSample {
    pub timestamp: i64,
    pub keyframe: bool,
    pub data: Vec<u8>,
}
#[derive(Debug, thiserror::Error)]
pub enum RecordingError {
    #[error("recording storage operation failed")]
    Storage,
    #[error("unsupported or changed H.264 parameters")]
    Parameters,
    #[error("unsupported reordered or invalid video timestamps")]
    Timestamp,
    #[error("invalid recording configuration")]
    Configuration,
}
#[derive(Debug, Serialize)]
pub struct SegmentReport {
    pub sequence: u64,
    pub frames: u64,
    pub duration_seconds: f64,
    /// Source RTP elapsed ticks; no network or account identity.
    pub first_timestamp: i64,
    pub end_timestamp: i64,
    pub timescale: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utc_timing: Option<SegmentUtcTiming>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SegmentUtcTiming {
    pub logical_minute_start_unix_ms: i64,
    pub first_frame_unix_ms: i64,
    pub end_frame_unix_ms: i64,
    pub timestamp_basis: &'static str,
}
#[derive(Debug, Clone, Copy)]
pub struct UtcMinuteTarget {
    pub camera_id: CameraId,
    pub window_start_unix_ms: i64,
    pub anchor_unix_ms: i64,
    pub anchor_rtp_ticks: i64,
}
struct ActiveSegment {
    writer: Mp4Writer<File>,
    partial: PathBuf,
    completed: PathBuf,
    first: i64,
    frames: u64,
    utc_timing: Option<(UtcMinuteTarget, PathBuf)>,
}
/// Synchronous sink: callers must use a blocking worker, with bounded input buffering.
/// Files are created exclusively. Only finalized, synced files receive an `.mp4` suffix.
/// A failed write/crash may leave an ignored `.partial` file; it is never advertised as complete.
pub struct Mp4Segments {
    directory: PathBuf,
    id: RecordingId,
    config: H264Config,
    segment_ticks: i64,
    sequence: u64,
    active: Option<ActiveSegment>,
    pending: Option<VideoSample>,
    last_duration: u32,
    reports: Vec<SegmentReport>,
    utc_target: Option<UtcMinuteTarget>,
    utc_directory: Option<PathBuf>,
    pub discarded_before_keyframe: u64,
}
impl Mp4Segments {
    pub fn new(
        directory: &Path,
        config: H264Config,
        segment_seconds: u32,
        terminal_duration: u32,
    ) -> Result<Self, RecordingError> {
        if config.width == 0
            || config.height == 0
            || config.sps.is_empty()
            || config.pps.is_empty()
            || config.timescale == 0
            || segment_seconds == 0
            || terminal_duration == 0
        {
            return Err(RecordingError::Configuration);
        }
        fs::create_dir_all(directory).map_err(|_| RecordingError::Storage)?;
        Ok(Self {
            directory: directory.into(),
            id: RecordingId::generate(),
            segment_ticks: i64::from(config.timescale) * i64::from(segment_seconds),
            config,
            sequence: 0,
            active: None,
            pending: None,
            last_duration: terminal_duration,
            reports: Vec::new(),
            utc_target: None,
            utc_directory: None,
            discarded_before_keyframe: 0,
        })
    }
    pub fn new_utc_minute(
        directory: &Path,
        config: H264Config,
        terminal_duration: u32,
        target: UtcMinuteTarget,
    ) -> Result<Self, RecordingError> {
        if target.window_start_unix_ms < 0
            || target.window_start_unix_ms % 60_000 != 0
            || terminal_duration == 0
            || config.width == 0
            || config.height == 0
            || config.sps.is_empty()
            || config.pps.is_empty()
            || config.timescale == 0
        {
            return Err(RecordingError::Configuration);
        }
        let instant = chrono::DateTime::from_timestamp_millis(target.window_start_unix_ms)
            .ok_or(RecordingError::Configuration)?;
        let minute_dir = directory
            .join(format!("camera-{}", target.camera_id))
            .join(instant.format("%Y").to_string())
            .join(instant.format("%m").to_string())
            .join(instant.format("%d").to_string())
            .join(instant.format("%H").to_string());
        fs::create_dir_all(&minute_dir).map_err(|_| RecordingError::Storage)?;
        Ok(Self {
            directory: minute_dir.clone(),
            id: RecordingId::generate(),
            segment_ticks: i64::MAX,
            config,
            sequence: 0,
            active: None,
            pending: None,
            last_duration: terminal_duration,
            reports: Vec::new(),
            utc_target: Some(target),
            utc_directory: Some(minute_dir),
            discarded_before_keyframe: 0,
        })
    }
    fn start(&mut self, timestamp: i64) -> Result<(), RecordingError> {
        let (partial, completed, utc_timing) = if let (Some(target), Some(directory)) =
            (self.utc_target, self.utc_directory.as_ref())
        {
            let instant = chrono::DateTime::from_timestamp_millis(target.window_start_unix_ms)
                .ok_or(RecordingError::Configuration)?;
            let minute = instant.format("%M");
            let mut selected = None;
            for sequence in 0..u64::MAX {
                let stem = format!("{minute}_{sequence:03}");
                let partial = directory.join(format!("{stem}.mp4.partial"));
                let completed = directory.join(format!("{stem}.mp4"));
                let metadata_partial = directory.join(format!("{stem}.json.partial"));
                let metadata_completed = directory.join(format!("{stem}.json"));
                if completed.exists() || metadata_completed.exists() || metadata_partial.exists() {
                    continue;
                }
                let mut create = OpenOptions::new();
                create.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    create.mode(0o600);
                }
                match create.open(&partial) {
                    Ok(file) => {
                        drop(file);
                        self.sequence = sequence;
                        selected = Some((partial, completed, Some((target, metadata_partial))));
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(_) => return Err(RecordingError::Storage),
                }
            }
            selected.ok_or(RecordingError::Storage)?
        } else {
            let stem = format!("{}-{:06}", self.id.0, self.sequence);
            let partial = self.directory.join(format!("{stem}.partial"));
            let completed = self.directory.join(format!("{stem}.mp4"));
            (partial, completed, None)
        };
        let mut open = OpenOptions::new();
        open.write(true);
        if utc_timing.is_none() {
            open.create_new(true);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            open.mode(0o600);
        }
        let file = open.open(&partial).map_err(|_| RecordingError::Storage)?;
        let config = Mp4Config {
            major_brand: "isom".parse().map_err(|_| RecordingError::Configuration)?,
            minor_version: 512,
            compatible_brands: vec![
                "isom".parse().unwrap(),
                "iso2".parse().unwrap(),
                "avc1".parse().unwrap(),
                "mp41".parse().unwrap(),
            ],
            timescale: self.config.timescale,
        };
        let mut writer =
            Mp4Writer::write_start(file, &config).map_err(|_| RecordingError::Storage)?;
        let mut track = TrackConfig::from(AvcConfig {
            width: self.config.width,
            height: self.config.height,
            seq_param_set: self.config.sps.clone(),
            pic_param_set: self.config.pps.clone(),
        });
        track.timescale = self.config.timescale;
        writer
            .add_track(&track)
            .map_err(|_| RecordingError::Storage)?;
        self.active = Some(ActiveSegment {
            writer,
            partial,
            completed,
            first: timestamp,
            frames: 0,
            utc_timing,
        });
        Ok(())
    }
    fn write_pending(&mut self, duration: u32) -> Result<(), RecordingError> {
        let sample = self.pending.take().ok_or(RecordingError::Timestamp)?;
        let active = self.active.as_mut().ok_or(RecordingError::Timestamp)?;
        let start_time = u64::try_from(
            sample
                .timestamp
                .checked_sub(active.first)
                .ok_or(RecordingError::Timestamp)?,
        )
        .map_err(|_| RecordingError::Timestamp)?;
        active
            .writer
            .write_sample(
                1,
                &Mp4Sample {
                    start_time,
                    duration,
                    rendering_offset: 0,
                    is_sync: sample.keyframe,
                    bytes: Bytes::from(sample.data),
                },
            )
            .map_err(|_| RecordingError::Storage)?;
        active.frames += 1;
        self.last_duration = duration;
        Ok(())
    }
    fn finalize(&mut self, end: i64) -> Result<(), RecordingError> {
        let Some(mut active) = self.active.take() else {
            return Ok(());
        };
        active
            .writer
            .write_end()
            .map_err(|_| RecordingError::Storage)?;
        active
            .writer
            .into_writer()
            .sync_all()
            .map_err(|_| RecordingError::Storage)?;
        // UUID + exclusive staging creation prevents collisions; never overwrite a completed file.
        if active.completed.exists() {
            return Err(RecordingError::Storage);
        }
        let utc_timing = if let Some((target, metadata_partial)) = active.utc_timing.as_ref() {
            let first_frame_unix_ms =
                timestamp_to_unix_ms(*target, active.first, self.config.timescale)?;
            let end_frame_unix_ms = timestamp_to_unix_ms(*target, end, self.config.timescale)?;
            let timing = SegmentUtcTiming {
                logical_minute_start_unix_ms: target.window_start_unix_ms,
                first_frame_unix_ms,
                end_frame_unix_ms,
                timestamp_basis: "gateway_receive_anchor_plus_rtp_elapsed",
            };
            let metadata = serde_json::json!({
                "cameraId": target.camera_id,
                "codec": "H264",
                "resolution": { "width": self.config.width, "height": self.config.height },
                "segmentReport": { "sequence": self.sequence, "frames": active.frames, "durationSeconds": (end-active.first) as f64 / f64::from(self.config.timescale), "firstTimestamp": active.first, "endTimestamp": end, "timescale": self.config.timescale },
                "utcTiming": timing,
            });
            let mut metadata_open = OpenOptions::new();
            metadata_open.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                metadata_open.mode(0o600);
            }
            let mut file = metadata_open
                .open(metadata_partial)
                .map_err(|_| RecordingError::Storage)?;
            serde_json::to_writer(&mut file, &metadata).map_err(|_| RecordingError::Storage)?;
            file.sync_all().map_err(|_| RecordingError::Storage)?;
            Some(timing)
        } else {
            None
        };
        if let Some((_, metadata_partial)) = active.utc_timing.as_ref() {
            let metadata_final = metadata_partial.with_extension("");
            let metadata_final = metadata_final.with_extension("json");
            // The pair is not atomic; publishing MP4 last makes it the completed-media marker.
            fs::rename(metadata_partial, metadata_final).map_err(|_| RecordingError::Storage)?;
        }
        if active.completed.exists() {
            return Err(RecordingError::Storage);
        }
        fs::rename(&active.partial, &active.completed).map_err(|_| RecordingError::Storage)?;
        self.reports.push(SegmentReport {
            sequence: self.sequence,
            frames: active.frames,
            duration_seconds: (end - active.first) as f64 / f64::from(self.config.timescale),
            first_timestamp: active.first,
            end_timestamp: end,
            timescale: self.config.timescale,
            utc_timing,
        });
        self.sequence += 1;
        Ok(())
    }
    pub fn push(&mut self, sample: VideoSample) -> Result<(), RecordingError> {
        if self.active.is_none() {
            if !sample.keyframe {
                self.discarded_before_keyframe += 1;
                return Ok(());
            }
            self.start(sample.timestamp)?;
        }
        if let Some(previous) = self.pending.as_ref() {
            let delta = sample
                .timestamp
                .checked_sub(previous.timestamp)
                .ok_or(RecordingError::Timestamp)?;
            let duration = u32::try_from(delta)
                .ok()
                .filter(|d| *d > 0 && *d <= self.config.timescale.saturating_mul(3))
                .ok_or(RecordingError::Timestamp)?;
            self.write_pending(duration)?;
        }
        let first = self.active.as_ref().unwrap().first;
        if sample.keyframe && sample.timestamp - first >= self.segment_ticks {
            self.finalize(sample.timestamp)?;
            self.start(sample.timestamp)?;
        }
        self.pending = Some(sample);
        Ok(())
    }
    /// Final sample duration uses the latest measured interval (or declared interval for one frame).
    pub fn finish(mut self) -> Result<(Vec<SegmentReport>, u64), RecordingError> {
        if let Some(last) = self.pending.as_ref() {
            let end = last
                .timestamp
                .checked_add(i64::from(self.last_duration))
                .ok_or(RecordingError::Timestamp)?;
            self.write_pending(self.last_duration)?;
            self.finalize(end)?;
        }
        Ok((self.reports, self.discarded_before_keyframe))
    }
}

fn timestamp_to_unix_ms(
    target: UtcMinuteTarget,
    timestamp: i64,
    timescale: u32,
) -> Result<i64, RecordingError> {
    let elapsed = timestamp
        .checked_sub(target.anchor_rtp_ticks)
        .ok_or(RecordingError::Timestamp)?;
    let elapsed_ms = i128::from(elapsed)
        .checked_mul(1000)
        .ok_or(RecordingError::Timestamp)?
        / i128::from(timescale);
    i64::try_from(
        i128::from(target.anchor_unix_ms)
            .checked_add(elapsed_ms)
            .ok_or(RecordingError::Timestamp)?,
    )
    .map_err(|_| RecordingError::Timestamp)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> H264Config {
        H264Config {
            width: 16,
            height: 16,
            sps: vec![0x67, 0x42, 0, 0x1e, 0xf4, 0x4b, 0x20],
            pps: vec![0x68, 0xce, 0x3c, 0x80],
            timescale: 1000,
        }
    }
    fn push_utc_samples(sink: &mut Mp4Segments) {
        for i in 0..4 {
            sink.push(VideoSample {
                timestamp: 100_000 + i * 250,
                keyframe: i == 0,
                data: vec![0, 0, 0, 2, if i == 0 { 0x65 } else { 0x41 }, 0x80],
            })
            .unwrap();
        }
    }
    fn utc_target(window_start_unix_ms: i64, camera_id: CameraId) -> UtcMinuteTarget {
        UtcMinuteTarget {
            camera_id,
            window_start_unix_ms,
            anchor_unix_ms: window_start_unix_ms + 20,
            anchor_rtp_ticks: 100_000,
        }
    }
    #[test]
    fn utc_minute_layout_metadata_and_sequence_are_safe() {
        let root =
            std::env::temp_dir().join(format!("ferrissight-test-{}", RecordingId::generate().0));
        let window = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .timestamp_millis();
        let target = utc_target(window, CameraId::generate());
        let mut first = Mp4Segments::new_utc_minute(&root, config(), 250, target).unwrap();
        first
            .push(VideoSample {
                timestamp: 99_750,
                keyframe: false,
                data: vec![0, 0, 0, 1, 0x41],
            })
            .unwrap();
        push_utc_samples(&mut first);
        let (reports, _) = first.finish().unwrap();
        assert_eq!(reports.len(), 1);
        let report = &reports[0];
        let timing = report.utc_timing.as_ref().unwrap();
        assert_eq!(timing.first_frame_unix_ms, target.anchor_unix_ms);
        assert_eq!(timing.end_frame_unix_ms, target.anchor_unix_ms + 1000);
        let dir = root
            .join(format!("camera-{}", target.camera_id))
            .join("2026/01/01/00");
        assert!(dir.join("00_000.mp4").is_file());
        let metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join("00_000.json")).unwrap()).unwrap();
        assert_eq!(
            metadata["utcTiming"]["logical_minute_start_unix_ms"],
            window
        );
        let keys = metadata
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            keys,
            [
                "cameraId",
                "codec",
                "resolution",
                "segmentReport",
                "utcTiming"
            ]
            .into_iter()
            .collect()
        );
        let file = File::open(dir.join("00_000.mp4")).unwrap();
        let len = file.metadata().unwrap().len();
        let mut reader = mp4::Mp4Reader::read_header(file, len).unwrap();
        assert_eq!(reader.sample_count(1).unwrap(), 4);
        assert!(reader.read_sample(1, 1).unwrap().unwrap().is_sync);

        let completed_before = fs::read(dir.join("00_000.mp4")).unwrap();
        let mut second = Mp4Segments::new_utc_minute(&root, config(), 250, target).unwrap();
        push_utc_samples(&mut second);
        second.finish().unwrap();
        assert!(dir.join("00_001.mp4").is_file());
        assert_eq!(fs::read(dir.join("00_000.mp4")).unwrap(), completed_before);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn utc_stale_partial_is_preserved_and_skipped() {
        let root =
            std::env::temp_dir().join(format!("ferrissight-test-{}", RecordingId::generate().0));
        let window = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .timestamp_millis();
        let target = utc_target(window, CameraId::generate());
        let dir = root
            .join(format!("camera-{}", target.camera_id))
            .join("2026/01/01/00");
        fs::create_dir_all(&dir).unwrap();
        let stale = b"preserve stale staging bytes";
        fs::write(dir.join("00_000.mp4.partial"), stale).unwrap();
        let mut sink = Mp4Segments::new_utc_minute(&root, config(), 250, target).unwrap();
        push_utc_samples(&mut sink);
        sink.finish().unwrap();
        assert_eq!(fs::read(dir.join("00_000.mp4.partial")).unwrap(), stale);
        assert!(dir.join("00_001.mp4").is_file());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn utc_directory_paths_roll_over_year_day_and_hour() {
        let root =
            std::env::temp_dir().join(format!("ferrissight-test-{}", RecordingId::generate().0));
        let camera_id = CameraId::generate();
        let before = chrono::DateTime::parse_from_rfc3339("2026-12-31T23:59:00Z")
            .unwrap()
            .timestamp_millis();
        let after = chrono::DateTime::parse_from_rfc3339("2027-01-01T00:00:00Z")
            .unwrap()
            .timestamp_millis();
        let before_target = utc_target(before, camera_id);
        let after_target = utc_target(after, camera_id);
        let before_sink = Mp4Segments::new_utc_minute(&root, config(), 250, before_target).unwrap();
        drop(before_sink);
        let after_sink = Mp4Segments::new_utc_minute(&root, config(), 250, after_target).unwrap();
        drop(after_sink);
        let camera_dir = root.join(format!("camera-{camera_id}"));
        assert!(camera_dir.join("2026/12/31/23").is_dir());
        assert!(camera_dir.join("2027/01/01/00").is_dir());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn segments_start_on_keyframes_preserve_intervals_and_finalize_last_sample() {
        let directory =
            std::env::temp_dir().join(format!("ferrissight-test-{}", RecordingId::generate().0));
        let mut sink = Mp4Segments::new(&directory, config(), 1, 250).unwrap();
        for i in 0..10 {
            sink.push(VideoSample {
                timestamp: i * 250,
                keyframe: i == 1 || i == 6,
                data: vec![0, 0, 0, 2, if i == 1 || i == 6 { 0x65 } else { 0x41 }, 0x80],
            })
            .unwrap();
        }
        let (reports, dropped) = sink.finish().unwrap();
        assert_eq!(dropped, 1);
        assert_eq!(reports.iter().map(|s| s.frames).sum::<u64>(), 9);
        assert_eq!(reports[0].duration_seconds, 1.25);
        assert_eq!(reports[0].end_timestamp, reports[1].first_timestamp);
        for entry in fs::read_dir(&directory).unwrap() {
            let file = File::open(entry.unwrap().path()).unwrap();
            let len = file.metadata().unwrap().len();
            let mut reader = mp4::Mp4Reader::read_header(file, len).unwrap();
            let count = reader.sample_count(1).unwrap();
            assert!(reader.read_sample(1, 1).unwrap().unwrap().is_sync);
            for id in 1..=count {
                let sample = reader.read_sample(1, id).unwrap().unwrap();
                assert_eq!(sample.duration, 250);
                assert_eq!(sample.start_time, u64::from(id - 1) * 250);
            }
        }
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn thirty_second_segments_have_no_boundary_frame_loss() {
        let directory =
            std::env::temp_dir().join(format!("ferrissight-test-{}", RecordingId::generate().0));
        let mut sink = Mp4Segments::new(&directory, config(), 30, 50).unwrap();
        for i in 0..1300 {
            sink.push(VideoSample {
                timestamp: i * 50,
                keyframe: i % 620 == 0,
                data: vec![0, 0, 0, 2, if i % 620 == 0 { 0x65 } else { 0x41 }, 0x80],
            })
            .unwrap();
        }
        let (reports, discarded) = sink.finish().unwrap();
        assert_eq!(discarded, 0);
        assert_eq!(reports.iter().map(|r| r.frames).sum::<u64>(), 1300);
        assert_eq!(reports.len(), 3);
        assert_eq!(reports[0].duration_seconds, 31.0);
        assert_eq!(reports[1].duration_seconds, 31.0);
        assert_eq!(reports[2].duration_seconds, 3.0);
        for pair in reports.windows(2) {
            assert_eq!(pair[0].end_timestamp, pair[1].first_timestamp);
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reordered_timestamps_fail_without_publishing_partial_segment() {
        let directory =
            std::env::temp_dir().join(format!("ferrissight-test-{}", RecordingId::generate().0));
        let mut sink = Mp4Segments::new(&directory, config(), 1, 250).unwrap();
        sink.push(VideoSample {
            timestamp: 100,
            keyframe: true,
            data: vec![0, 0, 0, 1, 0x65],
        })
        .unwrap();
        assert!(matches!(
            sink.push(VideoSample {
                timestamp: 50,
                keyframe: false,
                data: vec![]
            }),
            Err(RecordingError::Timestamp)
        ));
        drop(sink);
        assert!(fs::read_dir(&directory)
            .unwrap()
            .all(|e| e.unwrap().path().extension().unwrap() == "partial"));
        fs::remove_dir_all(directory).unwrap();
    }
}
