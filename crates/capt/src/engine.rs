//! Printer conversation for a job on the LBP2900.
//!
//! The sequence follows captdriver's LBP2900 support, which prints on real hardware,
//! with the macOS fix from duy12i1i7/canon-LBP2900-for-macOS: status is always read
//! as the extended `A0A8` record, because the page counters only live there. See
//! `docs/re-notes.md`.

use std::io;
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::packet::{self, hex, op, PacketBuilder, Response};

/// Byte transport to the printer (USB bulk endpoints in the backend).
pub trait Transport {
    fn write(&mut self, data: &[u8], timeout: Duration) -> io::Result<()>;
    /// One read of up to `buf.len()` bytes.
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
}

/// `SetLed` payload sent when a job starts (captdriver `lbp3010_gpio_init`).
const LED_JOB: [u8; 16] = [0x13, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
/// `SetLed` payload sent when a job is cancelled (captdriver `lbp2900_gpio_init`).
const LED_IDLE: [u8; 12] = [0; 12];
/// `GoOnline` payload used to wake an uninitialised engine.
const WAKE: [u8; 16] = [0xEE, 0xDB, 0xEA, 0xAD, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// `SetJobInfo` stages.
const STAGE_BEGIN: u8 = 1;
const STAGE_RECEIVED: u8 = 2;
const STAGE_END: u8 = 4;
const STAGE_PRINTING: u8 = 6;

/// Data packets sent between two busy checks.
const DATA_PACKETS_PER_CHECK: u32 = 16;

#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// Status poll interval while waiting for the printer.
    pub poll_interval: Duration,
    /// Give up waiting for one condition (ready, page received, page out) after this.
    pub page_timeout: Duration,
    pub read_timeout: Duration,
    pub write_timeout: Duration,
    pub user: String,
    pub host: String,
    pub title: String,
    /// One-way link (network print server): requests are written but no response is
    /// read, every wait succeeds at once and the job number is `blind_session`.
    pub blind: bool,
    pub blind_session: u16,
}

impl Default for EngineOptions {
    fn default() -> Self {
        EngineOptions {
            poll_interval: Duration::from_millis(100),
            page_timeout: Duration::from_secs(20),
            read_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(60),
            user: "user".into(),
            host: "mac".into(),
            title: "document".into(),
            blind: false,
            blind_session: 0,
        }
    }
}

/// Decoded printer status (`A0A8` extended or `E0A0` basic).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub words: [u16; 7],
    pub page_decoding: u16,
    pub page_printing: u16,
    pub page_out: u16,
    pub page_completed: u16,
    pub page_received: u16,
}

impl Status {
    /// The record starts right after the 4-byte header, so it includes the two bytes
    /// [`Response`] keeps as `flags`.
    pub fn parse(r: &Response) -> Status {
        let mut s = r.flags.to_vec();
        s.extend_from_slice(&r.body);
        let w = |i: usize| s.get(i..i + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
        Status {
            words: [w(0), w(8), w(10), w(12), w(24), w(30), w(38)],
            page_decoding: w(14),
            page_printing: w(16),
            page_out: w(18),
            page_completed: w(20),
            page_received: w(34),
        }
    }
    pub fn busy(&self) -> bool {
        self.words[0] & 1 << 7 != 0
    }
    pub fn uninitialised(&self) -> bool {
        self.words[0] & (1 << 5 | 1 << 4) != 0
    }
    pub fn buffer_full(&self) -> bool {
        self.words[0] & 1 << 2 != 0
    }
    pub fn no_paper(&self) -> bool {
        self.words[0] & 1 << 1 != 0 || self.words[1] & 1 << 14 != 0
    }
    pub fn printing(&self) -> bool {
        self.words[1] & (1 << 2 | 1 << 7) != 0
    }
}

pub struct Engine<T: Transport> {
    io: T,
    pub opts: EngineOptions,
    log: Box<dyn FnMut(&str)>,
    job: Option<u16>,
    /// Page counter reported in blind mode, where the printer cannot be asked.
    blind_page: u16,
}

impl<T: Transport> Engine<T> {
    pub fn new(io: T, opts: EngineOptions, log: Box<dyn FnMut(&str)>) -> Self {
        Engine { io, opts, log, job: None, blind_page: 0 }
    }

