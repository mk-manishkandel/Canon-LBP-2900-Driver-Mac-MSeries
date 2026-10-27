//! Page geometry and the per-page CAPT packet sequence for the P1080 engine.
//!
//! Layout and packet order follow captdriver's LBP2900 support, which prints on real
//! hardware (see `docs/re-notes.md`).

use crate::packet::{op, PacketBuilder};
use crate::slim::{self, RefDesc};

/// Engine id the LBP3000/LBP2900 path writes into the page header (`_tTargetModelTbl`).
pub const ENGINE_ID_P1080: u16 = 0x2A30;
pub const DPI: u32 = 600;

/// 5 mm hardware margin on every side, at 600 dpi and in points.
pub const MARGIN: u32 = 118;
pub const MARGIN_PT: u16 = 14;

/// Lines per independently compressed band.
pub const BAND_LINES: usize = 70;

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
    /// Bytes per image line: the page width in points, as captdriver sends it.
    pub fn image_line_bytes(&self) -> usize {
        self.width_pt as usize
    }
    pub fn image_width_px(&self) -> u32 {
        self.image_line_bytes() as u32 * 8
    }
    /// The image lines are centred horizontally on the page.
    pub fn image_left_px(&self) -> u32 {
        self.width_px.saturating_sub(self.image_width_px()) / 2
    }
    pub fn image_top_px(&self) -> u32 {
        MARGIN
    }
    pub fn image_height_px(&self) -> u32 {
        self.height_px - 2 * MARGIN
    }
    /// Width of the printable area in pixels.
    pub fn printable_width_px(&self) -> u32 {
        self.width_px - 2 * MARGIN
    }
}

/// Per-job settings that end up in the page header.
#[derive(Debug, Clone, Copy)]
pub struct PageSettings {
    /// Darkness (0 = printer default); written as `darkness << 2`.
    pub darkness: u8,
    pub toner_save: bool,
    /// Media type (plain paper = 0).
    pub media_type: u8,
    /// Fuser mode for the media (plain paper = 1).
    pub fuser_mode: u8,
}

impl Default for PageSettings {
    fn default() -> Self {
        PageSettings { darkness: 0, toner_save: false, media_type: 0, fuser_mode: 1 }
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
            .bytes(&[0, 0, 0])
            .u8(s.darkness << 2)
            .bytes(&[0x1C; 3])
            .u8(s.media_type)
            .u8(0x11) // 600 dpi
            .u8(4)
            .u8(0)
            .u8(1)
            .u8(1)
            .u8(2) // automatic image refinement
            .u8(s.toner_save as u8)
            .u16(0)
            .u16(MARGIN_PT)
            .u16(MARGIN_PT)
            .u16(line_bytes as u16)
            .u16(lines as u16)
            .u16(self.paper.printable_width_px() as u16)
            .u16(lines as u16)
            .u16(0)
            .u8(s.fuser_mode)
            .pad_to(44)
            .finish()
    }

    /// All packets for one page; `bitmap` is 1 bit per pixel, MSB first, 1 = black,
    /// `line_bytes` × `lines` (normally `image_line_bytes()` × `image_height_px()`).
    ///
    /// `D0A9` page setup (`D0A0`, `D0A4`, `D0A1`, `D0A2` in one packet), then each
    /// band of [`BAND_LINES`] lines compressed on its own in `C0A0` packets, then `C0A4`.
    pub fn encode_page(&self, bitmap: &[u8], line_bytes: usize, lines: usize) -> Vec<u8> {
        let mut setup = self.page_params(line_bytes, lines);
        setup.extend(PacketBuilder::new(op::SET_REF_DESC).bytes(&self.refs.to_bytes()).finish());
        setup.extend(crate::packet::simple(op::BEGIN_DATA));
        setup.extend(crate::packet::simple(op::END_PAGE));
        let mut out = PacketBuilder::new(op::PAGE_SETUP).bytes(&setup).finish();
        for start in (0..lines).step_by(BAND_LINES) {
            let n = BAND_LINES.min(lines - start);
            let band = &bitmap[start * line_bytes..(start + n) * line_bytes];
            let data = slim::compress(band, line_bytes, n, &self.refs);
            for chunk in data.chunks(MAX_VIDEO_CHUNK) {
                out.extend(PacketBuilder::new(op::VIDEO_DATA).bytes(chunk).finish());
            }
        }
        out.extend(crate::packet::simple(op::VIDEO_END));
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
        assert_eq!(a4.image_line_bytes(), 595);
        assert_eq!(a4.image_left_px(), 100);
        assert_eq!(a4.image_height_px(), 6778);
        assert_eq!(a4.printable_width_px(), 4724);
        assert_eq!(Paper::by_points(595.3, 841.9).unwrap().name, "A4");
    }

    #[test]
    fn page_header_layout() {
        let enc = PageEncoder::new(Paper::by_name("A4").unwrap(), PageSettings::default());
        let p = enc.page_params(595, 6778);
        assert_eq!(p.len(), 44);
        assert_eq!(&p[..8], &[0xA0, 0xD0, 44, 0, 0, 0, 0x30, 0x2A]);
        assert_eq!(p[8], 2); // A4
        assert_eq!(&p[12..16], &[0, 0x1C, 0x1C, 0x1C]);
        assert_eq!(&p[16..24], &[0, 0x11, 4, 0, 1, 1, 2, 0]);
        assert_eq!(&p[26..30], &[14, 0, 14, 0]);
        assert_eq!(u16::from_le_bytes([p[30], p[31]]), 595);
        assert_eq!(u16::from_le_bytes([p[32], p[33]]), 6778);
        assert_eq!(u16::from_le_bytes([p[34], p[35]]), 4724);
        assert_eq!(u16::from_le_bytes([p[36], p[37]]), 6778);
        assert_eq!(p[40], 1);
    }

    #[test]
    fn page_packet_sequence() {
        let enc = PageEncoder::new(Paper::by_name("A5").unwrap(), PageSettings::default());
        let (lb, lines) = (enc.paper.image_line_bytes(), 150);
        let bitmap = vec![0u8; lb * lines];
        let data = enc.encode_page(&bitmap, lb, lines);
        let pkts = split(&data).unwrap();
        let ops: Vec<u16> = pkts.iter().map(|p| header(p).unwrap().0).collect();
        // 150 lines = bands of 70 + 70 + 10
        assert_eq!(ops, [op::PAGE_SETUP, op::VIDEO_DATA, op::VIDEO_DATA, op::VIDEO_DATA, op::VIDEO_END]);
        let inner: Vec<u16> = split(&pkts[0][4..]).unwrap().iter().map(|p| header(p).unwrap().0).collect();
        assert_eq!(inner, [op::SET_PAGE_PARAMS, op::SET_REF_DESC, op::BEGIN_DATA, op::END_PAGE]);
        assert_eq!(pkts[0].len(), 4 + 44 + 12 + 4 + 4);
        let band = slim::decompress(&pkts[3][4..], lb, &enc.refs).unwrap();
        assert_eq!(band.len(), lb * 10);
    }
}
