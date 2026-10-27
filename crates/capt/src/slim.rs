//! Canon "Slim" raster compression, as produced by `lCaptCompEx` in libcanon_slim.
//!
//! A bit-oriented LZ-style coder over a 1-bit-per-pixel bitmap. Each byte is predicted
//! from an "up" reference (previous line, optionally shifted), a "left" reference
//! (bytes earlier in the stream) or a "far" reference; bytes that cannot be copied are
//! sent as literals through a 16-entry move-to-front cache. Codes are packed MSB-first
//! and every output byte is XORed with 0xBC. See `docs/re-notes.md`.

/// Compression references; sent to the printer verbatim in the `D0A4` packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefDesc {
    /// Left reference distance in bytes.
    pub left: u8,
    /// Alternate left reference distance in bytes (0 = none).
    pub alt_left: u8,
    /// Up reference distance in lines.
    pub up: u8,
    /// Alternate up reference distance in lines (0 = none).
    pub alt_up: u8,
    /// Horizontal shift of the up reference: ref = cur - up*W - shift.
    pub up_shift: i8,
    /// Horizontal shift of the alternate up reference.
    pub alt_up_shift: i8,
    /// Far reference distance in bytes within the current line (0 = none).
    pub far: u16,
}

impl RefDesc {
    /// `CompParamTbl_P1080` row 0 (also the driver's built-in default).
    pub const P1080: RefDesc = RefDesc {
        left: 1,
        alt_left: 4,
        up: 1,
        alt_up: 1,
        up_shift: 0,
        alt_up_shift: -7,
        far: 0,
    };

    pub fn to_bytes(&self) -> [u8; 8] {
        let f = self.far.to_le_bytes();
        [
            self.left,
            self.alt_left,
            self.up,
            self.alt_up,
            self.up_shift as u8,
            self.alt_up_shift as u8,
            f[0],
            f[1],
        ]
    }

    /// Byte offsets (negative = earlier) relative to the current position.
    fn offsets(&self, width: usize) -> Offsets {
        let w = width as isize;
        let up_off = |lines: u8, shift: i8| -(lines as isize) * w - shift as isize;
        Offsets {
            up: [
                Some(up_off(self.up, self.up_shift)),
                (self.alt_up != 0).then(|| up_off(self.alt_up, self.alt_up_shift)),
            ],
            left: [
                Some(-(self.left as isize)),
                (self.alt_left != 0).then(|| -(self.alt_left as isize)),
            ],
            far: (self.far != 0).then(|| -(self.far as isize)),
        }
    }
}

struct Offsets {
    up: [Option<isize>; 2],
    left: [Option<isize>; 2],
    far: Option<isize>,
}

/// Command codes `(value, bit length)` — `command_table`.
const CMD: [(u32, u32); 12] = [
    (0x1, 1),  // 0: copy from current up reference
    (0x1, 2),  // 1: literal from MRU cache (+4 bits)
    (0x3, 4),  // 2: switch up reference, copy
    (0x2, 4),  // 3: raw literal (+8 bits, inverted)
    (0x1, 4),  // 4: copy from current left reference
    (0x1, 5),  // 5: copy from far reference
    (0x1, 6),  // 6: switch left reference, copy
    (0x3, 8),  // 7: long-count escape
    (0x2, 8),  // 8: literal zero byte
    (0x0, 8),  // 9: (unused by the encoder)
    (0x7, 10), // 10: end of block
    (0x6, 10), // 11: (unused by the encoder)
];
const CMD_UP: usize = 0;
const CMD_CACHE: usize = 1;
const CMD_SWITCH_UP: usize = 2;
const CMD_RAW: usize = 3;
const CMD_LEFT: usize = 4;
const CMD_FAR: usize = 5;
const CMD_SWITCH_LEFT: usize = 6;
const CMD_ESCAPE: usize = 7;
const CMD_ZERO: usize = 8;
const CMD_EOB: usize = 10;

/// Count class codes — `count_table`, indexed by `floor(log2 n) + 1` (0 for n = 0).
const COUNT: [(u32, u32); 8] = [
    (0x0, 6),
    (0x3, 2),
    (0x2, 2),
    (0x1, 2),
    (0x1, 3),
    (0x1, 4),
    (0x1, 5),
    (0x1, 6),
];

const MAX_RUN: usize = 0x3FFF;
const XOR: u8 = 0xBC;

fn log2(n: u32) -> u32 {
    31 - n.leading_zeros()
}

struct BitWriter {
    acc: u32,
    bits: u32,
    out: Vec<u8>,
}