    fn log(&mut self, msg: String) {
        (self.log)(&msg);
    }

    /// Write packets that the printer does not answer (page data).
    pub fn send(&mut self, pkt: &[u8]) -> io::Result<()> {
        if let Some((code, len)) = packet::header(pkt) {
            let shown = &pkt[..pkt.len().min(48)];
            self.log(format!(">> {code:04X} {} len={len} {}", op::name(code), hex(shown)));
        }
        self.io.write(pkt, self.opts.write_timeout)
    }

    /// Send a request and read its response.
    pub fn request(&mut self, pkt: &[u8]) -> io::Result<Response> {
        let (code, _) = packet::header(pkt).unwrap_or((0, 0));
        self.log(format!(">> {code:04X} {} {}", op::name(code), hex(pkt)));
        self.io.write(pkt, self.opts.write_timeout)?;
        if self.opts.blind {
            let body = if code == op::RESERVE { self.opts.blind_session.to_le_bytes().to_vec() } else { vec![] };
            return Ok(Response { opcode: code, flags: [0, 0], body });
        }
        let mut raw = Vec::new();
        let mut buf = [0u8; 512];
        let deadline = std::time::Instant::now() + self.opts.read_timeout;
        loop {
            let n = self.io.read(&mut buf, self.opts.read_timeout)?;
            raw.extend_from_slice(&buf[..n]);
            if let Some((_, len)) = packet::header(&raw) {
                // Some replies carry their length in BCD.
                if raw.len() >= 6 && (raw.len() >= len.max(6) || Some(raw.len()) == bcd(len)) {
                    break;
                }
            }
            if std::time::Instant::now() > deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "response incomplete"));
            }
        }
        self.log(format!("<< {}", hex(&raw)));
        let resp = Response::parse(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if resp.opcode != code {
            self.log(format!("!! response opcode {:04X} does not match request {code:04X}", resp.opcode));
        }
        Ok(resp)
    }

    pub fn device_id(&mut self) -> io::Result<String> {
        let r = self.request(&packet::simple(op::DEVICE_ID))?;
        Ok(String::from_utf8_lossy(&r.body).trim_matches(char::from(0)).to_string())
    }

    /// Extended status (`A0A8`): flags plus the page counters.
    pub fn xstatus(&mut self) -> io::Result<Status> {
        if self.opts.blind {
            let p = self.blind_page;
            return Ok(Status { page_decoding: p, page_printing: p, page_out: p, page_completed: p, page_received: p, ..Default::default() });
        }
        let r = self.request(&packet::simple(op::EXTENDED_STATUS))?;
        let s = Status::parse(&r);
        self.log(format!(
            "status {:04X?} pages decoding/printing/out/completed/received {}/{}/{}/{}/{}",
            s.words, s.page_decoding, s.page_printing, s.page_out, s.page_completed, s.page_received
        ));
        Ok(s)
    }

    /// Poll the extended status until `done` holds, or give up after `page_timeout`
    /// and carry on with the last status (as captdriver does).
    fn wait_for(&mut self, what: &str, done: impl Fn(&Status) -> bool) -> io::Result<Status> {
        let max_polls = (self.opts.page_timeout.as_millis() / self.opts.poll_interval.as_millis().max(1)) as u32;
        let mut polls = 0;
        loop {
            let s = self.xstatus()?;
            if done(&s) {
                return Ok(s);
            }
            if polls >= max_polls {
                self.log(format!("waiting for {what} timed out; continuing"));
                return Ok(s);
            }
            polls += 1;
            sleep(self.opts.poll_interval);
        }
    }

    fn wait_ready(&mut self) -> io::Result<Status> {
        self.wait_for("printer ready", |s| !s.busy())
    }

    /// Basic status (`E0A0`) busy check between data packets.
    fn wait_ready_basic(&mut self) -> io::Result<()> {
        if self.opts.blind {
            return Ok(());
        }
        for _ in 0..(self.opts.page_timeout.as_millis() / self.opts.poll_interval.as_millis().max(1)) {
            let s = Status::parse(&self.request(&packet::simple(op::BASIC_STATUS))?);
            if !s.busy() {
                return Ok(());
            }
            sleep(self.opts.poll_interval);
        }
        self.log("printer stayed busy; continuing".into());
        Ok(())
    }

    pub fn reserve(&mut self) -> io::Result<u16> {
        let pkt = PacketBuilder::new(op::RESERVE).u16(0).u16(0x1E).pad_to(12).finish();
        let r = self.request(&pkt)?;
        let job = r.u16_at(0).unwrap_or(0);
        self.log(format!("reserved printer, job {job:#06X}"));
        self.job = Some(job);
        Ok(job)
    }

    pub fn release(&mut self) -> io::Result<()> {
        if let Some(job) = self.job.take() {
            self.request(&PacketBuilder::new(op::RELEASE).u16(job).finish())?;
        }
        Ok(())
    }

    /// `E1A1` job information for one stage of the job.
    fn job_info(&mut self, stage: u8, page: u16) -> io::Result<()> {
        let (year, month, day, h, m, s) = utc_now();
        let pkt = PacketBuilder::new(op::SET_JOB_INFO)
            .u32(0)
            .u16(page)
            .u16(0)
            .u16(0) // host name length
            .u16(0) // user name length
            .u16(0) // document name length
            .u16(0)
            .u8(stage)
            .u8(1)
            .u16(self.job.unwrap_or(0))
            .u16(-60i16 as u16)
            .u16(-120i16 as u16)
            .u16(year - 1900)
            .u8(month - 1)
            .u8(day)
            .u8(h)
            .u8(m)
            .u8(s)
            .u8(1)
            .bytes(&[0; 40])
            .finish();
        self.request(&pkt).map(drop)
    }

    /// Print a job. `pages` yields each page's packet sequence (from the filter).
    pub fn print_job<I>(&mut self, pages: I) -> io::Result<u32>
    where
        I: IntoIterator<Item = io::Result<Vec<u8>>>,
    {
        self.request(&packet::simple(op::PRINTER_INFO))?;
        if !self.opts.blind {
            sleep(Duration::from_secs(1));
        }
        self.xstatus()?;
        self.request(&packet::simple(op::ANNOUNCE_JOB))?;
        self.reserve()?;
        let result = self.print_reserved(pages);
        if let Err(e) = &result {
            self.log(format!("job failed ({e}); ending it on the printer"));
            if let Err(e) = self.cancel() {
                self.log(format!("ending the job failed: {e}"));
            }
        }
        result
    }

    fn print_reserved<I>(&mut self, pages: I) -> io::Result<u32>
    where
        I: IntoIterator<Item = io::Result<Vec<u8>>>,
    {
        self.request(&PacketBuilder::new(op::SET_LED).bytes(&LED_JOB).finish())?;
        self.wait_ready()?;
        self.job_info(STAGE_BEGIN, 0)?;
        self.wait_ready()?;

        let mut printed = 0u32;
        for page in pages {
            let page = page?;
            printed += 1;
            self.print_page(&page, printed)?;
        }

        let s = self.wait_for("job completed", |s| s.page_completed == s.page_decoding)?;
        self.job_info(STAGE_END, s.page_decoding)?;
        self.release()?;
        Ok(printed)
    }

    fn print_page(&mut self, page: &[u8], number: u32) -> io::Result<()> {
        self.log(format!("page {number}"));
        if self.xstatus()?.uninitialised() {
            self.request(&packet::simple(op::CLEAR_MISPRINT))?;
            self.request(&packet::simple(op::CLEAR_ERROR))?;
            self.request(&packet::simple(op::DISCARD_DATA))?;
            self.wait_ready()?;
            self.request(&PacketBuilder::new(op::GO_ONLINE).bytes(&WAKE).finish())?;
            self.wait_ready()?;
        }
        self.wait_for("printer buffer", |s| !s.buffer_full())?;

        let packets = packet::split(page).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut data_packets = 0;
        for p in packets {
            if packet::header(p).unwrap().0 == op::VIDEO_DATA {
                data_packets += 1;
                if data_packets % DATA_PACKETS_PER_CHECK == 0 {
                    self.wait_ready_basic()?;
                }
            }
            self.send(p)?;
        }
        self.blind_page = number as u16;

        let s = self.wait_for("page received", |s| s.page_received == s.page_decoding)?;
        let n = s.page_decoding;
        self.job_info(STAGE_RECEIVED, n)?;
        self.wait_ready()?;
        self.request(&PacketBuilder::new(op::START_PRINT).u16(n).finish())?;
        self.wait_ready()?;
        self.job_info(STAGE_PRINTING, n)?;

        let s = self.wait_for("page out", |s| s.page_out == s.page_decoding || s.no_paper())?;
        if s.no_paper() && s.page_out != s.page_decoding {
            self.log("printer is out of paper".into());
            if !s.printing() {
                return Err(io::Error::other("printer is out of paper"));
            }
        }
        Ok(())
    }

    /// End a job that failed part-way so the printer's page counters stay in step.
    fn cancel(&mut self) -> io::Result<()> {
        if self.job.is_none() {
            return Ok(());
        }
        self.request(&PacketBuilder::new(op::SET_LED).bytes(&LED_IDLE).finish())?;
        let s = self.xstatus()?;
        self.job_info(STAGE_END, s.page_completed)?;
        self.release()
    }
}

