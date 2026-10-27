//! Page geometry and the per-page CAPT packet sequence for the P1080 engine.

use crate::packet::{op, PacketBuilder};
use crate::slim::{self, RefDesc};

/// Engine id the LBP3000/LBP2900 path writes into the page header (`_tTargetModelTbl`).
pub const ENGINE_ID_P1080: u16 = 0x2A30;
pub const DPI: u32 = 600;

/// Non-printable margins at 600 dpi, from the LBP3000 PPD's ImageableArea
/// (14.40 pt left/right, 11.52 pt top, 17.04 pt bottom).
pub const MARGIN_LEFT: u32 = 120;
pub const MARGIN_RIGHT: u32 = 120;
pub const MARGIN_TOP: u32 = 96;
pub const MARGIN_BOTTOM: u32 = 142;

/// Largest data payload in one `C0A0` packet (captmoncnab3 `CopyPage2`).
pub const MAX_VIDEO_CHUNK: usize = 0xFF00;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Paper {
    pub name: &'static str,
    /// Windows DMPAPER id (Canon's paper id).
    pub dmpaper: u16,
    /// Code written into the page header (`_tPaperSizeTbl`).
    pub code: u8,
    pub width_pt: u32,
    pub height_pt: u32,
    /// Size at 600 dpi (`_P1080PSizeTbl`).
    pub width_px: u32,
    pub height_px: u32,
}

macro_rules! paper {
    ($name:expr, $dm:expr, $code:expr, $wpt:expr, $hpt:expr, $wpx:expr, $hpx:expr) => {
        Paper { name: $name, dmpaper: $dm, code: $code, width_pt: $wpt, height_pt: $hpt, width_px: $wpx, height_px: $hpx }
    };
}

pub const PAPERS: &[Paper] = &[
    paper!("A4", 9, 2, 595, 842, 4960, 7014),
    paper!("Letter", 1, 13, 612, 792, 5100, 6600),
    paper!("Legal", 5, 12, 612, 1008, 5100, 8400),
    paper!("Executive", 7, 10, 522, 756, 4350, 6300),
    paper!("A5", 11, 3, 420, 595, 3496, 4960),
    paper!("B5", 13, 7, 516, 729, 4298, 6070),
    paper!("Monarch", 37, 23, 279, 540, 2328, 4500),
    paper!("Com10", 20, 22, 297, 684, 2478, 5700),
    paper!("DL", 27, 24, 312, 624, 2598, 5196),
    paper!("C5", 28, 21, 459, 649, 3826, 5408),
    paper!("Index3x5", 4600, 64, 216, 360, 1800, 3000),
];

impl Paper {
    pub fn by_name(name: &str) -> Option<&'static Paper> {
        PAPERS.iter().find(|p| p.name.eq_ignore_ascii_case(name))
    }
    /// Find the paper whose size matches `w`×`h` points (±3 pt).
    pub fn by_points(w: f32, h: f32) -> Option<&'static Paper> {
        PAPERS
            .iter()
            .find(|p| (p.width_pt as f32 - w).abs() <= 3.0 && (p.height_pt as f32 - h).abs() <= 3.0)
    }
    /// Printable image width in pixels (rounded up to 32 pixels like Canon's filter).
    pub fn image_width_px(&self) -> u32 {
        (self.width_px - MARGIN_LEFT - MARGIN_RIGHT + 31) & !31
    }
    pub fn image_height_px(&self) -> u32 {
        self.height_px - MARGIN_TOP - MARGIN_BOTTOM
    }
    pub fn image_line_bytes(&self) -> usize {
        self.image_width_px() as usize / 8
    }
}

/// Per-job settings that end up in the page header.
#[derive(Debug, Clone, Copy)]
pub struct PageSettings {
    /// Four density bytes at header offset +8 (Canon default 0x1F).
    pub density: [u8; 4],
    /// Media code (`_tMediaTypeTbl`; plain paper = 1).
    pub media_code: u8,
    /// Paper source code (`_tPaperSourceTbl`; auto = 0).
    pub source: u8,
}

impl Default for PageSettings {
    fn default() -> Self {
        PageSettings { density: [0x1F; 4], media_code: 1, source: 0 }
    }
}

