//! CAPT packet framing: `u16 opcode, u16 total_length, payload`, little-endian.

/// CAPT opcodes used by the P1080 path (names follow Canon's function names).
pub mod op {
    // Page / image data (sent without a response).
    pub const SET_PAGE_PARAMS: u16 = 0xD0A0; // IcBeginPage
    pub const BEGIN_DATA: u16 = 0xD0A1; // IcBeginData
    pub const END_PAGE: u16 = 0xD0A2; // IcEndPage
    pub const SET_REF_DESC: u16 = 0xD0A4; // IcColorPlane (K plane compression references)
    pub const PAGE_SETUP: u16 = 0xD0A9; // the four D0Ax packets above sent as one
    pub const VIDEO_DATA: u16 = 0xC0A0; // IcVideoData
    pub const VIDEO_END: u16 = 0xC0A4; // IcBlackEnd

    // Engine requests (each answered by a response packet).
    pub const PAPER_STATUS: u16 = 0xA0A1;
    pub const OUTPUT_STATUS: u16 = 0xA0A2;
    pub const CARTRIDGE_STATUS: u16 = 0xA0A4;
    pub const JOB_INFO: u16 = 0xA0A7;
    pub const EXTENDED_STATUS: u16 = 0xA0A8;
    pub const DEVICE_ID: u16 = 0xA1A0;
    pub const PRINTER_INFO: u16 = 0xA1A1;
    pub const ENGINE_VERSION: u16 = 0xA1A8;
    pub const RESERVE: u16 = 0xA2A0;
    pub const MISPRINT_STATUS: u16 = 0xA2A1;
    pub const JAM_STATUS: u16 = 0xA2A3;
    pub const WAITING_STATUS: u16 = 0xA2A4;
    pub const ANNOUNCE_JOB: u16 = 0xA3A2;
    pub const PAGE_STATUS: u16 = 0xA3AB;
    pub const BASIC_STATUS: u16 = 0xE0A0;
    pub const RESET_ENGINE: u16 = 0xE0A1;
    pub const CLEAR_ERROR: u16 = 0xE0A2;
    pub const CLEAR_MISPRINT: u16 = 0xE0A3;
    pub const DISCARD_DATA: u16 = 0xE0A4;
    pub const GO_ONLINE: u16 = 0xE0A5;
    pub const GO_OFFLINE: u16 = 0xE0A6;
    pub const START_PRINT: u16 = 0xE0A7;
    pub const RELEASE: u16 = 0xE0A9;
    pub const SET_JOB_INFO: u16 = 0xE1A1;
    pub const SET_LED: u16 = 0xE1A2;
    pub const SET_PRINTED_PAGES: u16 = 0xE1A6;

    /// Human-readable name for logs.
    pub fn name(op: u16) -> &'static str {
        match op {
            SET_PAGE_PARAMS => "SetPageParams",
            BEGIN_DATA => "BeginData",
            END_PAGE => "EndPage",
            SET_REF_DESC => "SetRefDesc",
            PAGE_SETUP => "PageSetup",
            VIDEO_DATA => "VideoData",
            VIDEO_END => "VideoEnd",
            PAPER_STATUS => "PaperStatus",
            OUTPUT_STATUS => "OutputStatus",
            CARTRIDGE_STATUS => "CartridgeStatus",
            JOB_INFO => "JobInfo",
            EXTENDED_STATUS => "ExtendedStatus",
            DEVICE_ID => "DeviceId",
            PRINTER_INFO => "PrinterInfo",
            ENGINE_VERSION => "EngineVersion",
            RESERVE => "Reserve",
            MISPRINT_STATUS => "MisprintStatus",
            JAM_STATUS => "JamStatus",
            WAITING_STATUS => "WaitingStatus",
            ANNOUNCE_JOB => "AnnounceJob",
            PAGE_STATUS => "PageStatus",
            BASIC_STATUS => "BasicStatus",
            RESET_ENGINE => "ResetEngine",
            CLEAR_ERROR => "ClearError",
            CLEAR_MISPRINT => "ClearMisprint",
            DISCARD_DATA => "DiscardData",
            GO_ONLINE => "GoOnline",
            GO_OFFLINE => "GoOffline",
            START_PRINT => "StartPrint",
            RELEASE => "Release",
            SET_JOB_INFO => "SetJobInfo",
            SET_LED => "SetLed",
            SET_PRINTED_PAGES => "SetPrintedPages",
            _ => "?",
        }
    }
}

