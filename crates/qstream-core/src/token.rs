use std::str::FromStr;

pub const CMD_REGISTER: u32 = 0x0102_0002;
pub const CMD_GPU_SCOPE_GROUPS: u32 = 0x0102_0018;
pub const CMD_SCOPE_START: u32 = 0x0102_0019;
pub const CMD_SCOPE_STOP: u32 = 0x0102_001A;

/// Add or remove one GPU scope counter pair.
pub const GPU_SCOPE_COUNTER_REMOVE: u32 = 0;
pub const GPU_SCOPE_COUNTER_ADD: u32 = 1;

pub const CMD_CAPTURE_FRAME: u32 = 0x0B02_0001;
pub const CMD_CAPTURE_START: u32 = 0x0B02_0002;
pub const CMD_CAPTURE_STOP: u32 = 0x0B02_0003;
pub const CMD_FPS: u32 = 0x0B02_0009;
pub const CMD_FRAME_RATE: u32 = 0x0B02_000A;
pub const CMD_FRAME_DELIMITERS: u32 = 0x0B02_000B;

pub const REPLY_REGISTER_OK: u32 = 0xABCD_0001;
pub const REPLY_VERSION: u32 = 0x0B02_0012;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

impl Endian {
    pub fn to_bytes(self, value: u32) -> [u8; 4] {
        match self {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        }
    }

    pub fn from_bytes(self, bytes: [u8; 4]) -> u32 {
        match self {
            Endian::Little => u32::from_le_bytes(bytes),
            Endian::Big => u32::from_be_bytes(bytes),
        }
    }
}

impl FromStr for Endian {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "le" | "little" => Ok(Endian::Little),
            "be" | "big" => Ok(Endian::Big),
            other => Err(format!("expected le or be, got {other}")),
        }
    }
}

/// Command header: token, payload_len, arg.
///
/// The layer reads `payload_len` more bytes and hands that buffer to the
/// command handler. Small arguments travel in the payload, not in the header.
pub fn header(token: u32, payload_len: u32, arg: u32, endian: Endian) -> [u8; 12] {
    let mut out = [0u8; 12];
    out[0..4].copy_from_slice(&endian.to_bytes(token));
    out[4..8].copy_from_slice(&endian.to_bytes(payload_len));
    out[8..12].copy_from_slice(&endian.to_bytes(arg));
    out
}

/// Header with no payload.
pub fn request(token: u32, endian: Endian) -> Vec<u8> {
    header(token, 0, 0, endian).to_vec()
}

/// Payload layout used by scalar commands: {u32 pad, u32 value}.
pub fn request_u32(token: u32, value: u32, endian: Endian) -> Vec<u8> {
    let mut out = header(token, 8, 0, endian).to_vec();
    out.extend_from_slice(&endian.to_bytes(0));
    out.extend_from_slice(&endian.to_bytes(value));
    out
}

/// Payload layout used by frame capture: {u32 pad, u32 pad, u64 device}.
pub fn request_device(token: u32, device: u64, endian: Endian) -> Vec<u8> {
    let mut out = header(token, 16, 0, endian).to_vec();
    out.extend_from_slice(&endian.to_bytes(0));
    out.extend_from_slice(&endian.to_bytes(0));
    let lo = device as u32;
    let hi = (device >> 32) as u32;
    match endian {
        Endian::Little => {
            out.extend_from_slice(&endian.to_bytes(lo));
            out.extend_from_slice(&endian.to_bytes(hi));
        }
        Endian::Big => {
            out.extend_from_slice(&endian.to_bytes(hi));
            out.extend_from_slice(&endian.to_bytes(lo));
        }
    }
    out
}

/// Payload layout used by the gpu scope start command: pad, mode, flag1, flag2.
pub fn request_scope(mode: u32, flag1: u32, flag2: u32, endian: Endian) -> Vec<u8> {
    let mut out = header(CMD_SCOPE_START, 16, 0, endian).to_vec();
    for value in [16, mode, flag1, flag2] {
        out.extend_from_slice(&endian.to_bytes(value));
    }
    out
}

/// Payload layout used by counter selection: pad, pad, pad, action, group, counter.
pub fn request_group_counter(action: u32, group: u32, counter: u32, endian: Endian) -> Vec<u8> {
    let mut out = header(CMD_GPU_SCOPE_GROUPS, 24, 0, endian).to_vec();
    for value in [0, 0, 0, action, group, counter] {
        out.extend_from_slice(&endian.to_bytes(value));
    }
    out
}

