// Copyright (C) The Retina Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

use bytes::Bytes;
use log::{debug, warn};
use sdp_types::Media;
use std::{net::IpAddr, num::NonZeroU16};
use url::Url;

use super::{Presentation, Stream};

/// A static payload type in the [RTP parameters
/// registry](https://www.iana.org/assignments/rtp-parameters/rtp-parameters.xhtml#rtp-parameters-1).
#[derive(Debug)]
struct StaticPayloadType {
    encoding: &'static str,
    media: &'static str,
    clock_rate: u32,
    channels: Option<NonZeroU16>,
}

/// All registered static payload types.
/// The registry is officially closed, so this list should never change.
#[rustfmt::skip]
static STATIC_PAYLOAD_TYPES: [Option<StaticPayloadType>; 35] = [
    /* 0 */ Some(StaticPayloadType {
        encoding: "pcmu",
        media: "audio",
        clock_rate: 8_000,
        channels: NonZeroU16::new(1),
    }),
    /* 1 */ None, // reserved
    /* 2 */ None, // reserved
    /* 3 */ Some(StaticPayloadType {
        encoding: "gsm",
        media: "audio",
        clock_rate: 8_000,
        channels: NonZeroU16::new(1),
    }),
    /* 4 */ Some(StaticPayloadType {
        encoding: "g723",
        media: "audio",
        clock_rate: 8_000,
        channels: NonZeroU16::new(1),
    }),
    /* 5 */ Some(StaticPayloadType {
        encoding: "dvi4",
        media: "audio",
        clock_rate: 8_000,
        channels: NonZeroU16::new(1),
    }),
    /* 6 */ Some(StaticPayloadType {
        encoding: "dvi4",
        media: "audio",
        clock_rate: 16_000,
        channels: NonZeroU16::new(1),
    }),
    /* 7 */ Some(StaticPayloadType {
        encoding: "lpc",
        media: "audio",
        clock_rate: 8_000,
        channels: NonZeroU16::new(1),
    }),
    /* 8 */ Some(StaticPayloadType {
        encoding: "pcma",
        media: "audio",
        clock_rate: 8_000,
        channels: NonZeroU16::new(1),
    }),
    /* 9 */ Some(StaticPayloadType {
        encoding: "g722",
        media: "audio",
        clock_rate: 8_000,
        channels: NonZeroU16::new(1),
    }),
    /* 10 */ Some(StaticPayloadType {
        encoding: "l16",
        media: "audio",
        clock_rate: 441_000,
        channels: NonZeroU16::new(2),
    }),
    /* 11 */ Some(StaticPayloadType {
        encoding: "l16",
        media: "audio",
        clock_rate: 441_000,
        channels: NonZeroU16::new(1),
    }),
    /* 12 */ Some(StaticPayloadType {
        encoding: "qcelp",
        media: "audio",
        clock_rate: 8_000,
        channels: NonZeroU16::new(1),
    }),
    /* 13 */ Some(StaticPayloadType {
        encoding: "cn",
        media: "audio",
        clock_rate: 8_000,
        channels: NonZeroU16::new(1),
    }),
    /* 14 */ Some(StaticPayloadType {
        encoding: "mpa",
        media: "audio",
        clock_rate: 90_000,
        channels: None,
    }),
    /* 15 */ Some(StaticPayloadType {
        encoding: "g728",
        media: "audio",
        clock_rate: 8_000,
        channels: NonZeroU16::new(1),
    }),
    /* 16 */ Some(StaticPayloadType {
        encoding: "dvi4",
        media: "audio",
        clock_rate: 11_025,
        channels: NonZeroU16::new(1),
    }),
    /* 17 */ Some(StaticPayloadType {
        encoding: "dvi4",
        media: "audio",
        clock_rate: 22_050,
        channels: NonZeroU16::new(1),
    }),
    /* 18 */ Some(StaticPayloadType {
        encoding: "g729",
        media: "audio",
        clock_rate: 8_000,
        channels: NonZeroU16::new(1),
    }),
    /* 19 */ None, // reserved
    /* 20 */ None, // unassigned
    /* 21 */ None, // unassigned
    /* 22 */ None, // unassigned
    /* 23 */ None, // unassigned
    /* 24 */ None, // unassigned
    /* 25 */ Some(StaticPayloadType {
        encoding: "celb",
        media: "video",
        clock_rate: 90_000,
        channels: None,
    }),
    /* 26 */ Some(StaticPayloadType {
        encoding: "jpeg",
        media: "video",
        clock_rate: 90_000,
        channels: None,
    }),
    /* 27 */ None, // unassigned
    /* 28 */ Some(StaticPayloadType {
        encoding: "nv",
        media: "video",
        clock_rate: 90_000,
        channels: None,
    }),
    /* 29 */ None, // unassigned
    /* 30 */ None, // unassigned
    /* 31 */ Some(StaticPayloadType {
        encoding: "h261",
        media: "video",
        clock_rate: 90_000,
        channels: None,
    }),
    /* 32 */ Some(StaticPayloadType {
        encoding: "mpv",
        media: "video",
        clock_rate: 90_000,
        channels: None,
    }),
    /* 33 */ Some(StaticPayloadType {
        encoding: "mp2t",
        // The RTP parameters registry says type AV (audio and video).
        // The MIME registration says the media type is "video".
        // https://datatracker.ietf.org/doc/html/rfc3555#section-4.2.9
        media: "video",
        clock_rate: 90_000,
        channels: None,
    }),
    /* 34 */ Some(StaticPayloadType {
        encoding: "h263",
        media: "video",
        clock_rate: 90_000,
        channels: None,
    }),
];

