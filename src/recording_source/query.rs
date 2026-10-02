//! Bounded allowlist decoding of already-authenticated recording query results.
use super::*;
use chrono::NaiveDate;
use serde_json::Value;
use std::collections::BTreeSet;
fn object(bytes: &[u8]) -> Result<Value, SourceError> {
    if bytes.len() > 256 * 1024 {
        return Err(SourceError::ResourceLimit);
    }
    serde_json::from_slice(bytes).map_err(|_| SourceError::Protocol)
}
pub fn dates(bytes: &[u8]) -> Result<Vec<NaiveDate>, SourceError> {
    let value = object(bytes)?;
    let list = value.as_array().ok_or(SourceError::Protocol)?;
    if list.len() > 366 {
        return Err(SourceError::ResourceLimit);
    }
    let mut dates = BTreeSet::new();
    for value in list {
        let date = value.as_str().ok_or(SourceError::Protocol)?;
        if date.len() != 8 || !date.bytes().all(|b| b.is_ascii_digit()) {
            return Err(SourceError::Protocol);
        }
        dates.insert(NaiveDate::parse_from_str(date, "%Y%m%d").map_err(|_| SourceError::Protocol)?);
    }
    Ok(dates.into_iter().collect())
}
/// Parses only the search_video_results array, after control envelope/error checks.
/// correction_seconds must come from a separately verified camera clock policy.
/// No guessed event-type mapping or vendor response data is retained.
pub fn ranges(
    bytes: &[u8],
    camera: CameraId,
    query: &RangeQuery,
    correction_seconds: i64,
) -> Result<Vec<RecordingRange>, SourceError> {
    #[derive(serde::Deserialize)]
    struct RawRange {
        #[serde(rename = "startTime")]
        start: i64,
        #[serde(rename = "endTime")]
        end: i64,
    }
    if bytes.len() > 256 * 1024 {
        return Err(SourceError::ResourceLimit);
    }
    let list: Vec<std::collections::BTreeMap<String, RawRange>> =
        serde_json::from_slice(bytes).map_err(|_| SourceError::Protocol)?;
    if list.len() > usize::from(query.page_size()) {
        return Err(SourceError::ResourceLimit);
    }
    let mut ranges = Vec::new();
    for row in list {
        if row.len() != 1 {
            return Err(SourceError::Protocol);
        }
        let raw = row.into_values().next().ok_or(SourceError::Protocol)?;
        let time = |n: i64| -> Result<DateTime<Utc>, SourceError> {
            DateTime::from_timestamp(
                n.checked_add(correction_seconds)
                    .ok_or(SourceError::Protocol)?,
                0,
            )
            .ok_or(SourceError::Protocol)
        };
        let utc =
            UtcRange::new(time(raw.start)?, time(raw.end)?).map_err(|_| SourceError::Protocol)?;
        if utc.end() <= query.utc().start() || utc.start() >= query.utc().end() {
            return Err(SourceError::Protocol);
        }
        ranges.push(RecordingRange {
            camera_id: camera,
            utc,
            kind: RecordingKind::Unknown,
            source_id: None,
        });
    }
    ranges.sort_by_key(|r| r.utc.start());
    Ok(ranges)
}
/// Budgeted cursor tracking; query/camera binding remains the adapter's responsibility.
pub struct PaginationGuard {
    seen: BTreeSet<String>,
    calls: usize,
    results: usize,
    max_calls: usize,
    max_results: usize,
}
impl PaginationGuard {
    pub fn new(max_calls: usize, max_results: usize) -> Result<Self, SourceError> {
        if max_calls == 0 || max_calls > 1024 || max_results == 0 || max_results > 65536 {
            return Err(SourceError::InvalidRequest);
        }
        Ok(Self {
            seen: BTreeSet::new(),
            calls: 0,
            results: 0,
            max_calls,
            max_results,
        })
    }
    pub fn observe(
        &mut self,
        count: usize,
        next: Option<&OpaqueRecordingId>,
    ) -> Result<(), SourceError> {
        self.calls = self.calls.saturating_add(1);
        self.results = self.results.saturating_add(count);
        if self.calls > self.max_calls || self.results > self.max_results {
            return Err(SourceError::ResourceLimit);
        }
        if let Some(next) = next {
            if !self.seen.insert(next.expose_for_protocol().to_owned()) {
                return Err(SourceError::Protocol);
            }
        }
        Ok(())
    }
}
