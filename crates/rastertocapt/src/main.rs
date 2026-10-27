//! CUPS filter `rastertocapt`: converts CUPS raster (from macOS's PDF rasterizer) into
//! the LBP2900 job stream read by the `lbp2900` backend.
//!
//! Usage (CUPS): rastertocapt job user title copies options [file]

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::process::ExitCode;

use capt::halftone::{Halftoner, Method};
use capt::page::{PageEncoder, PageSettings, Paper, DPI};
use capt::raster::{PageHeader, RasterReader, CSPACE_K};
use capt::spool;

struct Options {
    halftone: Method,
    darkness: i32,
    page_size: Option<String>,
}

/// Parse the CUPS options string (`key=value key2 noKey3 ...`).
fn parse_options(s: &str) -> Options {
    let mut o = Options { halftone: Method::ErrorDiffusion, darkness: 0, page_size: None };
    for tok in s.split_whitespace() {
        let (k, v) = tok.split_once('=').unwrap_or((tok, "true"));
        match k {
            "LBPHalftone" => o.halftone = Method::parse(v).unwrap_or(o.halftone),
            "LBPDarkness" => o.darkness = v.parse::<i32>().unwrap_or(0).clamp(-2, 2),
            "PageSize" | "media" => o.page_size = Some(v.to_string()),
            _ => {}
        }
    }
    o
}

