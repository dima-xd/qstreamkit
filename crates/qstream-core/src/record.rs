use crate::token::Endian;

pub const MAGIC: u32 = 0xABCD_0001;

pub const KIND_PROCESS: u32 = 0x0B01_0001;
pub const KIND_DEVICE: u32 = 0x0B01_0002;
pub const KIND_COUNTERS: u32 = 0x0B01_0004;
pub const KIND_MONITOR: u32 = crate::monitor::KIND_MONITOR;
pub const KIND_MONITORS_DONE: u32 = crate::monitor::KIND_MONITORS_DONE;
/// Old name kept for callers that treated 0x0102001C as a scope ack.
pub const KIND_SCOPE_ENABLED: u32 = KIND_MONITORS_DONE;
pub const KIND_STARTED: u32 = 0x0B02_0004;
pub const KIND_STOPPED: u32 = 0x0B02_0005;
pub const KIND_FPS: u32 = 0x0B02_0009;
pub const KIND_FRAME_BATCH: u32 = 0x0B02_000E;
pub const KIND_VERSION: u32 = 0x0B02_0012;
pub const KIND_SUBMIT_TIMING: u32 = 0x0B05_0013;
pub const KIND_SUBMIT_INFO: u32 = 0x0B03_006B;
pub const KIND_FRAME_START: u32 = 0x0102_0004;
pub const KIND_FRAME_END: u32 = 0x0102_0005;

/// One reply record.
///
/// Header: magic, index, body_len. Body: kind, payload_len, arg0, payload.
/// `body_len == 12 + payload_len`. `arg0` carries the pid on most records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record<'a> {
    pub index: u32,
    pub kind: u32,
    pub arg0: u32,
    pub payload: &'a [u8],
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RecordError {
    #[error("bad magic 0x{0:08X} at offset {1}")]
    BadMagic(u32, usize),
    #[error("truncated header at offset {0}")]
    TruncatedHeader(usize),
    #[error("bad body length {len} at offset {off}")]
    BadLength { off: usize, len: u32 },
    #[error("truncated body at offset {0}")]
    TruncatedBody(usize),
    #[error("payload length mismatch at offset {0}")]
    BadPayload(usize),
}

pub fn parse(buf: &[u8], endian: Endian) -> Result<Vec<Record<'_>>, RecordError> {
    let mut out = Vec::new();
    let mut off = 0usize;

    while off < buf.len() {
        if buf.len() - off < 12 {
            return Err(RecordError::TruncatedHeader(off));
        }
        let magic = word(buf, off, endian);
        if magic != MAGIC {
            return Err(RecordError::BadMagic(magic, off));
        }
        let index = word(buf, off + 4, endian);
        let body_len = word(buf, off + 8, endian) as usize;
        let body = off + 12;
        if body_len < 12 || body + body_len > buf.len() {
            return Err(RecordError::BadLength {
                off,
                len: body_len as u32,
            });
        }

        let kind = word(buf, body, endian);
        let plen = word(buf, body + 4, endian) as usize;
        if body_len != 12 + plen {
            return Err(RecordError::BadPayload(off));
        }
        let arg0 = word(buf, body + 8, endian);
        let payload = &buf[body + 12..body + 12 + plen];

        out.push(Record {
            index,
            kind,
            arg0,
            payload,
        });
        off = body + body_len;
    }

    Ok(out)
}

fn word(buf: &[u8], off: usize, endian: Endian) -> u32 {
    endian.from_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

/// Parse as many records as possible and return the unconsumed tail.
pub fn parse_lenient(buf: &[u8], endian: Endian) -> (Vec<Record<'_>>, usize) {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < buf.len() {
        if buf.len() - off < 12 {
            break;
        }
        let magic = word(buf, off, endian);
        if magic != MAGIC {
            break;
        }
        let index = word(buf, off + 4, endian);
        let body_len = word(buf, off + 8, endian) as usize;
        let body = off + 12;
        if body_len < 12 || body + body_len > buf.len() {
            break;
        }
        let kind = word(buf, body, endian);
        let plen = word(buf, body + 4, endian) as usize;
        if body_len != 12 + plen {
            break;
        }
        let arg0 = word(buf, body + 8, endian);
        let payload = &buf[body + 12..body + 12 + plen];
        out.push(Record {
            index,
            kind,
            arg0,
            payload,
        });
        off = body + body_len;
    }
    (out, off)
}

