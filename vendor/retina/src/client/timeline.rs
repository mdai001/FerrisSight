// Copyright (C) The Retina Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::convert::TryFrom;
use std::num::{NonZeroI32, NonZeroU32};

use crate::Timestamp;

const MAX_FORWARD_TIME_JUMP_SECS: u32 = 10;

/// Creates [Timestamp]s (which don't wrap and can be converted to NPT aka normal play time)
/// from 32-bit (wrapping) RTP timestamps. Unstable, exposed for benchmark.
#[doc(hidden)]
#[derive(Debug)]
pub struct Timeline {
    timestamp: i64,
    clock_rate: NonZeroU32,
    start: Option<u32>,

    /// The maximum forward jump to allow, in clock rate units.
    /// If this is absent, don't do any enforcement of sane time units.
    max_forward_jump: Option<NonZeroI32>,

    /// The same in seconds, for logging.
    max_forward_jump_secs: u32,
}

impl Timeline {
    /// Creates a new timeline, erroring on crazy clock rates.
    pub fn new(
        start: Option<u32>,
        clock_rate: u32,
        enforce_with_max_forward_jump_secs: Option<NonZeroU32>,
    ) -> Result<Self, String> {
        let clock_rate = NonZeroU32::new(clock_rate)
            .ok_or_else(|| "clock_rate=0 rejected to prevent division by zero".to_string())?;
        let max_forward_jump = enforce_with_max_forward_jump_secs
            .map(|j| i32::try_from(u64::from(j.get()) * u64::from(clock_rate.get())))
            .transpose()
            .map_err(|_| {
                format!(
                    "clock_rate={clock_rate} rejected because max forward jump of {MAX_FORWARD_TIME_JUMP_SECS} sec exceeds i32::MAX"
                )
            })?
            .map(|j| NonZeroI32::new(j).expect("non-zero times non-zero must be non-zero"));
        Ok(Timeline {
            timestamp: i64::from(start.unwrap_or(0)),
            start,
            clock_rate,
            max_forward_jump,
            max_forward_jump_secs: enforce_with_max_forward_jump_secs
                .map(NonZeroU32::get)
                .unwrap_or(0),
        })
    }

    /// Advances to the given (wrapping) RTP timestamp.
    ///
    /// If enforcement was enabled, this produces a monotonically increasing
    /// [Timestamp], erroring on excessive or backward time jumps.
    pub fn advance_to(&mut self, rtp_timestamp: u32) -> Result<Timestamp, String> {
        let (timestamp, delta) = self.ts_and_delta(rtp_timestamp)?;
        if matches!(self.max_forward_jump, Some(j) if !(0..j.get()).contains(&delta)) {
            return Err(format!(
                "Timestamp jumped {} ({:.03} sec) from {} to {}; \
                   policy is to allow 0..{} sec only",
                delta,
                (delta as f64) / f64::from(self.clock_rate.get()),
                self.timestamp,
                timestamp,
                self.max_forward_jump_secs
            ));
        }
        self.timestamp = timestamp.timestamp;
        Ok(timestamp)
    }

    /// Places `rtp_timestamp` on the timeline without advancing the timeline
    /// or applying time jump policy. Will set the NPT epoch if unset.
    ///
    /// This is useful for RTP timestamps in RTCP packets. They commonly refer
    /// to time slightly before the most timestamp of the matching RTP stream.
    pub fn place(&mut self, rtp_timestamp: u32) -> Result<Timestamp, String> {
        Ok(self.ts_and_delta(rtp_timestamp)?.0)
    }

    fn ts_and_delta(&mut self, rtp_timestamp: u32) -> Result<(Timestamp, i32), String> {
        let start = match self.start {
            None => {
                self.start = Some(rtp_timestamp);
                self.timestamp = i64::from(rtp_timestamp);
                rtp_timestamp
            }
            Some(start) => start,
        };
        let delta = (rtp_timestamp as i32).wrapping_sub(self.timestamp as i32);
        let timestamp = self
            .timestamp
            .checked_add(i64::from(delta))
            .ok_or_else(|| {
                // This probably won't happen even with a hostile server. It'd
                // take ~2^32 packets (~ 4 billion) to advance the time this far
                // forward or backward even with no limits on time jump per
                // packet.
                format!(
                    "timestamp {} + delta {} won't fit in i64!",
                    self.timestamp, delta
                )
            })?;

        // Also error in similarly-unlikely NPT underflow.
        if timestamp.checked_sub(i64::from(start)).is_none() {
            return Err(format!(
                "timestamp {} + delta {} - start {} underflows i64!",
                self.timestamp, delta, start
            ));
        }
        Ok((
            Timestamp {
                timestamp,
                clock_rate: self.clock_rate,
                start,
            },
            delta,
        ))
    }
}
