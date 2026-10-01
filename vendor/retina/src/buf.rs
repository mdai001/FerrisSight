// Copyright (C) The Retina Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Power-of-two ring buffer for network I/O.

use crate::inputs::Split;
use crate::to_u64;
use std::sync::{Arc, Mutex};

/// A reference to a byte range within a [`MarkBuf`].
///
/// 10 bytes of data (16 with padding).
#[derive(Clone, Copy, Debug)]
pub(crate) struct BufRange {
    pub pos: u64,
    pub len: u16,
}

/// A power-of-two ring buffer.
///
/// Data is accessed as up to two slices (the ring's discontiguous halves).
/// This is simpler than maintaining a copy region for contiguous access
/// and sufficient when callers can work with split slices (e.g. the RTSP
/// parser via [`crate::inputs::Split`]).
///
/// `start` and `end` are monotonically increasing stream positions;
/// the index into `buf` is obtained by masking with `buf.len() - 1`.
///
/// ```text
/// Contiguous data (start_i < end_i):
///
///   0             start_i        end_i       buf.len()
///   |  free space  | === data === |  free space  |
///
/// Wrapped data (end_i <= start_i):
///
///   0        end_i          start_i          buf.len()
///   | = data = |  free space  | ==== data ==== |
/// ```
pub(crate) struct RingBuf {
    buf: Box<[u8]>,

    /// Stream position of the first valid byte.
    start: u64,

    /// Stream position one past the last valid byte.
    end: u64,
}