/// Builds the CAPT packets for one page.
pub struct PageEncoder {
    pub paper: &'static Paper,
    pub settings: PageSettings,
    pub refs: RefDesc,
}

impl PageEncoder {
    pub fn new(paper: &'static Paper, settings: PageSettings) -> Self {
        PageEncoder { paper, settings, refs: RefDesc::P1080 }
    }

    /// `D0A0` page parameters (44 bytes).
    pub fn page_params(&self, line_bytes: usize, lines: usize) -> Vec<u8> {
        let s = &self.settings;
        PacketBuilder::new(op::SET_PAGE_PARAMS)
            .u16(0)
            .u16(ENGINE_ID_P1080)
            .u8(self.paper.code)
            .u8(0)
            .u8(s.source)
            .u8(0)
            .bytes(&s.density)
            .u8(0) // media mode: model 1 has no _tModelToModeTbl entry
            .u8(0x11)
            .u8(4)
            .u8(0)
            .u8(1) // bits per pixel
            .u8(1) // monochrome
            .u8(0)
            .u8(0)
            .u16(0)
            .u16(MARGIN_LEFT as u16)
            .u16(MARGIN_TOP as u16)
            .u16(line_bytes as u16)
            .u16(lines as u16)
            .u16(self.paper.width_px as u16)
            .u16(self.paper.height_px as u16)
            .u16(0) // flags
            .u8(s.media_code)
            .pad_to(44)
            .finish()
    }

    /// All packets for one page; `bitmap` is 1 bit per pixel, MSB first, 1 = black,
    /// `line_bytes` × `lines` (normally `image_line_bytes()` × `image_height_px()`).
    pub fn encode_page(&self, bitmap: &[u8], line_bytes: usize, lines: usize) -> Vec<u8> {
        let mut out = self.page_params(line_bytes, lines);
        out.extend(PacketBuilder::new(op::SET_REF_DESC).bytes(&self.refs.to_bytes()).finish());
        out.extend(crate::packet::simple(op::BEGIN_DATA));
        let data = slim::compress(bitmap, line_bytes, lines, &self.refs);
        for chunk in data.chunks(MAX_VIDEO_CHUNK) {
            out.extend(PacketBuilder::new(op::VIDEO_DATA).bytes(chunk).finish());
        }
        out.extend(crate::packet::simple(op::VIDEO_END));
        out.extend(crate::packet::simple(op::END_PAGE));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{header, split};

    #[test]
    fn a4_geometry() {
        let a4 = Paper::by_name("a4").unwrap();
        assert_eq!(a4.image_width_px(), 4736);
        assert_eq!(a4.image_line_bytes(), 592);
        assert_eq!(a4.image_height_px(), 6776);
        assert_eq!(Paper::by_points(595.3, 841.9).unwrap().name, "A4");
    }

    #[test]
    fn page_header_layout() {
        let enc = PageEncoder::new(Paper::by_name("A4").unwrap(), PageSettings::default());
        let p = enc.page_params(592, 6776);
        assert_eq!(p.len(), 44);
        assert_eq!(&p[..8], &[0xA0, 0xD0, 44, 0, 0, 0, 0x30, 0x2A]);
        assert_eq!(p[8], 2); // A4
        assert_eq!(&p[12..16], &[0x1F; 4]);
        assert_eq!(&p[17..22], &[0x11, 4, 0, 1, 1]);
        assert_eq!(u16::from_le_bytes([p[30], p[31]]), 592);
        assert_eq!(u16::from_le_bytes([p[32], p[33]]), 6776);
        assert_eq!(p[40], 1);
    }

    #[test]
    fn page_packet_sequence() {
        let enc = PageEncoder::new(Paper::by_name("A5").unwrap(), PageSettings::default());
        let (lb, lines) = (enc.paper.image_line_bytes(), 64);
        let bitmap = vec![0u8; lb * lines];
        let data = enc.encode_page(&bitmap, lb, lines);
        let ops: Vec<u16> = split(&data).unwrap().iter().map(|p| header(p).unwrap().0).collect();
        assert_eq!(
            ops,
            [op::SET_PAGE_PARAMS, op::SET_REF_DESC, op::BEGIN_DATA, op::VIDEO_DATA, op::VIDEO_END, op::END_PAGE]
        );
    }
}