/// Joins a control URL to a base URL in a non-RFC-compliant but common way.
/// This matches what live555 and ffmpeg do.
///
/// See discussion at [#9](https://github.com/scottlamb/retina/issues/9).
fn join_control(base_url: &Url, control: &str) -> Result<Url, String> {
    if control == "*" {
        return Ok(base_url.clone());
    }
    if let Ok(absolute_url) = Url::parse(control) {
        return Ok(absolute_url);
    }

    Url::parse(&format!(
        "{}{}{}",
        base_url.as_str(),
        if base_url.as_str().ends_with('/') {
            ""
        } else {
            "/"
        },
        control
    ))
    .map_err(|e| format!("unable to join base url {base_url} with control url {control:?}: {e}"))
}

/// Returns the `CSeq` from an RTSP response as a `u32`, or `None` if missing/unparseable.
pub(crate) fn get_cseq(response: &crate::rtsp::msg::Response) -> Option<u32> {
    response
        .headers
        .get("CSeq")
        .and_then(|cseq| u32::from_str_radix(cseq, 10).ok())
}

/// Parses a [MediaDescription] to a [Stream].
/// On failure, returns an error which is expected to be supplemented with
/// the [MediaDescription] debug string and packed into a `RtspResponseError`.
fn parse_media(base_url: &Url, media_description: &Media) -> Result<Stream, String> {
    let media = media_description.media.clone().into_boxed_str();

    // https://tools.ietf.org/html/rfc8866#section-5.14 says "If the <proto>
    // sub-field is "RTP/AVP" or "RTP/SAVP" the <fmt> sub-fields contain RTP
    // payload type numbers."
    // https://www.iana.org/assignments/sdp-parameters/sdp-parameters.xhtml#sdp-parameters-2
    // shows several other variants, such as "TCP/RTP/AVP". Looking for a "RTP" component
    // seems appropriate.
    // https://www.ietf.org/archive/id/draft-sheedy-mmusic-rtsp-ext-01.txt
    // adds "MP2T" as a valid proto which is used by some ISPs.
    if !media_description.proto.starts_with("RTP/")
        && !media_description.proto.contains("/RTP/")
        && !media_description.proto.contains("MP2T/")
    {
        return Err("Expected RTP-based proto".into());
    }

    // RFC 8866 continues: "When a list of payload type numbers is given,
    // this implies that all of these payload formats MAY be used in the
    // session, but the first of these formats SHOULD be used as the default
    // format for the session." Just use the first until we find a stream
    // where this isn't the right thing to do.
    let rtp_payload_type_str = media_description
        .fmt
        .split_ascii_whitespace()
        .next()
        .unwrap();
    let rtp_payload_type = u8::from_str_radix(rtp_payload_type_str, 10)
        .map_err(|_| format!("invalid RTP payload type {rtp_payload_type_str:?}"))?;
    if (rtp_payload_type & 0x80) != 0 {
        return Err(format!("invalid RTP payload type {rtp_payload_type}"));
    }

    // Capture interesting attributes.
    // RFC 8866: "For dynamic payload type assignments, the "a=rtpmap:"
    // attribute (see Section 6.6) SHOULD be used to map from an RTP payload
    // type number to a media encoding name that identifies the payload
    // format. The "a=fmtp:" attribute MAY be used to specify format
    // parameters (see Section 6.15)."
    let mut rtpmap = None;
    let mut fmtp = None;
    let mut control = None;
    let mut framerate = None;
    for a in &media_description.attributes {
        match a.attribute.as_str() {
            "rtpmap" => {
                let v = a
                    .value
                    .as_ref()
                    .ok_or_else(|| "rtpmap attribute with no value".to_string())?
                    .trim_end_matches(' ');
                // https://tools.ietf.org/html/rfc8866#section-6.6
                // rtpmap-value = payload-type SP encoding-name
                //   "/" clock-rate [ "/" encoding-params ]
                // payload-type = zero-based-integer
                // encoding-name = token
                // clock-rate = integer
                // encoding-params = channels
                // channels = integer
                //
                // At least one camera (improperly) sends a trailing space; trim this.
                let (rtpmap_payload_type, v) = v
                    .split_once(' ')
                    .ok_or_else(|| "invalid rtmap attribute".to_string())?;
                if rtpmap_payload_type == rtp_payload_type_str {
                    rtpmap = Some(v);
                }
            }
            "fmtp" => {
                // Similarly should start with payload-type SP.
                let v = a
                    .value
                    .as_ref()
                    .ok_or_else(|| "fmtp attribute with no value".to_string())?;

                if let Some((fmtp_payload_type, v)) = v.split_once(' ') {
                    if fmtp_payload_type == rtp_payload_type_str {
                        fmtp = Some(v);
                    }
                } else {
                    // Ubiquiti cameras sometimes have e.g. "a=fmtp:96": payload
                    // type only, no actual attributes. Don't fail on this.
                    warn!("ignoring invalid fmtp attribute value {:?}", v);
                }
            }
            "control" => {
                control = a
                    .value
                    .as_deref()
                    .map(|c| join_control(base_url, c))
                    .transpose()?;
            }
            "framerate" => {
                if let Some(s) = a.value.as_ref()
                    && let Ok(f) = s.parse::<f32>()
                {
                    framerate = Some(f);
                }
            }
            _ => (),
        }
    }

    let encoding_name;
    let clock_rate;
    let channels;
    match rtpmap {
        Some(rtpmap) => {
            let (e, rtpmap) = rtpmap
                .split_once('/')
                .ok_or_else(|| "invalid rtpmap attribute".to_string())?;
            encoding_name = e;
            let (clock_rate_str, channels_str) = match rtpmap.find('/') {
                None => (rtpmap, None),
                Some(i) => (&rtpmap[..i], Some(&rtpmap[i + 1..])),
            };
            clock_rate = u32::from_str_radix(clock_rate_str, 10)
                .map_err(|_| "bad clockrate in rtpmap".to_string())?;
            channels = channels_str
                .map(|c| {
                    u16::from_str_radix(c, 10)
                        .ok()
                        .and_then(NonZeroU16::new)
                        .ok_or_else(|| format!("Invalid channels specification {c:?}"))
                })
                .transpose()?;
        }
        None => {
            let type_ = STATIC_PAYLOAD_TYPES
                .get(usize::from(rtp_payload_type))
                .and_then(Option::as_ref)
                .ok_or_else(|| {
                    format!(
                        "Expected rtpmap parameter or assigned static payload type (got {rtp_payload_type})"
                    )
                })?;
            encoding_name = type_.encoding;
            clock_rate = type_.clock_rate;
            channels = type_.channels;
            if type_.media != &*media {
                return Err(format!(
                    "SDP media type {} must match RTP payload type {:#?}",
                    media, type_
                ));
            }
        }
    }

    let encoding_name = encoding_name.to_ascii_lowercase().into_boxed_str();
    let depacketizer =
        crate::codec::Depacketizer::new(&media, &encoding_name, clock_rate, channels, fmtp);

    Ok(Stream {
        media,
        encoding_name,
        clock_rate_hz: clock_rate,
        rtp_payload_type,
        depacketizer,
        control,
        channels,
        framerate,
        state: super::StreamState::Uninit,
    })
}

