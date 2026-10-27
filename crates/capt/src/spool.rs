//! Job stream between the CUPS filter (`rastertocapt`) and the backend (`lbp2900`).
//!
//! `"LBP2900J" u32 version` then records `u8 kind, u32 len, payload`. A page record
//! holds the page's complete CAPT packet sequence (D0A0 … D0A2); the backend adds the
//! engine handshake around it.

use std::io::{self, Read, Write};

pub const MAGIC: &[u8; 8] = b"LBP2900J";
pub const VERSION: u32 = 1;

const KIND_PAGE: u8 = 1;
const KIND_END: u8 = 2;

pub fn write_header<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(MAGIC)?;
    w.write_all(&VERSION.to_le_bytes())
}

pub fn write_page<W: Write>(w: &mut W, packets: &[u8]) -> io::Result<()> {
    w.write_all(&[KIND_PAGE])?;
    w.write_all(&(packets.len() as u32).to_le_bytes())?;
    w.write_all(packets)?;
    w.flush()
}

pub fn write_end<W: Write>(w: &mut W) -> io::Result<()> {
    w.write_all(&[KIND_END, 0, 0, 0, 0])?;
    w.flush()
}

pub struct SpoolReader<R: Read> {
    input: R,
    done: bool,
}

impl<R: Read> SpoolReader<R> {
    pub fn new(mut input: R) -> io::Result<Self> {
        let mut head = [0u8; 12];
        input.read_exact(&mut head)?;
        if &head[..8] != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "input is not an LBP2900 job stream"));
        }
        let ver = u32::from_le_bytes(head[8..12].try_into().unwrap());
        if ver != VERSION {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unsupported job stream version {ver}")));
        }
        Ok(SpoolReader { input, done: false })
    }

    /// Next page's packets, or `None` at end of job.
    pub fn next_page(&mut self) -> io::Result<Option<Vec<u8>>> {
        if self.done {
            return Ok(None);
        }
        let mut rec = [0u8; 5];
        match self.input.read_exact(&mut rec) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                self.done = true;
                return Ok(None);
            }
            Err(e) => return Err(e),
        }
        let len = u32::from_le_bytes(rec[1..5].try_into().unwrap()) as usize;
        match rec[0] {
            KIND_PAGE => {
                let mut buf = vec![0u8; len];
                self.input.read_exact(&mut buf)?;
                Ok(Some(buf))
            }
            KIND_END => {
                self.done = true;
                Ok(None)
            }
            k => Err(io::Error::new(io::ErrorKind::InvalidData, format!("unknown record kind {k}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut buf = Vec::new();
        write_header(&mut buf).unwrap();
        write_page(&mut buf, &[1, 2, 3]).unwrap();
        write_page(&mut buf, &[4]).unwrap();
        write_end(&mut buf).unwrap();
        let mut r = SpoolReader::new(&buf[..]).unwrap();
        assert_eq!(r.next_page().unwrap(), Some(vec![1, 2, 3]));
        assert_eq!(r.next_page().unwrap(), Some(vec![4]));
        assert_eq!(r.next_page().unwrap(), None);
    }
}
