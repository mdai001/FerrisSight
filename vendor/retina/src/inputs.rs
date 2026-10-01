// Copyright (C) The Retina Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Input abstractions for parsing.
//!
//! Two [`Input`] implementations are provided:
//!
//! * `&[u8]` — contiguous byte slice.
//! * [`Split`] — two `&[u8]` halves, for discontiguous ring-buffer views.

use std::borrow::Cow;

use derive_more::Debug;

use crate::mostly_ascii::MostlyAscii;

/// Byte input, usable as a checkpoint (since `Copy`).
///
/// This is a pure view over bytes with no streaming semantics.
/// The caller decides whether incomplete parses are retriable.
pub trait Input<'i>: Copy {
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the next byte without consuming it.
    fn peek_byte(&self) -> Option<u8>;

    /// Skips `n` bytes. Panics if `n > self.len()`.
    fn advance(&mut self, n: usize);

    /// Consumes and returns the next `n` bytes. Panics if `n > self.len()`.
    fn next_slice(&mut self, n: usize) -> Self;

    /// Returns byte at index `i`. Panics if `i >= self.len()`.
    fn byte_at(&self, i: usize) -> u8;

    /// Returns true iff the next `lit.len()` bytes equal `lit`.
    /// Panics if `self.len() < lit.len()`.
    fn starts_with_lit(&self, lit: &[u8]) -> bool;

    /// Returns the offset of the first occurrence of `b`, or `None` if absent.
    fn find_byte(&self, b: u8) -> Option<usize>;

    /// Returns the offset of the first occurrence of `a` or `b`, or `None` if absent.
    fn find_bytes2(&self, a: u8, b: u8) -> Option<usize>;

    /// Returns the offset of the first occurrence of `a`, `b`, or `c`, or `None` if absent.
    fn find_bytes3(&self, a: u8, b: u8, c: u8) -> Option<usize>;

    /// Returns the offset of the first byte satisfying `pred`, or `None` if none do.
    fn find_first<F: Fn(u8) -> bool>(&self, pred: F) -> Option<usize>;

    /// Copies the first `N` bytes into an array. Panics if `self.len() < N`.
    fn peek_array<const N: usize>(&self) -> [u8; N];

    /// Converts to a `Cow<[u8]>`, borrowing if contiguous.
    fn to_cow(self) -> Cow<'i, [u8]>;

    /// Converts to a `Cow<str>`, borrowing if contiguous and valid UTF-8.
    fn to_cow_str(self) -> Result<Cow<'i, str>, std::str::Utf8Error> {
        match self.to_cow() {
            Cow::Borrowed(b) => std::str::from_utf8(b).map(Cow::Borrowed),
            Cow::Owned(b) => String::from_utf8(b)
                .map(Cow::Owned)
                .map_err(|e| e.utf8_error()),
        }
    }
    fn to_owned(self) -> Vec<u8>;
}

// ---------------------------------------------------------------------------
// &[u8] — contiguous byte slice
// ---------------------------------------------------------------------------

impl<'i> Input<'i> for &'i [u8] {
    fn len(&self) -> usize {
        <[u8]>::len(self)
    }

    fn peek_byte(&self) -> Option<u8> {
        self.first().copied()
    }

    fn advance(&mut self, n: usize) {
        *self = &self[n..];
    }

    fn next_slice(&mut self, n: usize) -> &'i [u8] {
        let (ret, rest) = self.split_at(n);
        *self = rest;
        ret
    }

    fn byte_at(&self, i: usize) -> u8 {
        self[i]
    }

    fn starts_with_lit(&self, lit: &[u8]) -> bool {
        self.starts_with(lit)
    }

    fn find_byte(&self, b: u8) -> Option<usize> {
        memchr::memchr(b, self)
    }

    fn find_bytes2(&self, a: u8, b: u8) -> Option<usize> {
        memchr::memchr2(a, b, self)
    }

    fn find_bytes3(&self, a: u8, b: u8, c: u8) -> Option<usize> {
        memchr::memchr3(a, b, c, self)
    }

    fn find_first<F: Fn(u8) -> bool>(&self, pred: F) -> Option<usize> {
        self.iter().position(|&b| pred(b))
    }

    fn peek_array<const N: usize>(&self) -> [u8; N] {
        self[..N].try_into().unwrap()
    }

    fn to_cow(self) -> Cow<'i, [u8]> {
        Cow::Borrowed(self)
    }

    fn to_owned(self) -> Vec<u8> {
        Vec::from(self)
    }
}