impl BitWriter {
    fn put(&mut self, code: u32, len: u32) {
        self.bits += len;
        self.acc |= code << (32 - self.bits);
        while self.bits >= 8 {
            self.out.push((self.acc >> 24) as u8 ^ XOR);
            self.acc <<= 8;
            self.bits -= 8;
        }
    }
    fn put_count(&mut self, n: u32) {
        debug_assert!(n < 128);
        if n == 0 {
            let (c, l) = COUNT[0];
            self.put(c, l);
            return;
        }
        let k = log2(n);
        let (c, l) = COUNT[k as usize + 1];
        self.put(c, l);
        if n >= 2 {
            self.put(n - (1 << k), k);
        }
    }
    fn put_copy(&mut self, cmd: usize, n: usize) {
        let n = n as u32;
        if n >= 128 {
            let (c, l) = CMD[CMD_ESCAPE];
            self.put(c, l);
            self.put_count((n >> 7) & 0x7F);
        }
        let (c, l) = CMD[cmd];
        self.put(c, l);
        self.put_count(n & 0x7F);
    }
    fn finish(mut self) -> Vec<u8> {
        let (c, l) = CMD[CMD_EOB];
        self.put(c, l);
        if self.bits > 0 {
            self.out.push((self.acc >> 24) as u8 ^ XOR);
        }
        while self.out.len() % 4 != 0 {
            self.out.push(XOR);
        }
        self.out
    }
}

/// 16-entry most-recently-used byte cache shared by encoder and decoder.
struct Cache {
    data: [u8; 16],
    head: usize,
    len: usize,
}

impl Cache {
    fn new() -> Self {
        Cache { data: [0; 16], head: 15, len: 0 }
    }
    fn find(&self, b: u8) -> Option<usize> {
        (0..self.len).find(|&k| self.data[(self.head + 16 - k) & 15] == b)
    }
    /// Move the entry at depth `k` to the front and return it.
    fn take(&mut self, k: usize) -> u8 {
        let mut j = (self.head + 16 - k) & 15;
        let b = self.data[j];
        while j != self.head {
            let next = (j + 1) & 15;
            self.data[j] = self.data[next];
            j = next;
        }
        self.data[self.head] = b;
        b
    }
    fn push(&mut self, b: u8) {
        self.head = (self.head + 1) & 15;
        self.data[self.head] = b;
        self.len = (self.len + 1).min(16);
    }
}

/// Compress `lines` lines of `width` bytes into one self-contained block.
pub fn compress(img: &[u8], width: usize, lines: usize, refs: &RefDesc) -> Vec<u8> {
    const MIN_MATCH: usize = 2;
    assert!(width > 8, "line width must exceed the alternate-up shift");
    assert!(img.len() >= width * lines);
    let offs = refs.offsets(width);
    let mut w = BitWriter { acc: 0, bits: 0, out: Vec::with_capacity(width * lines / 4 + 16) };
    let mut cache = Cache::new();
    let (mut cur_up, mut cur_left) = (0usize, 0usize);

    let run = |i: usize, line_end: usize, off: isize, floor: usize| -> usize {
        let r = i as isize + off;
        if r < floor as isize {
            return 0;
        }
        let r = r as usize;
        let max = (line_end - i).min(MAX_RUN);
        (0..max).take_while(|&k| img[r + k] == img[i + k]).count()
    };

    for line in 0..lines {
        let start = line * width;
        let end = start + width;
        let mut i = start;
        while i < end {
            let try_ref = |off: Option<isize>, floor: usize| off.map_or(0, |o| run(i, end, o, floor));

            let n = try_ref(offs.up[cur_up], 0);
            if n >= MIN_MATCH {
                w.put_copy(CMD_UP, n);
                i += n;
                continue;
            }
            let n = try_ref(offs.left[cur_left], 0);
            if n >= MIN_MATCH {
                w.put_copy(CMD_LEFT, n);
                i += n;
                continue;
            }
            let n = try_ref(offs.far, start);
            if n >= MIN_MATCH {
                w.put_copy(CMD_FAR, n);
                i += n;
                continue;
            }
            let n = try_ref(offs.up[1 - cur_up], 0);
            if n >= MIN_MATCH {
                cur_up = 1 - cur_up;
                w.put_copy(CMD_SWITCH_UP, n);
                i += n;
                continue;
            }
            let n = try_ref(offs.left[1 - cur_left], 0);
            if n >= MIN_MATCH {
                cur_left = 1 - cur_left;
                w.put_copy(CMD_SWITCH_LEFT, n);
                i += n;
                continue;
            }

            let b = img[i];
            if let Some(k) = cache.find(b) {
                cache.take(k);
                let (c, l) = CMD[CMD_CACHE];
                w.put(c, l);
                w.put(k as u32, 4);
            } else {
                if b == 0 {
                    let (c, l) = CMD[CMD_ZERO];
                    w.put(c, l);
                } else {
                    let (c, l) = CMD[CMD_RAW];
                    w.put(c, l);
                    w.put((b ^ 0xFF) as u32, 8);
                }
                cache.push(b);
            }
            i += 1;
        }
    }
    w.finish()
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
}