/// Little-endian packet builder.
#[derive(Debug, Clone)]
pub struct PacketBuilder {
    buf: Vec<u8>,
}

impl PacketBuilder {
    pub fn new(opcode: u16) -> Self {
        let mut buf = Vec::with_capacity(16);
        buf.extend_from_slice(&opcode.to_le_bytes());
        buf.extend_from_slice(&[0, 0]); // length, patched in finish()
        PacketBuilder { buf }
    }
    pub fn u8(mut self, v: u8) -> Self {
        self.buf.push(v);
        self
    }
    pub fn u16(mut self, v: u16) -> Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u32(mut self, v: u32) -> Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn bytes(mut self, v: &[u8]) -> Self {
        self.buf.extend_from_slice(v);
        self
    }
    /// Zero-pad the packet to `total` bytes (header included).
    pub fn pad_to(mut self, total: usize) -> Self {
        if self.buf.len() < total {
            self.buf.resize(total, 0);
        }
        self
    }
    pub fn finish(mut self) -> Vec<u8> {
        let len = self.buf.len();
        assert!(len <= u16::MAX as usize, "CAPT packet too long: {len}");
        self.buf[2..4].copy_from_slice(&(len as u16).to_le_bytes());
        self.buf
    }
}

/// Packet with no payload.
pub fn simple(opcode: u16) -> Vec<u8> {
    PacketBuilder::new(opcode).finish()
}

/// Header of a packet: (opcode, total length).
pub fn header(p: &[u8]) -> Option<(u16, usize)> {
    if p.len() < 4 {
        return None;
    }
    Some((
        u16::from_le_bytes([p[0], p[1]]),
        u16::from_le_bytes([p[2], p[3]]) as usize,
    ))
}

/// Split a buffer of concatenated packets.
pub fn split(mut data: &[u8]) -> Result<Vec<&[u8]>, String> {
    let mut out = Vec::new();
    while !data.is_empty() {
        let (op, len) = header(data).ok_or("truncated packet header")?;
        if len < 4 || len > data.len() {
            return Err(format!("bad packet length {len} for opcode {op:04X}"));
        }
        out.push(&data[..len]);
        data = &data[len..];
    }
    Ok(out)
}

/// A response packet: 6-byte header (`u16 opcode, u16 len, u8, u8`) plus body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub opcode: u16,
    pub flags: [u8; 2],
    pub body: Vec<u8>,
}

impl Response {
    pub fn parse(raw: &[u8]) -> Result<Response, String> {
        if raw.len() < 6 {
            return Err(format!("short response ({} bytes)", raw.len()));
        }
        let (opcode, len) = header(raw).unwrap();
        if opcode == 0 {
            return Err("printer answered with opcode 0".into());
        }
        if len < 6 {
            return Err(format!("response length {len} < 6"));
        }
        let end = len.min(raw.len());
        Ok(Response {
            opcode,
            flags: [raw[4], raw[5]],
            body: raw[6..end].to_vec(),
        })
    }
    pub fn u16_at(&self, off: usize) -> Option<u16> {
        self.body
            .get(off..off + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    }
}

pub fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_reserve_like_canon() {
        let p = PacketBuilder::new(op::RESERVE)
            .u16(0)
            .u16(0x1E)
            .pad_to(12)
            .finish();
        assert_eq!(p, [0xA0, 0xA2, 0x0C, 0x00, 0, 0, 0x1E, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn splits_packets() {
        let mut d = simple(op::BEGIN_DATA);
        d.extend(simple(op::END_PAGE));
        let parts = split(&d).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(header(parts[1]), Some((op::END_PAGE, 4)));
    }
}