pub fn kind_name(kind: u32) -> &'static str {
    match kind {
        KIND_PROCESS => "process",
        KIND_DEVICE => "device",
        KIND_COUNTERS => "counters",
        KIND_MONITOR => "monitor",
        KIND_MONITORS_DONE => "monitors_done",
        KIND_STARTED => "ack.started",
        KIND_STOPPED => "ack.stopped",
        KIND_FPS => "fps",
        KIND_FRAME_BATCH => "frame_batch",
        KIND_VERSION => "version",
        KIND_SUBMIT_TIMING => "submit_timing",
        KIND_SUBMIT_INFO => "submit_info",
        KIND_FRAME_START => "frame_start",
        KIND_FRAME_END => "frame_end",
        _ => "unknown",
    }
}

pub fn cstr(payload: &[u8], off: usize) -> &str {
    let tail = &payload[off.min(payload.len())..];
    let end = tail.iter().position(|b| *b == 0).unwrap_or(tail.len());
    std::str::from_utf8(&tail[..end]).unwrap_or("")
}

/// fps record payload: size, 0, f32 fps, 0, u64 gpu timestamp.
pub fn fps_value(rec: &Record<'_>, endian: Endian) -> Option<f32> {
    if rec.kind != KIND_FPS || rec.payload.len() < 12 {
        return None;
    }
    let bits = endian.from_bytes([
        rec.payload[8],
        rec.payload[9],
        rec.payload[10],
        rec.payload[11],
    ]);
    Some(f32::from_bits(bits))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex;

    fn sample() -> Vec<u8> {
        hex::parse(include_str!("../tests/fixtures/register.hex")).unwrap()
    }

    #[test]
    fn truncated_header() {
        let buf = hex::parse("0100cdab 00000000").unwrap();
        assert_eq!(
            parse(&buf, Endian::Little),
            Err(RecordError::TruncatedHeader(0))
        );
    }

    #[test]
    fn parse_records() {
        let buf = sample();
        let recs = parse(&buf, Endian::Little).unwrap();
        assert_eq!(recs.len(), 3);
        assert_eq!(recs[0].kind, KIND_PROCESS);
        assert_eq!(recs[0].payload.len(), 136);
        assert_eq!(recs[0].arg0, 301);
        assert_eq!(word(recs[0].payload, 4, Endian::Little), 301);
        assert_eq!(cstr(recs[0].payload, 8), "com.example.qprofprobe");
        assert_eq!(recs[1].kind, KIND_COUNTERS);
        assert_eq!(recs[2].kind, KIND_DEVICE);
        assert!(cstr(recs[2].payload, 20).starts_with("Adreno"));
    }

    #[test]
    fn parse_version() {
        let buf = hex::parse(include_str!("../tests/fixtures/version.hex")).unwrap();
        let recs = parse(&buf, Endian::Little).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].kind, KIND_VERSION);
        assert_eq!(recs[0].payload.len(), 56);
        assert_eq!(
            word(recs[0].payload, 0, Endian::Little) as usize,
            recs[0].payload.len()
        );
    }

    #[test]
    fn parse_fps() {
        let buf = hex::parse(include_str!("../tests/fixtures/fps.hex")).unwrap();
        let recs = parse(&buf, Endian::Little).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].kind, KIND_FPS);
        let fps = fps_value(&recs[0], Endian::Little).unwrap();
        assert!(fps > 1.0 && fps < 1000.0, "fps {fps}");
    }
}
