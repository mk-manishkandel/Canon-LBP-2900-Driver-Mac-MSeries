//! 8-bit grey → 1-bit (1 = black) conversion, one line at a time.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Floyd–Steinberg error diffusion (photos, smooth greys).
    ErrorDiffusion,
    /// 8×8 Bayer ordered dither (compresses better, slightly coarser).
    Ordered,
    /// Plain 50 % threshold (crisp text, no greys).
    Threshold,
}

impl Method {
    pub fn parse(s: &str) -> Option<Method> {
        match s.to_ascii_lowercase().as_str() {
            "errordiffusion" | "diffusion" | "fs" => Some(Method::ErrorDiffusion),
            "ordered" | "pattern" => Some(Method::Ordered),
            "threshold" | "none" => Some(Method::Threshold),
            _ => None,
        }
    }
}

const BAYER8: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

pub struct Halftoner {
    method: Method,
    /// Added to every darkness value (ink) before thresholding; -2..=2 steps of 20.
    darkness: i32,
    width: usize,
    err: Vec<i32>,
    line: usize,
}

impl Halftoner {
    pub fn new(method: Method, darkness: i32, width: usize) -> Self {
        Halftoner { method, darkness: darkness * 20, width, err: vec![0; width + 2], line: 0 }
    }

    /// `grey` holds `width` luminance values (0 = black, 255 = white); writes
    /// `ceil(width / 8)` bytes to `out`.
    pub fn line(&mut self, grey: &[u8], out: &mut [u8]) {
        out.fill(0);
        let ink = |v: u8| (255 - v as i32 + self.darkness).clamp(0, 255);
        match self.method {
            Method::Threshold => {
                for x in 0..self.width {
                    if ink(grey[x]) >= 128 {
                        out[x / 8] |= 0x80 >> (x % 8);
                    }
                }
            }
            Method::Ordered => {
                let row = &BAYER8[self.line % 8];
                for x in 0..self.width {
                    let t = row[x % 8] as i32 * 4 + 2;
                    if ink(grey[x]) > t {
                        out[x / 8] |= 0x80 >> (x % 8);
                    }
                }
            }
            Method::ErrorDiffusion => {
                // Serpentine scan; err[x + 1] is the error carried into pixel x.
                let mut next = vec![0i32; self.width + 2];
                let ltr = self.line % 2 == 0;
                for step in 0..self.width {
                    let x = if ltr { step } else { self.width - 1 - step };
                    let v = ink(grey[x]) + self.err[x + 1];
                    let black = v >= 128;
                    if black {
                        out[x / 8] |= 0x80 >> (x % 8);
                    }
                    let e = v - if black { 255 } else { 0 };
                    let (fwd, back) = if ltr { (x + 2, x) } else { (x, x + 2) };
                    self.err[fwd] += e * 7 / 16;
                    next[back] += e * 3 / 16;
                    next[x + 1] += e * 5 / 16;
                    next[fwd] += e / 16;
                }
                self.err = next;
            }
        }
        self.line += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_stays_white_black_stays_black() {
        for m in [Method::ErrorDiffusion, Method::Ordered, Method::Threshold] {
            let mut h = Halftoner::new(m, 0, 16);
            let mut out = [0u8; 2];
            h.line(&[255; 16], &mut out);
            assert_eq!(out, [0, 0], "{m:?}");
            h.line(&[0; 16], &mut out);
            assert_eq!(out, [0xFF, 0xFF], "{m:?}");
        }
    }

    #[test]
    fn mid_grey_is_about_half_black() {
        let mut h = Halftoner::new(Method::ErrorDiffusion, 0, 256);
        let mut out = [0u8; 32];
        let mut ones = 0;
        for _ in 0..16 {
            h.line(&[128; 256], &mut out);
            ones += out.iter().map(|b| b.count_ones()).sum::<u32>();
        }
        let ratio = ones as f32 / (256.0 * 16.0);
        assert!((0.4..0.6).contains(&ratio), "{ratio}");
    }
}