use crate::mostly_ascii::MostlyAscii;

/// Parses a successful RTSP `DESCRIBE` response into a [Presentation].
/// On error, returns a string which is expected to be packed into an `RtspProtocolError`.
pub(crate) fn parse_describe(
    request_url: Url,
    response: &crate::rtsp::msg::Response,
    body: &Bytes,
) -> Result<Presentation, String> {
    match response.headers.get("Content-Type") {
        Some(v) if &**v == "application/sdp" => {}
        Some(v) => {
            return Err(format!(
                "DESCRIBE response at {} has unexpected content type {}",
                request_url.as_str(),
                v,
            ));
        }
        None => {
            warn!(
                "DESCRIBE response at {} has no content type; trying sdp anyway",
                request_url.as_str()
            );
        }
    }
    let raw_sdp = MostlyAscii::new(&body[..]);
    let sdp = sdp_types::Session::parse(raw_sdp.bytes)
        .map_err(|e| format!("Unable to parse SDP: {e}\n\n{raw_sdp:#?}",))?;

    // https://tools.ietf.org/html/rfc2326#appendix-C.1.1
    let base_url = response
        .headers
        .get("Content-Base")
        .map(|v| ("Content-Base", v))
        .or_else(|| {
            response
                .headers
                .get("Content-Location")
                .map(|v| ("Content-Location", v))
        })
        .map(|(h, v)| {
            Url::parse(v).or_else(|_| {
                // Some cameras (e.g. Anjvision) send a Content-Base without a
                // scheme prefix. Try prepending the request URL's scheme.
                // See <https://github.com/scottlamb/moonfire-nvr/issues/356>.
                let fixed = format!("{}://{v}", request_url.scheme());
                let url = Url::parse(&fixed).map_err(|e| format!("bad {h} {v:?}: {e}"))?;
                warn!("repaired schemeless {h} {v:?} to {url}");
                Ok::<_, String>(url)
            })
        })
        .unwrap_or_else(|| Ok(request_url.clone()))?;

    let mut control = None;
    let mut tool = None;
    for a in &sdp.attributes {
        if a.attribute == "control" {
            control = a
                .value
                .as_deref()
                .map(|c| join_control(&base_url, c))
                .transpose()?;
        } else if a.attribute == "tool" {
            tool = a.value.as_deref().map(super::Tool::new);
        }
    }
    let control = control.unwrap_or(request_url);

    let streams: Box<[Stream]> = sdp
        .medias
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            parse_media(&base_url, m).map_or_else(
                |e| {
                    warn!(
                        "Ignoring unparseable stream {}: {}\nraw SDP: {:#?}",
                        i, e, raw_sdp
                    );
                    None
                },
                Some,
            )
        })
        .collect();

    if streams.is_empty() {
        return Err(format!(
            "No parseable streams (and {} unparseable streams)",
            sdp.medias.len()
        ));
    }

    Ok(Presentation {
        streams,
        base_url,
        control,
        tool,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SessionHeader {
    pub(crate) id: Box<str>,
    pub(crate) timeout_sec: u32,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct SetupResponse {
    pub(crate) session: SessionHeader,
    pub(crate) ssrc: Option<u32>,
    pub(crate) channel_id: Option<u8>,
    pub(crate) source: Option<IpAddr>,
    pub(crate) server_port: Option<u16>,
}

fn parse_server_port(server_port: &str) -> Result<u16, ()> {
    if let Some((a, b)) = server_port.split_once('-') {
        let a = u16::from_str_radix(a, 10).map_err(|_| ())?;
        let b = u16::from_str_radix(b, 10).map_err(|_| ())?;
        if a.checked_add(1) != Some(b) {
            // It's unclear what a non-consecutive range means.
            return Err(());
        }
        return Ok(a);
    }

    // Not returning a range is allowed by RFC 2326's grammar, but I'm not sure
    // what it means. RTSP 2.0 allows "RTCP-mux" for using a single port for
    // both RTP and RTCP, but it's only by client request, and RTSP 1.0 doesn't
    // reference this.
    Err(())
}

/// Parses a `SETUP` response.
/// `session_id` is checked for assignment or reassignment.
/// Returns an assigned interleaved channel id (implying the next channel id
/// is also assigned) or errors.
pub(crate) fn parse_setup(response: &crate::rtsp::msg::Response) -> Result<SetupResponse, String> {
    // https://datatracker.ietf.org/doc/html/rfc2326#section-12.37
    let session = response
        .headers
        .get("Session")
        .ok_or_else(|| "Missing Session header".to_string())?;
    let session_str: &str = session;
    let session = match session_str.split_once(';') {
        None => SessionHeader {
            id: session_str.into(),
            timeout_sec: 60, // default
        },
        Some((id, timeout_str)) => {
            if let Some(v) = timeout_str.trim().strip_prefix("timeout=") {
                let timeout_sec =
                    u32::from_str_radix(v, 10).map_err(|_| format!("Unparseable timeout {v}"))?;

                if timeout_sec == 0 {
                    // This would make Retina send keepalives at an absurd rate; reject.
                    return Err(format!(
                        "Invalid timeout=0 in Session header {:?}",
                        session_str
                    ));
                }
                SessionHeader {
                    id: id.into(),
                    timeout_sec,
                }
            } else {
                return Err(format!("Unparseable Session header {:?}", session_str));
            }
        }
    };
    let transport = response
        .headers
        .get("Transport")
        .ok_or_else(|| "Missing Transport header".to_string())?;
    let mut channel_id = None;
    let mut ssrc = None;
    let mut source = None;
    let mut server_port = None;
    let transport_str: &str = transport;
    for part in transport_str.split(';') {
        if let Some(v) = part.strip_prefix("ssrc=") {
            let v = v.trim();
            let v = u32::from_str_radix(v, 16).map_err(|_| format!("Unparseable ssrc {v}"))?;
            ssrc = Some(v);
        } else if let Some(interleaved) = part.strip_prefix("interleaved=") {
            let mut channels = interleaved.splitn(2, '-');
            let n = channels.next().expect("splitn returns at least one part");
            let n = u8::from_str_radix(n, 10).map_err(|_| format!("bad channel number {n}"))?;
            if let Some(m) = channels.next() {
                let m = u8::from_str_radix(m, 10)
                    .map_err(|_| format!("bad second channel number {m}"))?;
                if n.checked_add(1) != Some(m) {
                    return Err(format!("Expected adjacent channels; got {n}-{m}"));
                }
            }
            channel_id = Some(n);
        } else if let Some(s) = part.strip_prefix("source=") {
            source = Some(
                s.parse()
                    .map_err(|_| format!("Transport header has unparseable source {s:?}"))?,
            );
        } else if let Some(s) = part.strip_prefix("server_port=") {
            server_port = Some(parse_server_port(s).map_err(|()| {
                format!("Transport header {:?} has bad server_port", &**transport)
            })?);
        }
    }
    Ok(SetupResponse {
        session,
        ssrc,
        channel_id,
        source,
        server_port,
    })
}

/// Parses a `PLAY` response. The error should always be packed into a `RtspProtocolError`.
pub(crate) fn parse_play(
    response: &crate::rtsp::msg::Response,
    presentation: &mut Presentation,
) -> Result<(), String> {
    // https://tools.ietf.org/html/rfc2326#section-12.33
    let rtp_info = match response.headers.get("RTP-Info") {
        Some(rtsp_info) => rtsp_info,
        None => return Ok(()),
    };
    for s in rtp_info.split(',') {
        let s = s.trim();
        let mut parts = s.split(';');
        let url = parts
            .next()
            .expect("split always returns at least one part")
            .strip_prefix("url=")
            .ok_or_else(|| "RTP-Info missing stream URL".to_string())?;
        let url = join_control(&presentation.base_url, url)?;
        let stream = if presentation.streams.len() == 1 {
            // The server is allowed to not specify a stream control URL for
            // single-stream presentations. Additionally, some buggy
            // cameras (eg the GW Security GW4089IP) use an incorrect URL.
            // When there is a single stream in the presentation, there's no
            // ambiguity. Be "forgiving", just as RFC 2326 section 14.3 asks
            // servers to be forgiving of clients with single-stream
            // containers.
            // https://datatracker.ietf.org/doc/html/rfc2326#section-14.3
            Some(&mut presentation.streams[0])
        } else {
            presentation
                .streams
                .iter_mut()
                .find(|s| matches!(&s.control, Some(u) if u == &url))
        };
        let stream = match stream {
            Some(s) => s,
            None => {
                log::warn!("RTP-Info contains unknown stream {}", url);
                continue;
            }
        };
        let state = match &mut stream.state {
            super::StreamState::Uninit => {
                // This appears to happen for Reolink devices when we did not send a SETUP request
                // for all streams. It also happens in some of other the tests
                // here simply because I didn't include all the SETUP steps.
                debug!(
                    "PLAY response described stream {} in Uninit state",
                    stream.control.as_ref().unwrap_or(&presentation.control)
                );
                continue;
            }
            super::StreamState::Init(init) => init,
            super::StreamState::Playing { .. } => unreachable!(),
        };
        for part in parts {
            if part.is_empty() {
                continue;
            }
            let (key, value) = part
                .split_once('=')
                .ok_or_else(|| "RTP-Info param has no =".to_string())?;
            match key {
                "seq" => {
                    let seq =
                        u16::from_str_radix(value, 10).map_err(|_| format!("bad seq {value:?}"))?;
                    state.initial_seq = Some(seq);
                }
                "rtptime" => match u32::from_str_radix(value, 10) {
                    Ok(v) => state.initial_rtptime = Some(v),
                    Err(_) => warn!("Unparseable rtptime in RTP-Info header {:?}", rtp_info),
                },
                "ssrc" => {
                    let value = value.trim();
                    let ssrc = u32::from_str_radix(value, 16)
                        .map_err(|_| format!("Unparseable ssrc {value}"))?;
                    state.ssrc = Some(ssrc);
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[derive(Default)]
pub(crate) struct OptionsResponse {
    pub(crate) set_parameter_supported: bool,
    pub(crate) get_parameter_supported: bool,
}

/// Parses an `OPTIONS` response.
pub(crate) fn parse_options(
    response: &crate::rtsp::msg::Response,
) -> Result<OptionsResponse, String> {
    let mut interpreted = OptionsResponse::default();

    // RTSP/1.0 OPTIONS method: https://tools.ietf.org/html/rfc2326#section-10.1
    // HTTP/1.1 OPTIONS method: https://www.rfc-editor.org/rfc/rfc2616.html#section-9.2
    // RTSP/1.0 Public header: https://www.rfc-editor.org/rfc/rfc2326.html#section-12.28
    // HTTP/1.1 Public header: https://www.rfc-editor.org/rfc/rfc2068#section-14.35
    if let Some(public) = response.headers.get("Public") {
        for method in public.split(',') {
            let method = method.trim();
            match method {
                "SET_PARAMETER" => interpreted.set_parameter_supported = true,
                "GET_PARAMETER" => interpreted.get_parameter_supported = true,
                _ => {}
            }
        }
    }
    Ok(interpreted)
}