#[allow(dead_code)] // Some methods reserved for future steps.
impl RingBuf {
    /// Creates a new ring buffer with at least `capacity` bytes
    /// (rounded up to a power of two).
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1).checked_next_power_of_two().unwrap();
        Self {
            buf: vec![0; capacity].into_boxed_slice(),
            start: 0,
            end: 0,
        }
    }

    #[inline]
    fn mask(&self) -> usize {
        self.buf.len() - 1
    }

    /// Stream position of the first valid byte.
    #[inline]
    pub fn start(&self) -> u64 {
        self.start
    }

    /// Stream position one past the last valid byte.
    #[inline]
    pub fn end(&self) -> u64 {
        self.end
    }

    /// Number of valid bytes in the buffer.
    #[inline]
    pub fn len(&self) -> usize {
        (self.end - self.start) as usize
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Ring capacity (always a power of two).
    #[inline]
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Free space available for writing without reallocation.
    #[inline]
    pub fn available(&self) -> usize {
        self.buf.len() - self.len()
    }

    /// Returns data at `[pos, pos+len)` as a [`Split`].
    ///
    /// # Panics
    ///
    /// Panics if the range is outside `[start, end)`.
    pub fn split(&self, pos: u64, len: usize) -> Split<'_> {
        assert!(
            pos >= self.start && pos + to_u64(len) <= self.end,
            "split({pos}, {len}) outside valid range [{}, {})",
            self.start,
            self.end,
        );
        let i = pos as usize & self.mask();
        let contiguous = self.buf.len() - i;
        if let Some(second_len) = len.checked_sub(contiguous) {
            Split::new(&self.buf[i..], &self.buf[..second_len])
        } else {
            Split::new(&self.buf[i..i + len], &[])
        }
    }

    /// Returns all buffered data as a [`Split`].
    pub fn data_split(&self) -> Split<'_> {
        if self.is_empty() {
            return Split::new(&[], &[]);
        }
        self.split(self.start, self.len())
    }

    /// Marks bytes before `pos` as consumed.
    ///
    /// # Panics
    ///
    /// Panics unless `start <= pos <= end`.
    pub fn advance_to(&mut self, pos: u64) {
        assert!(
            self.start <= pos && pos <= self.end,
            "advance_to({pos}): must be within [{}, {}]",
            self.start,
            self.end,
        );
        self.start = pos;
    }

    /// Returns the free space as up to two mutable slices.
    ///
    /// After writing `n` bytes into these slices (starting from the first),
    /// call [`advance_end`](Self::advance_end) to mark them as valid.
    ///
    /// Ensures at least `reserve` bytes of free space (growing if needed).
    pub fn spare_capacity(&mut self, reserve: usize) -> (&mut [u8], &mut [u8]) {
        self.reserve(reserve);
        if self.len() == self.buf.len() {
            // Full. `start_i == end_i` below, which is indistinguishable from
            // empty, so special-case it rather than returning the whole ring.
            return (&mut [], &mut []);
        }
        let mask = self.mask();
        let end_i = self.end as usize & mask;
        let start_i = self.start as usize & mask;

        if end_i < start_i {
            // Data wraps; free space is contiguous [end_i, start_i).
            (&mut self.buf[end_i..start_i], &mut [])
        } else {
            // Data is contiguous (or buffer is empty);
            // free space wraps: [end_i, buf.len()) and [0, start_i).
            let (head, tail) = self.buf.split_at_mut(end_i);
            (tail, &mut head[..start_i])
        }
    }

    /// Marks `n` additional bytes at the end as valid.
    ///
    /// # Panics
    ///
    /// Panics if `n > self.available()`.
    pub fn advance_end(&mut self, n: usize) {
        assert!(
            n <= self.available(),
            "advance_end({n}): only {} available",
            self.available(),
        );
        self.end += to_u64(n);
    }

    /// Appends `data` to the buffer, growing if necessary.
    #[cfg(test)]
    pub fn extend(&mut self, data: &[u8]) {
        let (s1, s2) = self.spare_capacity(data.len());
        let mid = data.len().min(s1.len());
        s1[..mid].copy_from_slice(&data[..mid]);
        s2[..data.len() - mid].copy_from_slice(&data[mid..]);
        self.advance_end(data.len());
    }

    /// Ensures capacity for at least `additional` more bytes.
    #[inline]
    fn reserve(&mut self, additional: usize) {
        if additional > self.available() {
            self.realloc(additional);
        }
    }

    #[cold]
    fn realloc(&mut self, additional: usize) {
        let data_len = self.len();
        let new_size = data_len
            .checked_add(additional)
            .and_then(usize::checked_next_power_of_two)
            .unwrap();
        debug_assert!(new_size > self.buf.len());
        let mut new_buf = vec![0u8; new_size].into_boxed_slice();

        if data_len > 0 {
            let (s1, s2) = self.split(self.start, data_len).slices();
            // `start`'s index within the new ring is `start & (new_size - 1)`,
            // which is *not* necessarily its index within the old ring: it
            // differs by whichever multiple of the old size `start` has wrapped
            // by. The data can therefore still wrap around the end of the new
            // ring, so copy each source half in up to two chunks. `data_len <=
            // new_size`, so this never overwrites what it just wrote.
            let mut i = self.start as usize & (new_size - 1);
            for src in [s1, s2] {
                let contiguous = new_size - i;
                if let Some(second_len) = src.len().checked_sub(contiguous) {
                    new_buf[i..].copy_from_slice(&src[..contiguous]);
                    new_buf[..second_len].copy_from_slice(&src[contiguous..]);
                    i = second_len;
                } else {
                    new_buf[i..i + src.len()].copy_from_slice(src);
                    i += src.len();
                }
            }
        }

        self.buf = new_buf;
    }
}

impl std::fmt::Debug for RingBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RingBuf")
            .field("start", &self.start)
            .field("end", &self.end)
            .field("capacity", &self.buf.len())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Mark system
// ---------------------------------------------------------------------------

/// Shared registry of mark positions, held by [`MarkBuf`] and each live [`Mark`].
///
/// Uses `Arc<Mutex>` rather than `Rc<RefCell>` so that `Connection` (which
/// contains `MarkBuf`) remains `Send`—required for background teardown tasks.
#[derive(Debug)]
struct MarkRegistry {
    marks: Mutex<Vec<u64>>,
}

impl MarkRegistry {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            marks: Mutex::new(Vec::new()),
        })
    }

    fn add(&self, pos: u64) {
        let mut marks = self.marks.lock().unwrap();
        let i = marks.partition_point(|&m| m < pos);
        marks.insert(i, pos);
    }

    fn remove(&self, pos: u64) {
        let mut marks = self.marks.lock().unwrap();
        if let Ok(i) = marks.binary_search(&pos) {
            marks.remove(i);
        } else {
            panic!("Mark::drop: no mark at position {pos}");
        }
    }

    fn earliest(&self) -> Option<u64> {
        self.marks.lock().unwrap().first().copied()
    }
}

