//! Video-only H.264 remuxing. No codec decoding, filesystem paths or source identity in reports.
use crate::RecordingId;
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
}
struct ActiveSegment {
    writer: Mp4Writer<File>,
    partial: PathBuf,
    completed: PathBuf,
    first: i64,
    frames: u64,
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
            discarded_before_keyframe: 0,
        })
    }
    fn start(&mut self, timestamp: i64) -> Result<(), RecordingError> {
        let stem = format!("{}-{:06}", self.id.0, self.sequence);
        let partial = self.directory.join(format!("{stem}.partial"));
        let completed = self.directory.join(format!("{stem}.mp4"));
        let mut open = OpenOptions::new();
        open.write(true).create_new(true);
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
        fs::rename(&active.partial, &active.completed).map_err(|_| RecordingError::Storage)?;
        self.reports.push(SegmentReport {
            sequence: self.sequence,
            frames: active.frames,
            duration_seconds: (end - active.first) as f64 / f64::from(self.config.timescale),
            first_timestamp: active.first,
            end_timestamp: end,
            timescale: self.config.timescale,
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