impl BitReader<'_> {
    fn bit(&mut self) -> Result<u32, String> {
        let byte = *self.data.get(self.pos).ok_or("compressed data ended early")? ^ XOR;
        let v = (byte >> (7 - self.bit)) & 1;
        self.bit += 1;
        if self.bit == 8 {
            self.bit = 0;
            self.pos += 1;
        }
        Ok(v as u32)
    }
    fn bits(&mut self, n: u32) -> Result<u32, String> {
        (0..n).try_fold(0, |acc, _| Ok(acc << 1 | self.bit()?))
    }
    fn code(&mut self, table: &[(u32, u32)]) -> Result<usize, String> {
        let (mut v, mut len) = (0u32, 0u32);
        while len < 10 {
            v = v << 1 | self.bit()?;
            len += 1;
            if let Some(i) = table.iter().position(|&(c, l)| l == len && c == v) {
                return Ok(i);
            }
        }
        Err(format!("invalid code {v:010b}"))
    }
    fn count(&mut self) -> Result<usize, String> {
        let idx = self.code(&COUNT)?;
        Ok(match idx {
            0 => 0,
            1 => 1,
            _ => {
                let k = idx as u32 - 1;
                ((1 << k) | self.bits(k)?) as usize
            }
        })
    }
}

/// Decode one block (used to verify the encoder; mirrors Canon's `lCaptDecode`).
pub fn decompress(data: &[u8], width: usize, refs: &RefDesc) -> Result<Vec<u8>, String> {
    let offs = refs.offsets(width);
    let mut r = BitReader { data, pos: 0, bit: 0 };
    let mut out: Vec<u8> = Vec::new();
    let mut cache = Cache::new();
    let (mut cur_up, mut cur_left) = (0usize, 0usize);

    loop {
        let mut cmd = r.code(&CMD)?;
        let mut hi = 0;
        if cmd == CMD_ESCAPE {
            hi = r.count()? << 7;
            cmd = r.code(&CMD)?;
        }
        let off = match cmd {
            CMD_EOB => return Ok(out),
            CMD_CACHE => {
                let k = r.bits(4)? as usize;
                out.push(cache.take(k));
                continue;
            }
            CMD_RAW => {
                let b = r.bits(8)? as u8 ^ 0xFF;
                cache.push(b);
                out.push(b);
                continue;
            }
            CMD_ZERO => {
                cache.push(0);
                out.push(0);
                continue;
            }
            CMD_UP => offs.up[cur_up],
            CMD_SWITCH_UP => {
                cur_up = 1 - cur_up;
                offs.up[cur_up]
            }
            CMD_LEFT => offs.left[cur_left],
            CMD_SWITCH_LEFT => {
                cur_left = 1 - cur_left;
                offs.left[cur_left]
            }
            CMD_FAR => offs.far,
            other => return Err(format!("unexpected command {other}")),
        }
        .ok_or("copy from a reference that does not exist")?;
        let n = hi + r.count()?;
        for _ in 0..n {
            let src = out.len() as isize + off;
            if src < 0 {
                return Err("copy before start of block".into());
            }
            out.push(out[src as usize]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(img: &[u8], width: usize) {
        let lines = img.len() / width;
        let packed = compress(img, width, lines, &RefDesc::P1080);
        assert_eq!(packed.len() % 4, 0);
        let back = decompress(&packed, width, &RefDesc::P1080).unwrap();
        assert_eq!(back, img, "roundtrip mismatch (width {width})");
    }

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u8 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 33) as u8
        }
    }

    #[test]
    fn blank_page_is_tiny() {
        let img = vec![0u8; 592 * 200];
        let packed = compress(&img, 592, 200, &RefDesc::P1080);
        assert!(packed.len() < 1200, "{} bytes", packed.len());
        roundtrip(&img, 592);
    }

    #[test]
    fn roundtrips_noise_and_patterns() {
        let mut rng = Lcg(7);
        let width = 64;
        let mut img = vec![0u8; width * 40];
        for (i, b) in img.iter_mut().enumerate() {
            *b = match (i / width) % 4 {
                0 => rng.next(),
                1 => if i % 7 == 0 { 0xFF } else { 0 },
                2 => (i % 3) as u8 * 0x55,
                _ => rng.next() & 0x81,
            };
        }
        roundtrip(&img, width);
    }

    #[test]
    fn roundtrips_long_runs() {
        let width = 4096; // runs over 128 and up to the 0x3FFF cap
        let mut img = vec![0xFFu8; width * 3];
        img[width + 1000] = 0x12;
        roundtrip(&img, width);
    }

    #[test]
    fn single_raw_byte_encoding() {
        // One line of 16 bytes: 0xAA then zeros.
        let mut img = vec![0u8; 16];
        img[0] = 0xAA;
        let packed = compress(&img, 16, 1, &RefDesc::P1080);
        // First code: raw literal "0010" + !0xAA (0x55) = 0010 0101 0101 ...
        assert_eq!(packed[0] ^ 0xBC, 0b0010_0101);
        roundtrip(&img, 16);
    }

    #[test]
    fn ref_desc_bytes_match_canon_default() {
        assert_eq!(RefDesc::P1080.to_bytes(), [0x01, 0x04, 0x01, 0x01, 0x00, 0xF9, 0x00, 0x00]);
    }
}