pub fn name(token: u32) -> &'static str {
    match token {
        CMD_REGISTER => "cmd.register",
        CMD_GPU_SCOPE_GROUPS => "cmd.gpu_scope_groups",
        CMD_SCOPE_START => "cmd.gpu_scope_start",
        CMD_SCOPE_STOP => "cmd.gpu_scope_stop",
        CMD_CAPTURE_FRAME => "cmd.capture_frame",
        CMD_CAPTURE_START => "cmd.capture_start",
        CMD_CAPTURE_STOP => "cmd.capture_stop",
        CMD_FPS => "cmd.fps",
        CMD_FRAME_RATE => "cmd.frame_rate",
        CMD_FRAME_DELIMITERS => "cmd.frame_delimiters",
        REPLY_REGISTER_OK => "record.magic",
        REPLY_VERSION => "reply.version",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endian_roundtrip() {
        for e in [Endian::Little, Endian::Big] {
            assert_eq!(e.from_bytes(e.to_bytes(0x0102_0002)), CMD_REGISTER);
        }
    }

    #[test]
    fn little_bytes() {
        assert_eq!(
            Endian::Little.to_bytes(CMD_REGISTER),
            [0x02, 0x00, 0x02, 0x01]
        );
    }

    #[test]
    fn header_is_12_bytes() {
        let h = header(CMD_REGISTER, 0, 0, Endian::Little);
        assert_eq!(&h[..4], &[0x02, 0x00, 0x02, 0x01]);
        assert_eq!(&h[4..], &[0u8; 8]);
    }

    #[test]
    fn names() {
        assert_eq!(name(CMD_CAPTURE_START), "cmd.capture_start");
        assert_eq!(name(CMD_SCOPE_START), "cmd.gpu_scope_start");
        assert_eq!(name(CMD_SCOPE_STOP), "cmd.gpu_scope_stop");
        assert_eq!(name(REPLY_REGISTER_OK), "record.magic");
        assert_eq!(name(0xDEAD_BEEF), "unknown");
    }

    #[test]
    fn u32_request_layout() {
        let req = request_u32(CMD_FPS, 1, Endian::Little);
        assert_eq!(req.len(), 20);
        assert_eq!(&req[0..4], &CMD_FPS.to_le_bytes());
        assert_eq!(&req[4..8], &8u32.to_le_bytes());
        assert_eq!(&req[8..12], &0u32.to_le_bytes());
        assert_eq!(&req[12..16], &0u32.to_le_bytes());
        assert_eq!(&req[16..20], &1u32.to_le_bytes());
    }

    #[test]
    fn device_request_layout() {
        let req = request_device(CMD_CAPTURE_FRAME, 0xB400_0079_1234_5678, Endian::Little);
        assert_eq!(req.len(), 28);
        assert_eq!(&req[4..8], &16u32.to_le_bytes());
        assert_eq!(&req[12..16], &0u32.to_le_bytes());
        assert_eq!(&req[16..20], &0u32.to_le_bytes());
        assert_eq!(&req[20..24], &0x1234_5678u32.to_le_bytes());
        assert_eq!(&req[24..28], &0xB400_0079u32.to_le_bytes());
    }

    #[test]
    fn counter_request_layout() {
        let req = request_group_counter(GPU_SCOPE_COUNTER_ADD, 9, 1, Endian::Little);
        assert_eq!(req.len(), 36);
        assert_eq!(&req[0..4], &CMD_GPU_SCOPE_GROUPS.to_le_bytes());
        assert_eq!(&req[4..8], &24u32.to_le_bytes());
        assert_eq!(&req[12..16], &0u32.to_le_bytes());
        assert_eq!(&req[24..28], &GPU_SCOPE_COUNTER_ADD.to_le_bytes());
        assert_eq!(&req[28..32], &9u32.to_le_bytes());
        assert_eq!(&req[32..36], &1u32.to_le_bytes());
    }

    #[test]
    fn scope_request_layout() {
        let req = request_scope(1, 2, 3, Endian::Little);
        assert_eq!(req.len(), 28);
        assert_eq!(&req[0..4], &CMD_SCOPE_START.to_le_bytes());
        assert_eq!(&req[4..8], &16u32.to_le_bytes());
        assert_eq!(&req[12..16], &16u32.to_le_bytes());
        assert_eq!(&req[16..20], &1u32.to_le_bytes());
        assert_eq!(&req[20..24], &2u32.to_le_bytes());
        assert_eq!(&req[24..28], &3u32.to_le_bytes());
    }
}