/// An RAII handle pinning a buffer position.
///
/// Data from this position onward stays accessible in the [`MarkBuf`].
/// Automatically releases when dropped, preventing mark leaks.
pub(crate) struct Mark {
    pos: u64,
    registry: Arc<MarkRegistry>,
}

impl Mark {
    /// The stream position this mark holds open.
    #[inline]
    #[allow(dead_code)]
    pub fn pos(&self) -> u64 {
        self.pos
    }
}

impl Drop for Mark {
    fn drop(&mut self) {
        self.registry.remove(self.pos);
    }
}

impl std::fmt::Debug for Mark {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mark").field("pos", &self.pos).finish()
    }
}

/// A [`RingBuf`] wrapper with mark-based retention.
///
/// Marks pin stream positions so data from that point onward stays
/// accessible. The buffer can only reclaim space behind the oldest mark.
///
/// Mark operations use interior mutability (`&self`) so they can be used
/// while the depacketizer borrows the buffer. Only `spare_capacity` /
/// `advance_end` / `advance_unparsed` require `&mut self`.
///
/// # Invariant: marks never sit after `unparsed`
///
/// Every live [`Mark`] is at a position `<= unparsed`. [`reclaim`](Self::reclaim)
/// depends on this: it advances the ring's `start` to the earliest mark, which
/// would discard not-yet-parsed bytes if a mark could be beyond `unparsed`.
///
/// Callers get this for free by parsing a packet out of the buffer (which
/// advances `unparsed` past it) before handing it to a depacketizer that may
/// mark it. [`add_mark_at`](Self::add_mark_at) asserts it.
#[derive(Debug)]
pub(crate) struct MarkBuf {
    ring: RingBuf,
    registry: Arc<MarkRegistry>,
    unparsed: u64,
}

#[allow(dead_code)] // Some methods reserved for future steps.
impl MarkBuf {
    /// Creates a new buffer with at least `capacity` bytes.
    pub fn new(capacity: usize) -> Self {
        Self {
            ring: RingBuf::new(capacity),
            registry: MarkRegistry::new(),
            unparsed: 0,
        }
    }

    /// The current unparsed position (where new marks are created).
    #[inline]
    pub fn unparsed(&self) -> u64 {
        self.unparsed
    }

    /// Stream position one past the last buffered byte.
    #[inline]
    pub fn end(&self) -> u64 {
        self.ring.end()
    }

    /// Number of bytes between unparsed and end.
    #[inline]
    pub fn unparsed_len(&self) -> usize {
        (self.ring.end() - self.unparsed) as usize
    }

