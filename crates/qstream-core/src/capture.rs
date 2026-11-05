//! Frame capture payload (kind 0x0B02000E).
//!
//! The batch is a sequence of state tokens, each with the same 12-byte header
//! used by replies: kind, payload_len, pid, then payload. The capture is a
//! stream of queue submit timing tokens and VkSubmitInfo state tokens.

use crate::token::Endian;

pub const STATE_SUBMIT_TIMING: u32 = 0x0B05_0013;
pub const STATE_SUBMIT_INFO: u32 = 0x0B03_006B;

/// Kept for older call sites and captures.
pub const STATE_FRAME_TIMING: u32 = STATE_SUBMIT_TIMING;
pub const STATE_FRAME_AUX: u32 = STATE_SUBMIT_INFO;

const TOKEN_HEADER: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateToken<'a> {
    pub pid: u32,
    pub kind: u32,
    pub payload: &'a [u8],
}

/// Queue submit timing record (0x0B050013).
///
/// The layer fills an 88-byte struct and appends one 8-byte GPU timestamp
/// fragment, so the payload is 96 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmitTiming {
    pub pid: u32,
    pub frame_index: u32,
    pub field_8: u32,
    pub start_us: u64,
    pub end_us: u64,
    pub gpu_ts: u64,
    pub frame_clock: u64,
    pub cpu_clock: u64,
    pub gpu_ts2: u64,
    pub gpu_ts3: u64,
}

/// Serialized VkSubmitInfo state (0x0B03006B): 64-byte struct plus fragments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmitInfoState {
    pub pid: u32,
    pub size: u32,
    pub s_type: u32,
    pub wait_count: u32,
    pub p_wait: u64,
    pub p_stage: u64,
    pub cmd_count: u32,
    pub p_cmd: u64,
    pub sig_count: u32,
    pub p_signal: u64,
    /// Fragment bytes after the fixed struct, usually pointer/handle arrays.
    pub frag0: u64,
    pub frag1: u64,
    pub frag2: u64,
    pub frag3: u64,
}

pub fn parse_tokens(data: &[u8], endian: Endian) -> Result<Vec<StateToken<'_>>, String> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < data.len() {
        if data.len() - off < TOKEN_HEADER {
            return Err(format!("truncated token header at {off}"));
        }
        let kind = word32(data, off, endian);
        let len = word32(data, off + 4, endian) as usize;
        let pid = word32(data, off + 8, endian);
        let body = off + TOKEN_HEADER;
        let end = body + len;
        if end > data.len() {
            return Err(format!("truncated token payload at {off}"));
        }
        out.push(StateToken {
            pid,
            kind,
            payload: &data[body..end],
        });
        off = end;
    }
    Ok(out)
}

pub fn submit_timing(tok: &StateToken<'_>, endian: Endian) -> Option<SubmitTiming> {
    if tok.kind != STATE_SUBMIT_TIMING || tok.payload.len() < 96 {
        return None;
    }
    let p = tok.payload;
    Some(SubmitTiming {
        pid: tok.pid,
        frame_index: word32(p, 4, endian),
        field_8: word32(p, 8, endian),
        start_us: word64(p, 16, endian),
        end_us: word64(p, 24, endian),
        gpu_ts: word64(p, 32, endian),
        frame_clock: word64(p, 48, endian),
        cpu_clock: word64(p, 56, endian),
        gpu_ts2: word64(p, 72, endian),
        gpu_ts3: word64(p, 88, endian),
    })
}

pub fn submit_info(tok: &StateToken<'_>, endian: Endian) -> Option<SubmitInfoState> {
    if tok.kind != STATE_SUBMIT_INFO || tok.payload.len() < 64 {
        return None;
    }
    let p = tok.payload;
    let frag = |off: usize| {
        if p.len() >= off + 8 {
            word64(p, off, endian)
        } else {
            0
        }
    };
    Some(SubmitInfoState {
        pid: tok.pid,
        size: word32(p, 0, endian),
        s_type: word32(p, 4, endian),
        wait_count: word32(p, 8, endian),
        p_wait: word64(p, 16, endian),
        p_stage: word64(p, 24, endian),
        cmd_count: word32(p, 32, endian),
        p_cmd: word64(p, 40, endian),
        sig_count: word32(p, 48, endian),
        p_signal: word64(p, 56, endian),
        frag0: frag(64),
        frag1: frag(72),
        frag2: frag(80),
        frag3: frag(88),
    })
}