/// Convert one raster line to 8-bit luminance (0 = black, 255 = white).
fn to_grey(h: &PageHeader, line: &[u8], grey: &mut [u8]) {
    let w = h.width as usize;
    let k = h.color_space == CSPACE_K;
    match (h.bits_per_pixel, h.color_order) {
        (1, _) => {
            for (x, g) in grey.iter_mut().enumerate().take(w) {
                let bit = line[x / 8] & (0x80 >> (x % 8)) != 0;
                *g = if bit == k { 0 } else { 255 };
            }
        }
        (8, _) => {
            for (x, g) in grey.iter_mut().enumerate().take(w) {
                *g = if k { 255 - line[x] } else { line[x] };
            }
        }
        (24, 0) => {
            for (x, g) in grey.iter_mut().enumerate().take(w) {
                let p = &line[x * 3..x * 3 + 3];
                *g = ((p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000) as u8;
            }
        }
        (32, 0) => {
            // CMYK: approximate darkness from K plus average CMY.
            for (x, g) in grey.iter_mut().enumerate().take(w) {
                let p = &line[x * 4..x * 4 + 4];
                let ink = (p[3] as u32 + (p[0] as u32 + p[1] as u32 + p[2] as u32) / 3).min(255);
                *g = 255 - ink as u8;
            }
        }
        _ => grey.fill(255),
    }
}

fn convert<R: Read, W: Write>(input: R, out: &mut W, opts: &Options) -> io::Result<u32> {
    let mut raster = RasterReader::new(input)?;
    spool::write_header(out)?;
    let mut pages = 0;
    while let Some(h) = raster.next_page()? {
        pages += 1;
        let paper = Paper::by_points(h.page_size[0] as f32, h.page_size[1] as f32)
            .or_else(|| Paper::by_name(&h.page_size_name))
            .or_else(|| opts.page_size.as_deref().and_then(Paper::by_name))
            .unwrap_or_else(|| {
                eprintln!("WARNING: unknown page size {:?}, using A4", h.page_size);
                Paper::by_name("A4").unwrap()
            });
        let res = h.hw_resolution[0].max(1);
        let res_y = h.hw_resolution[1].max(1);
        eprintln!(
            "INFO: page {pages}: {} {}x{} px at {res}x{res_y} dpi, {} bpp, colour space {}",
            paper.name, h.width, h.height, h.bits_per_pixel, h.color_space
        );

        // Where the raster's top-left pixel sits on the page, in 600 dpi pixels.
        let full_page = (h.width as u64 * DPI as u64 / res as u64) as u32 + 16 >= paper.width_px;
        let (ox, oy) = if full_page {
            (0i64, 0i64)
        } else {
            let left = h.imaging_bbox[0] as i64 * DPI as i64 / 72;
            let top = (h.page_size[1] as i64 - h.imaging_bbox[3] as i64) * DPI as i64 / 72;
            (left, top)
        };

        let width = paper.image_width_px() as usize;
        let lines = paper.image_height_px() as usize;
        let line_bytes = paper.image_line_bytes();
        let mut bitmap = vec![0u8; line_bytes * lines];
        let mut halftoner = Halftoner::new(opts.halftone, opts.darkness, width);

        let mut raw = vec![0u8; h.bytes_per_line as usize];
        let mut grey_src = vec![255u8; h.width as usize];
        let mut grey = vec![255u8; width];
        let mut next_row = 0u32; // next raster row to read
        let mut have_row: Option<u32> = None;

        // Precompute raster column for every target column.
        let cols: Vec<Option<usize>> = (0..width)
            .map(|x| {
                let page_x = x as i64 + paper.image_left_px() as i64 - ox;
                let rx = page_x * res as i64 / DPI as i64;
                (page_x >= 0 && rx < h.width as i64).then_some(rx as usize)
            })
            .collect();

        for y in 0..lines {
            let page_y = y as i64 + paper.image_top_px() as i64 - oy;
            let ry = page_y * res_y as i64 / DPI as i64;
            if page_y >= 0 && ry < h.height as i64 {
                let ry = ry as u32;
                while have_row != Some(ry) {
                    raster.read_line(&mut raw)?;
                    have_row = Some(next_row);
                    next_row += 1;
                }
                to_grey(&h, &raw, &mut grey_src);
                for (g, c) in grey.iter_mut().zip(&cols) {
                    *g = c.map_or(255, |c| grey_src[c]);
                }
            } else {
                grey.fill(255);
            }
            halftoner.line(&grey, &mut bitmap[y * line_bytes..(y + 1) * line_bytes]);
        }

        let enc = PageEncoder::new(paper, PageSettings::default());
        let packets = enc.encode_page(&bitmap, line_bytes, lines);
        spool::write_page(out, &packets)?;
        eprintln!("PAGE: {pages} 1");
    }
    spool::write_end(out)?;
    Ok(pages)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 6 || args.len() > 7 {
        eprintln!("Usage: rastertocapt job user title copies options [file]");
        return ExitCode::from(1);
    }
    let opts = parse_options(&args[5]);
    let stdout = io::stdout();
    let mut out = BufWriter::with_capacity(1 << 20, stdout.lock());
    let result = if let Some(path) = args.get(6) {
        match File::open(path) {
            Ok(f) => convert(BufReader::new(f), &mut out, &opts),
            Err(e) => Err(e),
        }
    } else {
        convert(BufReader::new(io::stdin().lock()), &mut out, &opts)
    };
    match result.and_then(|n| out.flush().map(|_| n)) {
        Ok(n) => {
            eprintln!("INFO: rastertocapt: {n} page(s) prepared");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("ERROR: rastertocapt: {e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capt::packet::{self, op};
    use capt::slim::{self, RefDesc};

    /// One A4 page of 8-bit grey (v3, little-endian) with a black square at
    /// page pixels (1000..1100, 2000..2100).
    fn a4_raster() -> Vec<u8> {
        let (w, h) = (4960u32, 7014u32);
        let mut s = b"3SaR".to_vec();
        let mut hdr = vec![0u8; 1796];
        let mut put = |off: usize, v: u32| hdr[off..off + 4].copy_from_slice(&v.to_le_bytes());
        put(276, 600);
        put(280, 600);
        put(352, 595);
        put(356, 842);
        put(372, w);
        put(376, h);
        put(384, 8);
        put(388, 8);
        put(392, w);
        s.extend(hdr);
        for y in 0..h {
            for x in 0..w {
                let black = (1000..1100).contains(&x) && (2000..2100).contains(&y);
                s.push(if black { 0 } else { 255 });
            }
        }
        s
    }

    #[test]
    fn raster_to_job_stream_end_to_end() {
        let opts = parse_options("LBPHalftone=threshold");
        let mut out = Vec::new();
        assert_eq!(convert(&a4_raster()[..], &mut out, &opts).unwrap(), 1);

        let mut spool = spool::SpoolReader::new(&out[..]).unwrap();
        let page = spool.next_page().unwrap().unwrap();
        let pkts = packet::split(&page).unwrap();
        assert_eq!(packet::header(pkts[0]).unwrap().0, op::PAGE_SETUP);
        let params = &pkts[0][4..];
        assert_eq!(packet::header(params).unwrap().0, op::SET_PAGE_PARAMS);
        let lb = u16::from_le_bytes([params[30], params[31]]) as usize;
        // Each band is compressed on its own and fits in one packet here.
        let bitmap: Vec<u8> = pkts
            .iter()
            .filter(|p| packet::header(p).unwrap().0 == op::VIDEO_DATA)
            .flat_map(|p| slim::decompress(&p[4..], lb, &RefDesc::P1080).unwrap())
            .collect();
        let a4 = Paper::by_name("A4").unwrap();
        assert_eq!(bitmap.len(), lb * a4.image_height_px() as usize);
        let px = |x: usize, y: usize| bitmap[y * lb + x / 8] & (0x80 >> (x % 8)) != 0;
        // Page (1050, 2050) → image (1050 - 100, 2050 - 118).
        let (left, top) = (a4.image_left_px() as usize, a4.image_top_px() as usize);
        assert!(px(1050 - left, 2050 - top));
        assert!(!px(500, 500));
        let black: u32 = bitmap.iter().map(|b| b.count_ones()).sum();
        assert_eq!(black, 100 * 100);
        assert!(spool.next_page().unwrap().is_none());
    }
}