// ---------------------------------------------------------------------------
// Split — discontiguous two-slice input (for ring buffers)
// ---------------------------------------------------------------------------

/// Discontiguous input from two slices, as from a ring buffer.
#[derive(Copy, Clone, Debug)]
#[debug(
    "{first:?}<split>{second:?}",
    first = MostlyAscii { bytes: self.first, escape_newline: true },
    second = MostlyAscii { bytes: self.second, escape_newline: true }
)]
pub struct Split<'i> {
    first: &'i [u8],
    second: &'i [u8], // empty if first is empty.
}

impl<'i> Split<'i> {
    pub fn new(first: &'i [u8], second: &'i [u8]) -> Self {
        if first.is_empty() {
            Self {
                first: second,
                second: &[],
            }
        } else {
            Self { first, second }
        }
    }

    /// Returns the two underlying slices.
    ///
    /// Useful when raw slice access is needed (e.g. for vectored I/O or
    /// copying into a contiguous buffer).
    #[inline]
    pub fn slices(&self) -> (&'i [u8], &'i [u8]) {
        (self.first, self.second)
    }
}

impl<'i> Input<'i> for Split<'i> {
    fn len(&self) -> usize {
        self.first.len() + self.second.len()
    }

    fn is_empty(&self) -> bool {
        self.first.is_empty()
    }

    fn peek_byte(&self) -> Option<u8> {
        self.first.first().copied()
    }

    fn advance(&mut self, n: usize) {
        if let Some(beyond_first) = n.checked_sub(self.first.len()) {
            self.first = &std::mem::take(&mut self.second)[beyond_first..];
        } else {
            self.first = &self.first[n..];
        }
    }

    fn next_slice(&mut self, offset: usize) -> Self {
        if let Some(beyond_first) = offset.checked_sub(self.first.len()) {
            let ret = Split {
                first: self.first,
                second: &self.second[..beyond_first],
            };
            self.first = &std::mem::take(&mut self.second)[beyond_first..];
            ret
        } else {
            let (ret, rest) = self.first.split_at(offset);
            self.first = rest;
            Split {
                first: ret,
                second: &[],
            }
        }
    }

    fn byte_at(&self, i: usize) -> u8 {
        if let Some(beyond_first) = i.checked_sub(self.first.len()) {
            self.second[beyond_first]
        } else {
            self.first[i]
        }
    }

    fn starts_with_lit(&self, lit: &[u8]) -> bool {
        debug_assert!(self.first.len() + self.second.len() >= lit.len());
        self.first
            .iter()
            .chain(self.second.iter())
            .zip(lit.iter())
            .all(|(a, b)| a == b)
    }

    fn find_byte(&self, b: u8) -> Option<usize> {
        memchr::memchr(b, self.first)
            .or_else(|| memchr::memchr(b, self.second).map(|o| o + self.first.len()))
    }

    fn find_bytes2(&self, a: u8, b: u8) -> Option<usize> {
        memchr::memchr2(a, b, self.first)
            .or_else(|| memchr::memchr2(a, b, self.second).map(|o| o + self.first.len()))
    }

    fn find_bytes3(&self, a: u8, b: u8, c: u8) -> Option<usize> {
        memchr::memchr3(a, b, c, self.first)
            .or_else(|| memchr::memchr3(a, b, c, self.second).map(|o| o + self.first.len()))
    }

    fn find_first<F: Fn(u8) -> bool>(&self, pred: F) -> Option<usize> {
        self.first.iter().position(|&b| pred(b)).or_else(|| {
            self.second
                .iter()
                .position(|&b| pred(b))
                .map(|i| i + self.first.len())
        })
    }

    fn peek_array<const N: usize>(&self) -> [u8; N] {
        let mut arr = [0u8; N];
        if N <= self.first.len() {
            arr.copy_from_slice(&self.first[..N]);
        } else {
            let (a, b) = arr.split_at_mut(self.first.len());
            a.copy_from_slice(self.first);
            b.copy_from_slice(&self.second[..N - self.first.len()]);
        }
        arr
    }

    fn to_cow(self) -> Cow<'i, [u8]> {
        if self.second.is_empty() {
            Cow::Borrowed(self.first)
        } else {
            let mut v = Vec::with_capacity(self.first.len() + self.second.len());
            v.extend_from_slice(self.first);
            v.extend_from_slice(self.second);
            Cow::Owned(v)
        }
    }

    fn to_owned(self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.first.len() + self.second.len());
        v.extend_from_slice(self.first);
        v.extend_from_slice(self.second);
        v
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

