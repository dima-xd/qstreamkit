//! GPU scope monitor reports (kind 0x0B020007).
//!
//! A report is sent by QGLGpuScope after a queue submit or command buffer
//! completes while the scope is in monitor mode (start mode 2). The payload starts with a
//! 0x38 byte header and then appends the driver monitor blob:
//!
//!   header: fixed size, raw size, gpu id, mode, field, arg, object, submit,
//!           flags, frame, reserved, arg2
//!   raw:    driver monitor data
//!
//! The driver blob starts with its own header. `blocks` is 2 + pair count.
//! Each pair is a 16 byte entry {group, counter, 0, 0}. The counter block
//! follows the pair list:
//!
//!   +0x00 u32 5
//!   +0x04 u32 4
//!   +0x08 u64 clock
//!   +0x10 u64 value       cumulative counter for the first pair
//!   +0x18 u64 t0
//!   +0x20 u64 t1
//!   +0x28 u64 t2
//!
//! Some reports, usually the first one, also carry per-counter samples after
//! a 0x30 byte secondary header. A sample is 0x10 + 0x10 * pair_count bytes;
//! its value is at +0x10.

use crate::token::Endian;

pub const KIND_MONITOR: u32 = 0x0B02_0007;
pub const KIND_MONITORS_DONE: u32 = 0x0102_001C;

const PAYLOAD_HEADER: usize = 0x38;
const RAW_HEADER: usize = 0x30;
const PAIR_STRIDE: usize = 0x10;
const COUNTER_BLOCK: usize = 0x30;
const SAMPLE_HEADER: usize = 0x30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CounterPair {
    pub group: u32,
    pub counter: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorSample {
    pub id: u32,
    pub timestamp: u64,
    pub value: u64,
    pub aux: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorReport<'a> {
    pub gpu: u32,
    pub mode: u32,
    pub field: u32,
    pub arg: u32,
    /// Command buffer handle for command-buffer monitors, zero for queue monitors.
    pub object: u64,
    pub submit: u32,
    pub flags: u32,
    pub frame: u32,
    pub arg2: u64,
    pub raw_size: usize,
    pub pairs: Vec<CounterPair>,
    pub clock: u64,
    pub value: u64,
    pub t0: u64,
    pub t1: u64,
    pub t2: u64,
    pub samples: Vec<MonitorSample>,
    pub raw: &'a [u8],
}

impl MonitorReport<'_> {
    /// Pair whose cumulative value is stored in `value`.
    pub fn value_pair(&self) -> Option<CounterPair> {
        self.pairs.first().copied()
    }

    pub fn pair_index(&self, group: u32, counter: u32) -> Option<usize> {
        self.pairs
            .iter()
            .position(|p| p.group == group && p.counter == counter)
    }
}

pub fn parse(payload: &[u8], endian: Endian) -> Option<MonitorReport<'_>> {
    if payload.len() < PAYLOAD_HEADER {
        return None;
    }
    let header = word32(payload, 0, endian)? as usize;
    let raw_size = word32(payload, 4, endian)? as usize;
    if header < PAYLOAD_HEADER || raw_size == 0 || header + raw_size > payload.len() {
        return None;
    }
    let raw = &payload[header..header + raw_size];

    let gpu = word32(payload, 8, endian)?;
    let mode = word32(payload, 12, endian)?;
    let field = word32(payload, 16, endian)?;
    let arg = word32(payload, 20, endian)?;
    let object = word64(payload, 24, endian)?;
    let submit = word32(payload, 32, endian)?;
    let flags = word32(payload, 36, endian)?;
    let frame = word32(payload, 40, endian)?;
    let arg2 = word64(payload, 48, endian)?;

    let blocks = word32(raw, 8, endian).unwrap_or(0) as usize;
    let pair_count = blocks.saturating_sub(2);
    let mut pairs = Vec::new();
    for i in 0..pair_count {
        let off = RAW_HEADER + i * PAIR_STRIDE;
        if off + 8 > raw.len() {
            break;
        }
        pairs.push(CounterPair {
            group: word32(raw, off, endian)?,
            counter: word32(raw, off + 4, endian)?,
        });
    }

    let data = RAW_HEADER + pairs.len() * PAIR_STRIDE;
    let (clock, value, t0, t1, t2) = if data + COUNTER_BLOCK <= raw.len() {
        (
            word64(raw, data + 0x08, endian).unwrap_or(0),
            word64(raw, data + 0x10, endian).unwrap_or(0),
            word64(raw, data + 0x18, endian).unwrap_or(0),
            word64(raw, data + 0x20, endian).unwrap_or(0),
            word64(raw, data + 0x28, endian).unwrap_or(0),
        )
    } else {
        (0, 0, 0, 0, 0)
    };

    let mut samples = Vec::new();
    let sample_off = data + COUNTER_BLOCK + SAMPLE_HEADER;
    let sample_stride = PAIR_STRIDE + pairs.len() * PAIR_STRIDE;
    if !pairs.is_empty() && sample_off + 0x20 <= raw.len() {
        let mut off = sample_off;
        while off + 0x20 <= raw.len() {
            let count = word32(raw, off, endian).unwrap_or(0);
            let id = word32(raw, off + 4, endian).unwrap_or(0);
            if count != 1 || (id & 0xFFFF_0000) != 0x0008_0000 {
                break;
            }
            samples.push(MonitorSample {
                id,
                timestamp: word64(raw, off + 8, endian).unwrap_or(0),
                value: word64(raw, off + 16, endian).unwrap_or(0),
                aux: word64(raw, off + 24, endian).unwrap_or(0),
            });
            if sample_stride == 0 {
                break;
            }
            off += sample_stride;
        }
    }

    Some(MonitorReport {
        gpu,
        mode,
        field,
        arg,
        object,
        submit,
        flags,
        frame,
        arg2,
        raw_size,
        pairs,
        clock,
        value,
        t0,
        t1,
        t2,
        samples,
        raw,
    })
}