/// A length field read as binary-coded decimal, if it is valid BCD.
fn bcd(v: usize) -> Option<usize> {
    let mut out = 0;
    for shift in [12, 8, 4, 0] {
        let d = (v >> shift) & 0xF;
        if d > 9 {
            return None;
        }
        out = out * 10 + d;
    }
    Some(out)
}

/// Current UTC time as (year, month 1-12, day, hour, minute, second).
fn utc_now() -> (u16, u8, u8, u8, u8, u8) {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rem) = ((secs / 86400) as i64, secs % 86400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    (year as u16, month as u8, day as u8, (rem / 3600) as u8, (rem / 60 % 60) as u8, (rem % 60) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Fake printer: answers every request, counts pages as the data arrives and
    /// finishes a page when told to print it.
    #[derive(Default)]
    struct Fake {
        written: Vec<Vec<u8>>,
        pending: VecDeque<u8>,
        decoding: u16,
        printed: u16,
    }

    impl Fake {
        fn status_record(&self) -> Vec<u8> {
            let mut s = vec![0u8; 40];
            let put = |s: &mut Vec<u8>, i: usize, v: u16| s[i..i + 2].copy_from_slice(&v.to_le_bytes());
            put(&mut s, 14, self.decoding);
            put(&mut s, 16, self.printed);
            put(&mut s, 18, self.printed);
            put(&mut s, 20, self.printed);
            put(&mut s, 34, self.decoding);
            s
        }
    }

    impl Transport for Fake {
        fn write(&mut self, data: &[u8], _: Duration) -> io::Result<()> {
            self.written.push(data.to_vec());
            let (code, _) = packet::header(data).unwrap();
            match code {
                op::PAGE_SETUP | op::VIDEO_DATA => return Ok(()),
                op::VIDEO_END => {
                    self.decoding += 1;
                    return Ok(());
                }
                op::START_PRINT => self.printed = u16::from_le_bytes([data[4], data[5]]),
                _ => {}
            }
            let record = match code {
                op::RESERVE => vec![0, 0, 0x34, 0x12],
                op::EXTENDED_STATUS | op::BASIC_STATUS => self.status_record(),
                _ => vec![0, 0],
            };
            self.pending.extend(code.to_le_bytes());
            self.pending.extend(((4 + record.len()) as u16).to_le_bytes());
            self.pending.extend(record);
            Ok(())
        }
        fn read(&mut self, buf: &mut [u8], _: Duration) -> io::Result<usize> {
            let n = buf.len().min(self.pending.len()).min(64);
            for b in buf.iter_mut().take(n) {
                *b = self.pending.pop_front().unwrap();
            }
            Ok(n)
        }
    }

    fn test_page() -> Vec<u8> {
        let enc = crate::page::PageEncoder::new(crate::page::Paper::by_name("A5").unwrap(), Default::default());
        let lb = enc.paper.image_line_bytes();
        enc.encode_page(&vec![0; lb * 16], lb, 16)
    }

    fn ops(written: &[Vec<u8>]) -> Vec<u16> {
        written.iter().map(|p| packet::header(p).unwrap().0).collect()
    }

    fn stages(written: &[Vec<u8>]) -> Vec<(u8, u16)> {
        written
            .iter()
            .filter(|p| packet::header(p).unwrap().0 == op::SET_JOB_INFO)
            .map(|p| (p[20], u16::from_le_bytes([p[8], p[9]])))
            .collect()
    }

    #[test]
    fn job_sequence() {
        let opts = EngineOptions { poll_interval: Duration::from_millis(1), ..Default::default() };
        let mut e = Engine::new(Fake::default(), opts, Box::new(|_| {}));
        assert_eq!(e.print_job(vec![Ok(test_page()), Ok(test_page())]).unwrap(), 2);
        let ops = ops(&e.io.written);
        let pos = |c| ops.iter().position(|&o| o == c).unwrap();
        assert_eq!(&ops[..2], &[op::PRINTER_INFO, op::EXTENDED_STATUS]);
        assert!(pos(op::ANNOUNCE_JOB) < pos(op::RESERVE));
        assert!(pos(op::RESERVE) < pos(op::SET_LED));
        assert!(pos(op::PAGE_SETUP) < pos(op::VIDEO_DATA));
        assert!(pos(op::VIDEO_END) < pos(op::START_PRINT));
        assert!(!ops.contains(&op::SET_PRINTED_PAGES));
        assert_eq!(stages(&e.io.written), [(1, 0), (2, 1), (6, 1), (2, 2), (6, 2), (4, 2)]);
        let rel = e.io.written.last().unwrap();
        assert_eq!(&rel[..6], &[0xA9, 0xE0, 6, 0, 0x34, 0x12]);
        let info = e.io.written.iter().find(|p| p[..2] == [0xA1, 0xE1]).unwrap();
        assert_eq!(info.len(), 76);
        assert_eq!(&info[22..24], &[0x34, 0x12]);
    }

    /// Write-only transport: reading fails, as on a one-way network queue.
    struct Sink(Vec<Vec<u8>>);

    impl Transport for Sink {
        fn write(&mut self, data: &[u8], _: Duration) -> io::Result<()> {
            self.0.push(data.to_vec());
            Ok(())
        }
        fn read(&mut self, _: &mut [u8], _: Duration) -> io::Result<usize> {
            Err(io::ErrorKind::Unsupported.into())
        }
    }

    #[test]
    fn blind_job_never_reads() {
        let opts = EngineOptions { blind: true, blind_session: 0x0102, ..Default::default() };
        let mut e = Engine::new(Sink(vec![]), opts, Box::new(|_| {}));
        assert_eq!(e.print_job(vec![Ok(test_page()), Ok(test_page())]).unwrap(), 2);
        let ops = ops(&e.io.0);
        assert!(!ops.contains(&op::EXTENDED_STATUS) && !ops.contains(&op::BASIC_STATUS));
        assert_eq!(ops.iter().filter(|&&o| o == op::START_PRINT).count(), 2);
        assert_eq!(stages(&e.io.0), [(1, 0), (2, 1), (6, 1), (2, 2), (6, 2), (4, 2)]);
        let rel = e.io.0.last().unwrap();
        assert_eq!(&rel[4..6], &[0x02, 0x01]);
    }

    #[test]
    fn parses_status_record() {
        let mut raw = vec![0xA8, 0xA0, 0, 0, 0x80, 0x00];
        raw.extend([0u8; 34]);
        raw[4 + 14] = 3; // page_decoding
        raw[4 + 34] = 3; // page_received
        let len = raw.len() as u16;
        raw[2..4].copy_from_slice(&len.to_le_bytes());
        let s = Status::parse(&Response::parse(&raw).unwrap());
        assert!(s.busy());
        assert_eq!((s.page_decoding, s.page_received), (3, 3));
        assert_eq!(bcd(0x0060), Some(60));
        assert_eq!(bcd(0x003C), None);
    }
}
