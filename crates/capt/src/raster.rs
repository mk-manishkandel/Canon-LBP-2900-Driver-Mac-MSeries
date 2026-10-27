//! Minimal CUPS raster reader (v1, v2 compressed, v3; either byte order).

use std::io::{self, Read};

pub const CSPACE_W: u32 = 0;
pub const CSPACE_K: u32 = 3;
pub const CSPACE_SW: u32 = 18;

#[derive(Debug, Clone)]
pub struct PageHeader {
    pub hw_resolution: [u32; 2],
    pub imaging_bbox: [u32; 4],
    pub num_copies: u32,
    pub page_size: [u32; 2],
    pub width: u32,
    pub height: u32,
    pub media_type: u32,
    pub bits_per_color: u32,
    pub bits_per_pixel: u32,
    pub bytes_per_line: u32,
    pub color_order: u32,
    pub color_space: u32,
    pub page_size_name: String,
}

impl PageHeader {
    /// Value of a white pixel in this colour space (used by v2 "clear to end of line").
    fn clear_byte(&self) -> u8 {
        match self.color_space {
            CSPACE_W | CSPACE_SW | 1 | 17 | 19 | 20 => 0xFF,
            _ => 0x00,
        }
    }
}

const HEADER_LEN: usize = 1796;

pub struct RasterReader<R: Read> {
    input: R,
    big_endian: bool,
    compressed: bool,
    header: Option<PageHeader>,
    lines_left: u32,
    // v2 line repeat state
    line_buf: Vec<u8>,
    repeat: u32,
}

impl<R: Read> RasterReader<R> {
    pub fn new(mut input: R) -> io::Result<Self> {
        let mut sync = [0u8; 4];
        input.read_exact(&mut sync)?;
        let (big_endian, compressed) = match &sync {
            b"RaSt" | b"RaS3" => (true, false),
            b"tSaR" | b"3SaR" => (false, false),
            b"RaS2" => (true, true),
            b"2SaR" => (false, true),
            _ => {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("not a CUPS raster stream: {sync:02X?}")))
            }
        };
        Ok(RasterReader { input, big_endian, compressed, header: None, lines_left: 0, line_buf: Vec::new(), repeat: 0 })
    }

    /// Read the next page header; `None` at end of stream.
    pub fn next_page(&mut self) -> io::Result<Option<PageHeader>> {
        // Skip any unread lines of the previous page.
        if self.header.is_some() {
            let mut line = vec![0u8; self.header.as_ref().unwrap().bytes_per_line as usize];
            while self.lines_left > 0 {
                self.read_line(&mut line)?;
            }
        }
        let mut h = vec![0u8; HEADER_LEN];
        let mut got = 0;
        while got < HEADER_LEN {
            let n = self.input.read(&mut h[got..])?;
            if n == 0 {
                return if got == 0 { Ok(None) } else { Err(io::ErrorKind::UnexpectedEof.into()) };
            }
            got += n;
        }
        let u = |off: usize| {
            let b = [h[off], h[off + 1], h[off + 2], h[off + 3]];
            if self.big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
        };
        let name_bytes = &h[1732..1796];
        let name_end = name_bytes.iter().position(|&b| b == 0).unwrap_or(64);
        let header = PageHeader {
            hw_resolution: [u(276), u(280)],
            imaging_bbox: [u(284), u(288), u(292), u(296)],
            num_copies: u(340),
            page_size: [u(352), u(356)],
            width: u(372),
            height: u(376),
            media_type: u(380),
            bits_per_color: u(384),
            bits_per_pixel: u(388),
            bytes_per_line: u(392),
            color_order: u(396),
            color_space: u(400),
            page_size_name: String::from_utf8_lossy(&name_bytes[..name_end]).into_owned(),
        };
        if header.bytes_per_line == 0 || header.bytes_per_line > 1 << 20 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad bytes_per_line"));
        }
        self.lines_left = header.height;
        self.repeat = 0;
        self.line_buf = vec![0; header.bytes_per_line as usize];
        self.header = Some(header.clone());
        Ok(Some(header))
    }

    /// Read one line of the current page into `out` (`bytes_per_line` long).
    pub fn read_line(&mut self, out: &mut [u8]) -> io::Result<()> {
        let h = self.header.as_ref().ok_or_else(|| io::Error::other("no page"))?;
        if self.lines_left == 0 {
            return Err(io::Error::other("read past end of page"));
        }
        self.lines_left -= 1;
        if !self.compressed {
            return self.input.read_exact(out);
        }
        if self.repeat > 0 {
            self.repeat -= 1;
            out.copy_from_slice(&self.line_buf);
            return Ok(());
        }
        let bpl = h.bytes_per_line as usize;
        let bpp = if h.color_order == 0 {
            (h.bits_per_pixel as usize).div_ceil(8).max(1)
        } else {
            (h.bits_per_color as usize).div_ceil(8).max(1)
        };
        let clear = h.clear_byte();
        let mut byte = [0u8; 1];
        self.input.read_exact(&mut byte)?;
        self.repeat = byte[0] as u32;
        let mut pos = 0;
        while pos < bpl {
            self.input.read_exact(&mut byte)?;
            let c = byte[0];
            if c == 128 {
                self.line_buf[pos..].fill(clear);
                break;
            } else if c > 128 {
                let n = ((257 - c as usize) * bpp).min(bpl - pos);
                self.input.read_exact(&mut self.line_buf[pos..pos + n])?;
                pos += n;
            } else {
                let mut px = vec![0u8; bpp];
                self.input.read_exact(&mut px)?;
                for _ in 0..=c as usize {
                    let n = bpp.min(bpl - pos);
                    self.line_buf[pos..pos + n].copy_from_slice(&px[..n]);
                    pos += n;
                    if pos >= bpl {
                        break;
                    }
                }
            }
        }
        out.copy_from_slice(&self.line_buf);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut h = vec![0u8; HEADER_LEN];
        let mut put = |off: usize, v: u32| h[off..off + 4].copy_from_slice(&v.to_le_bytes());
        put(276, 600);
        put(280, 600);
        put(352, 595);
        put(356, 842);
        put(372, width);
        put(376, height);
        put(384, 8);
        put(388, 8);
        put(392, width);
        put(400, CSPACE_W);
        h
    }

    #[test]
    fn reads_v2_little_endian() {
        let mut s = b"2SaR".to_vec();
        s.extend(header_bytes(4, 3));
        // line group 1: repeat 1 extra line, 2 literal px then repeat 0x10 twice
        s.extend([1, 255, 0x01, 0x02, 1, 0x10]);
        // line group 2: clear to end of line
        s.extend([0, 128]);
        let mut r = RasterReader::new(&s[..]).unwrap();
        let h = r.next_page().unwrap().unwrap();
        assert_eq!((h.width, h.height, h.page_size), (4, 3, [595, 842]));
        let mut line = [0u8; 4];
        r.read_line(&mut line).unwrap();
        assert_eq!(line, [1, 2, 0x10, 0x10]);
        r.read_line(&mut line).unwrap();
        assert_eq!(line, [1, 2, 0x10, 0x10]);
        r.read_line(&mut line).unwrap();
        assert_eq!(line, [0xFF; 4]);
        assert!(r.next_page().unwrap().is_none());
    }
}