    /// Returns unparsed data as a [`Split`].
    pub fn unparsed_split(&self) -> Split<'_> {
        let len = self.unparsed_len();
        if len == 0 {
            return Split::new(&[], &[]);
        }
        self.ring.split(self.unparsed, len)
    }

    /// Creates a new mark at the current unparsed position.
    pub fn add_mark(&self) -> Mark {
        self.add_mark_at(self.unparsed)
    }

    /// Creates a new mark at an arbitrary valid position.
    ///
    /// `pos` must be in `[start, unparsed]`: the data at `pos` must still be in
    /// the buffer, and must already have been parsed out of it. See the
    /// invariant documented on [`MarkBuf`]; this is the sole chokepoint for
    /// creating marks, so asserting here enforces it for all of them.
    pub fn add_mark_at(&self, pos: u64) -> Mark {
        assert!(
            pos >= self.ring.start() && pos <= self.unparsed,
            "add_mark_at({pos}): must be within [{}, {}]",
            self.ring.start(),
            self.unparsed,
        );
        self.registry.add(pos);
        Mark {
            pos,
            registry: Arc::clone(&self.registry),
        }
    }

    /// Ring buffer capacity (always a power of two).
    #[inline]
    pub fn capacity(&self) -> usize {
        self.ring.capacity()
    }

    /// Returns data at `[pos, pos+len)` as a [`Split`].
    ///
    /// # Panics
    ///
    /// Panics if the range is outside valid buffer bounds.
    #[inline]
    pub fn split(&self, pos: u64, len: usize) -> Split<'_> {
        self.ring.split(pos, len)
    }

    /// Advances the unparsed position.
    ///
    /// Does **not** reclaim ring buffer space; that happens lazily in
    /// [`spare_capacity`](Self::spare_capacity). This ensures that data
    /// between the old and new unparsed positions (e.g. message bodies)
    /// remains accessible until the next buffer fill.
    ///
    /// # Panics
    ///
    /// Panics unless `unparsed <= new_unparsed <= end`.
    pub fn advance_unparsed(&mut self, new_unparsed: u64) {
        assert!(
            self.unparsed <= new_unparsed && new_unparsed <= self.ring.end(),
            "advance_unparsed({new_unparsed}): must be within [{}, {}]",
            self.unparsed,
            self.ring.end(),
        );
        self.unparsed = new_unparsed;
    }

    /// Returns the free space as up to two mutable slices.
    ///
    /// Ensures at least `reserve` bytes of free space (reclaiming behind
    /// the oldest mark/unparsed position and growing if needed). After
    /// writing into these slices, call [`advance_end`](Self::advance_end).
    pub fn spare_capacity(&mut self, reserve: usize) -> (&mut [u8], &mut [u8]) {
        self.reclaim();
        self.ring.spare_capacity(reserve)
    }

    /// Marks `n` additional bytes at the end as valid.
    ///
    /// Call after writing into slices returned by [`spare_capacity`](Self::spare_capacity).
    pub fn advance_end(&mut self, n: usize) {
        self.ring.advance_end(n);
    }

    /// Reclaims space behind the oldest mark (or unparsed position).
    ///
    /// Relies on the [`MarkBuf`] invariant that no mark is beyond `unparsed`,
    /// so `floor <= unparsed` and this never discards unparsed data.
    fn reclaim(&mut self) {
        let floor = self.registry.earliest().unwrap_or(self.unparsed);
        debug_assert!(floor <= self.unparsed);
        self.ring.advance_to(floor);
    }

    /// Appends data directly to the buffer.
    #[cfg(test)]
    pub fn extend(&mut self, data: &[u8]) {
        self.reclaim();
        self.ring.extend(data);
    }
}

/// An RTP packet referencing payload data in a [`MarkBuf`].
///
/// Passed to [`Depacketizer::push`](crate::codec::Depacketizer::push),
/// bundling packet metadata with buffer access.
pub(crate) struct PacketRef<'a> {
    pub meta: crate::rtp::PacketMeta,
    buf: &'a MarkBuf,
    payload_pos: u64,
    payload_len: u16,
}

impl<'a> PacketRef<'a> {
    /// Creates a `PacketRef`.
    pub fn new(
        meta: crate::rtp::PacketMeta,
        buf: &'a MarkBuf,
        payload_pos: u64,
        payload_len: u16,
    ) -> Self {
        Self {
            meta,
            buf,
            payload_pos,
            payload_len,
        }
    }

    /// Pin the payload position so the data stays accessible until the
    /// returned [`Mark`] is dropped.
    pub fn mark(&self) -> Mark {
        self.buf.add_mark_at(self.payload_pos)
    }

    /// Ring-buffer position of the first payload byte.
    #[inline]
    pub fn payload_pos(&self) -> u64 {
        self.payload_pos
    }

    /// Payload length in bytes.
    #[inline]
    pub fn payload_len(&self) -> u16 {
        self.payload_len
    }

    /// Returns the payload data as a [`Split`].
    #[inline]
    pub fn payload(&self) -> Split<'_> {
        self.buf
            .split(self.payload_pos, usize::from(self.payload_len))
    }

    /// The underlying [`MarkBuf`], for internal helper functions that
    /// need arbitrary buffer access (e.g. reading accumulated NAL data).
    #[inline]
    pub fn buf(&self) -> &MarkBuf {
        self.buf
    }
}

