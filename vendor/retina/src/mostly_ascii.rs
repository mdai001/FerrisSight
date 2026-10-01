// Copyright (C) The Retina Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Formatter for byte data that is expected to be mostly ASCII.

use std::fmt;

/// Holds something that's supposed to be mostly ASCII.
///
/// Some SDP values are allowed to be UTF-8-encoded ISO-10646 characters, but mostly it's
/// supposed to be valid ASCII. This has a `Debug` impl that is fairly readable for ASCII
/// characters and can be copy'n'pasted into Python's `codecs.decode(..., 'string_escape'`)`
/// or the like.
pub(crate) struct MostlyAscii<'a> {
    pub bytes: &'a [u8],
    pub escape_newline: bool,
}

impl<'a> MostlyAscii<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            escape_newline: false,
        }
    }
}

impl fmt::Debug for MostlyAscii<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"")?;
        for &b in self.bytes {
            match b {
                b'"' => write!(f, "\"")?,
                b'\r' => write!(f, "\\r")?,
                b'\n' if self.escape_newline => write!(f, "\\n")?,
                // Note: when escape_newline is false, newlines are written in unescaped form.
                b' ' | b'\n' | 0x20..=0x7E => write!(f, "{}", b as char)?,
                _ => write!(f, "\\x{b:02X}")?,
            }
        }
        write!(f, "\"")?;
        Ok(())
    }
}