fn word32(buf: &[u8], off: usize, endian: Endian) -> u32 {
    endian.from_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn word64(buf: &[u8], off: usize, endian: Endian) -> u64 {
    let lo = word32(buf, off, endian) as u64;
    let hi = word32(buf, off + 4, endian) as u64;
    match endian {
        Endian::Little => lo | (hi << 32),
        Endian::Big => (lo << 32) | hi,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(pid: u32, kind: u32, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&pid.to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn batch(tokens: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for tok in tokens {
            out.extend_from_slice(tok);
        }
        out
    }

    #[test]
    fn parses_tokens() {
        let data = batch(&[
            token(7, STATE_SUBMIT_TIMING, &[0u8; 96]),
            token(7, STATE_SUBMIT_INFO, &[0u8; 96]),
        ]);
        let toks = parse_tokens(&data, Endian::Little).unwrap();
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[0].pid, 7);
        assert_eq!(toks[0].kind, STATE_SUBMIT_TIMING);
        assert_eq!(toks[1].kind, STATE_SUBMIT_INFO);
        assert!(submit_timing(&toks[0], Endian::Little).is_some());
        assert!(submit_info(&toks[1], Endian::Little).is_some());
    }

    #[test]
    fn rejects_truncated() {
        let data = token(1, STATE_SUBMIT_TIMING, &[0u8; 8]);
        assert!(parse_tokens(&data[..15], Endian::Little).is_err());
    }

    #[test]
    fn timing_fields() {
        let mut p = vec![0u8; 96];
        p[4..8].copy_from_slice(&42u32.to_le_bytes());
        p[16..24].copy_from_slice(&1_000u64.to_le_bytes());
        p[24..32].copy_from_slice(&1_600u64.to_le_bytes());
        p[32..40].copy_from_slice(&0xB400_0079_491C_3CB0u64.to_le_bytes());
        p[88..96].copy_from_slice(&0xB400_0079_491C_3CC0u64.to_le_bytes());
        let ft = submit_timing(
            &StateToken {
                pid: 3,
                kind: STATE_SUBMIT_TIMING,
                payload: &p,
            },
            Endian::Little,
        )
        .unwrap();
        assert_eq!(ft.pid, 3);
        assert_eq!(ft.frame_index, 42);
        assert_eq!(ft.start_us, 1_000);
        assert_eq!(ft.end_us, 1_600);
        assert_eq!(ft.gpu_ts, 0xB400_0079_491C_3CB0);
        assert_eq!(ft.gpu_ts3, 0xB400_0079_491C_3CC0);
    }

    #[test]
    fn submit_fields() {
        let mut p = vec![0u8; 96];
        p[0..4].copy_from_slice(&64u32.to_le_bytes());
        p[4..8].copy_from_slice(&4u32.to_le_bytes());
        p[8..12].copy_from_slice(&1u32.to_le_bytes());
        p[32..36].copy_from_slice(&2u32.to_le_bytes());
        p[48..52].copy_from_slice(&3u32.to_le_bytes());
        let s = submit_info(
            &StateToken {
                pid: 9,
                kind: STATE_SUBMIT_INFO,
                payload: &p,
            },
            Endian::Little,
        )
        .unwrap();
        assert_eq!(s.pid, 9);
        assert_eq!(s.size, 64);
        assert_eq!(s.s_type, 4);
        assert_eq!(s.wait_count, 1);
        assert_eq!(s.cmd_count, 2);
        assert_eq!(s.sig_count, 3);
    }
}