fn word32(buf: &[u8], off: usize, endian: Endian) -> Option<u32> {
    let bytes = buf.get(off..off + 4)?;
    Some(endian.from_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn word64(buf: &[u8], off: usize, endian: Endian) -> Option<u64> {
    let lo = word32(buf, off, endian)? as u64;
    let hi = word32(buf, off + 4, endian)? as u64;
    Some(match endian {
        Endian::Little => lo | (hi << 32),
        Endian::Big => (lo << 32) | hi,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put32(buf: &mut [u8], off: usize, value: u32) {
        buf[off..off + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put64(buf: &mut [u8], off: usize, value: u64) {
        buf[off..off + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn sample_payload() -> Vec<u8> {
        let mut payload = vec![0u8; PAYLOAD_HEADER + 0xD0];
        put32(&mut payload, 0, PAYLOAD_HEADER as u32);
        put32(&mut payload, 4, 0xD0);
        put32(&mut payload, 8, 39);
        put32(&mut payload, 12, 2);
        put32(&mut payload, 16, 3);
        put32(&mut payload, 20, 0x491F_0910);
        put64(&mut payload, 24, 0xB400_0079_491F_09B0);
        put32(&mut payload, 32, 387);
        put32(&mut payload, 40, 139);
        put64(&mut payload, 48, 0xB400_0079_491F_0910);

        let raw = &mut payload[PAYLOAD_HEADER..];
        put32(raw, 0, 0xD0);
        put32(raw, 4, 39);
        put32(raw, 8, 3);
        put32(raw, 0x10, 0x1B);
        put32(raw, 0x24, 1);
        put32(raw, 0x30, 9);
        put32(raw, 0x34, 1);
        put32(raw, 0x40, 5);
        put32(raw, 0x44, 4);
        put64(raw, 0x48, 0x6AEC_72A9);
        put64(raw, 0x50, 766_081_486);
        put64(raw, 0x58, 114_680_245_721_314);
        put64(raw, 0x60, 114_680_245_727_532);
        put64(raw, 0x68, 114_680_245_729_000);
        put32(raw, 0x70, 5);
        put32(raw, 0x74, 6);
        put32(raw, 0x78, 0x8F);
        put32(raw, 0x7C, 39);
        put32(raw, 0xA0, 1);
        put32(raw, 0xA4, 0x0008_0001);
        put64(raw, 0xA8, 114_680_245_730_000);
        put64(raw, 0xB0, 84_262);
        put64(raw, 0xB8, 0xA9);
        payload
    }

    #[test]
    fn parses_one_pair() {
        let payload = sample_payload();
        let report = parse(&payload, Endian::Little).unwrap();
        assert_eq!(report.gpu, 39);
        assert_eq!(report.mode, 2);
        assert_eq!(report.submit, 387);
        assert_eq!(report.frame, 139);
        assert_eq!(
            report.pairs,
            vec![CounterPair {
                group: 9,
                counter: 1
            }]
        );
        assert_eq!(report.value, 766_081_486);
        assert_eq!(report.clock, 0x6AEC_72A9);
        assert_eq!(report.t0, 114_680_245_721_314);
        assert_eq!(report.samples.len(), 1);
        assert_eq!(report.samples[0].value, 84_262);
        assert_eq!(report.value_pair().unwrap().group, 9);
        assert_eq!(report.pair_index(9, 1), Some(0));
        assert_eq!(report.pair_index(9, 2), None);
    }

    #[test]
    fn parses_real_fixture() {
        let bytes =
            crate::hex::parse(include_str!("../tests/fixtures/monitor.hex").trim()).unwrap();
        let report = parse(&bytes, Endian::Little).unwrap();
        assert_eq!(report.gpu, 39);
        assert_eq!(report.mode, 2);
        assert_eq!(report.submit, 18_500);
        assert_eq!(report.frame, 9_414);
        assert_eq!(report.arg, 0x491A_7910);
        assert_eq!(
            report.pairs,
            vec![CounterPair {
                group: 9,
                counter: 1
            }]
        );
        assert_eq!(report.value, 64_603_726);
        assert_eq!(report.clock, 0x6AEC_75DD);
        assert_eq!(report.samples.len(), 2);
        assert_eq!(report.samples[0].id, 0x0008_0001);
        assert_eq!(report.samples[0].value, 9_562);
        assert_eq!(report.samples[1].id, 0x0008_0002);
        assert_eq!(report.samples[1].value, 48_577);
    }

    #[test]
    fn rejects_short_payload() {
        assert!(parse(&[0u8; 12], Endian::Little).is_none());
    }

    #[test]
    fn rejects_bad_raw_size() {
        let mut payload = sample_payload();
        put32(&mut payload, 4, 0xFFFF);
        assert!(parse(&payload, Endian::Little).is_none());
    }
}
