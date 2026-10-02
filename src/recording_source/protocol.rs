//! Offline-testable experimental framing. No sockets, authentication or decryption.
use super::{MediaChunk, SourceError};
use std::{collections::BTreeMap, fmt};

pub const MAX_HEADER: usize = 8192;
pub const MAX_PART: usize = MediaChunk::MAX_BYTES;
const MAX_BUFFER: usize = MAX_HEADER + MAX_PART + 128;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartKind {
    Json,
    MpegTs,
}
pub struct Part {
    pub kind: PartKind,
    pub encrypted: bool,
    session: Option<u64>,
    sequence: Option<u64>,
    body: Vec<u8>,
}
impl fmt::Debug for Part {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Part(<redacted>)")
    }
}
/// Strict observed subset, not a claim of universal camera compatibility.
pub struct MultipartDecoder {
    marker: Vec<u8>,
    buffer: Vec<u8>,
    closed: bool,
    failed: bool,
}
impl MultipartDecoder {
    /// Boundary token excludes the leading -- used on the wire.
    pub fn new(boundary: &str) -> Result<Self, SourceError> {
        if boundary.is_empty()
            || boundary.len() > 70
            || !boundary
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
        {
            return Err(SourceError::InvalidRequest);
        }
        Ok(Self {
            marker: format!("--{boundary}").into_bytes(),
            buffer: Vec::new(),
            closed: false,
            failed: false,
        })
    }
    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Part>, SourceError> {
        if self.failed {
            return Err(SourceError::Protocol);
        }
        let result = self.feed_inner(bytes);
        if result.is_err() {
            self.failed = true;
            self.buffer.clear();
        }
        result
    }
    fn feed_inner(&mut self, bytes: &[u8]) -> Result<Vec<Part>, SourceError> {
        if self.closed {
            return if bytes.is_empty() {
                Ok(vec![])
            } else {
                Err(SourceError::Protocol)
            };
        }
        if bytes.len() > MAX_BUFFER.saturating_sub(self.buffer.len()) {
            return Err(SourceError::ResourceLimit);
        }
        self.buffer.extend_from_slice(bytes);
        let mut parts = Vec::new();
        loop {
            if self.buffer.is_empty() {
                break;
            }
            let common = self.buffer.len().min(self.marker.len());
            if self.buffer[..common] != self.marker[..common] {
                return Err(SourceError::Protocol);
            }
            let prefix = self.marker.len() + 2;
            if self.buffer.len() < prefix {
                break;
            }
            let suffix = &self.buffer[self.marker.len()..prefix];
            if suffix == b"--" {
                if self.buffer.len() < prefix + 2 {
                    break;
                }
                if &self.buffer[prefix..] != b"\r\n" {
                    return Err(SourceError::Protocol);
                }
                self.buffer.clear();
                self.closed = true;
                break;
            }
            if suffix != b"\r\n" {
                return Err(SourceError::Protocol);
            }
            let Some(end) = self.buffer[prefix..]
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
            else {
                if self.buffer.len() - prefix > MAX_HEADER {
                    return Err(SourceError::ResourceLimit);
                }
                break;
            };
            if end > MAX_HEADER {
                return Err(SourceError::ResourceLimit);
            }
            let headers = std::str::from_utf8(&self.buffer[prefix..prefix + end])
                .map_err(|_| SourceError::Protocol)?;
            let mut fields = BTreeMap::new();
            for line in headers.split("\r\n") {
                let (name, value) = line.split_once(':').ok_or(SourceError::Protocol)?;
                if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                {
                    return Err(SourceError::Protocol);
                }
                let value = value.trim();
                if value.bytes().any(|b| b.is_ascii_control())
                    || fields.insert(name.to_ascii_lowercase(), value).is_some()
                {
                    return Err(SourceError::Protocol);
                }
            }
            let number = |key: &str| -> Result<Option<u64>, SourceError> {
                fields
                    .get(key)
                    .map(|s| {
                        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                            return Err(SourceError::Protocol);
                        }
                        s.parse().map_err(|_| SourceError::Protocol)
                    })
                    .transpose()
            };
            let length = number("content-length")?.ok_or(SourceError::Protocol)?;
            if length == 0 {
                return Err(SourceError::Protocol);
            }
            if length > MAX_PART as u64 {
                return Err(SourceError::ResourceLimit);
            }
            let encrypted = match number("x-if-encrypt")? {
                Some(0) => false,
                Some(1) => true,
                _ => return Err(SourceError::Protocol),
            };
            let kind = match fields.get("content-type").copied() {
                Some("application/json") => PartKind::Json,
                Some("video/mp2t") => PartKind::MpegTs,
                _ => return Err(SourceError::Protocol),
            };
            let session = number("x-session-id")?;
            let sequence = number("x-data-sequence")?;
            let body_start = prefix + end + 4;
            let body_end = body_start + length as usize;
            if self.buffer.len() < body_end + 2 {
                break;
            }
            if &self.buffer[body_end..body_end + 2] != b"\r\n" {
                return Err(SourceError::Protocol);
            }
            parts.push(Part {
                kind,
                encrypted,
                session,
                sequence,
                body: self.buffer[body_start..body_end].to_vec(),
            });
            self.buffer.drain(..body_end + 2);
        }
        Ok(parts)
    }
    /// Transport EOF is never equivalent to a camera's finished notification.
    pub fn finish(&self) -> Result<(), SourceError> {
        if !self.failed && self.buffer.is_empty() {
            Ok(())
        } else {
            Err(SourceError::Protocol)
        }
    }
}
#[derive(Debug)]
pub enum StreamEvent {
    Opened,
    Media(MediaChunk),
    Complete,
}
pub struct StreamRouter {
    request: u64,
    session: Option<u64>,
    last_sequence: Option<u64>,
    complete: bool,
    failed: bool,
}
impl StreamRouter {
    pub fn new(request: u64) -> Self {
        Self {
            request,
            session: None,
            last_sequence: None,
            complete: false,
            failed: false,
        }
    }
    pub fn accept(&mut self, part: Part) -> Result<StreamEvent, SourceError> {
        if self.failed || self.complete {
            return Err(SourceError::Protocol);
        }
        let result = self.accept_inner(part);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn accept_inner(&mut self, part: Part) -> Result<StreamEvent, SourceError> {
        // Encrypted data never reaches a plaintext parser without a future validated decryptor.
        if part.encrypted {
            return Err(SourceError::Decryption);
        }
        if part.kind == PartKind::MpegTs {
            if self.session.is_none() || part.session != self.session {
                return Err(SourceError::Protocol);
            }
            let seq = part.sequence.ok_or(SourceError::Protocol)?;
            if self
                .last_sequence
                .is_some_and(|last| last.checked_add(1) != Some(seq))
            {
                return Err(SourceError::Protocol);
            }
            self.last_sequence = Some(seq);
            return Ok(StreamEvent::Media(MediaChunk::new(part.body)?));
        }
        #[derive(serde::Deserialize)]
        struct Envelope {
            #[serde(rename = "type")]
            kind: String,
            seq: Option<u64>,
            params: Parameters,
        }
        #[derive(serde::Deserialize)]
        struct Parameters {
            error_code: Option<i64>,
            session_id: Option<u64>,
            event_type: Option<String>,
            status: Option<String>,
        }
        let value: Envelope =
            serde_json::from_slice(&part.body).map_err(|_| SourceError::Protocol)?;
        let params = value.params;
        match value.kind.as_str() {
            "response" if self.session.is_none() => {
                if value.seq != Some(self.request) || params.error_code != Some(0) {
                    return Err(SourceError::Protocol);
                }
                let session = params.session_id.ok_or(SourceError::Protocol)?;
                if part.session.is_some_and(|s| s != session) {
                    return Err(SourceError::Protocol);
                }
                self.session = Some(session);
                Ok(StreamEvent::Opened)
            }
            "notification" if self.session.is_some() => {
                let session = params.session_id.or(part.session);
                if session != self.session
                    || part.session.is_some_and(|s| Some(s) != session)
                    || params.event_type.as_deref() != Some("stream_status")
                    || params.status.as_deref() != Some("finished")
                {
                    return Err(SourceError::Protocol);
                }
                self.complete = true;
                Ok(StreamEvent::Complete)
            }
            _ => Err(SourceError::Protocol),
        }
    }

    pub fn finish(&self) -> Result<(), SourceError> {
        if self.complete && !self.failed {
            Ok(())
        } else {
            Err(SourceError::Protocol)
        }
    }
}
