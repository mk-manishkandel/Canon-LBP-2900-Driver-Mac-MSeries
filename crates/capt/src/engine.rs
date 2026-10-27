//! Printer conversation for a job, modelled on Canon's captmoncnab3
//! (`PrintJob1` → `PrintJob2` → `PrintJob` → `PrintContinuous`).
//!
//! Parts of the sequence whose exact semantics are not yet confirmed on hardware are
//! switchable through [`EngineOptions`] so they can be tuned from the config file
//! without a new build.

use std::io;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::packet::{self, hex, op, PacketBuilder, Response};

/// Byte transport to the printer (USB bulk endpoints in the backend).
pub trait Transport {
    fn write(&mut self, data: &[u8], timeout: Duration) -> io::Result<()>;
    /// One read of up to `buf.len()` bytes.
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
}

#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// Argument of the A2A0 reserve request.
    pub reserve_arg: u16,
    /// Send E1A1 job information (user/host/title) after reserving.
    pub send_job_info: bool,
    /// Send E0A2 ClearError / E0A4 DiscardData before the first page.
    pub clear_before_job: bool,
    /// Send E0A7 StartPrint before each page's image data.
    pub start_print: bool,
    /// Send E0A5 GoOnline after each page's data.
    pub go_online: bool,
    pub go_online_arg: u16,
    /// Maximum time to wait for a page to finish printing.
    pub page_timeout: Duration,
    pub poll_interval: Duration,
    pub read_timeout: Duration,
    pub write_timeout: Duration,
    pub user: String,
    pub host: String,
    pub title: String,
}

impl Default for EngineOptions {
    fn default() -> Self {
        EngineOptions {
            reserve_arg: 0,
            send_job_info: true,
            clear_before_job: true,
            start_print: true,
            go_online: true,
            go_online_arg: 0,
            page_timeout: Duration::from_secs(120),
            poll_interval: Duration::from_millis(500),
            read_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(60),
            user: "user".into(),
            host: "mac".into(),
            title: "document".into(),
        }
    }
}

pub struct Engine<T: Transport> {
    io: T,
    pub opts: EngineOptions,
    log: Box<dyn FnMut(&str)>,
    session: Option<u16>,
}

impl<T: Transport> Engine<T> {
    pub fn new(io: T, opts: EngineOptions, log: Box<dyn FnMut(&str)>) -> Self {
        Engine { io, opts, log, session: None }
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
        let mut raw = Vec::new();
        let mut buf = [0u8; 512];
        let deadline = Instant::now() + self.opts.read_timeout;
        loop {
            let n = self.io.read(&mut buf, self.opts.read_timeout)?;
            raw.extend_from_slice(&buf[..n]);
            if let Some((_, len)) = packet::header(&raw) {
                if raw.len() >= 6 && raw.len() >= len.max(6) {
                    break;
                }
            }
            if Instant::now() > deadline {
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

    /// Status: A0A8 (extended) before reserving, E0A0 (basic) while reserved.
    pub fn status(&mut self) -> io::Result<Response> {
        let code = if self.session.is_some() { op::BASIC_STATUS } else { op::EXTENDED_STATUS };
        self.request(&packet::simple(code))
    }

    pub fn reserve(&mut self) -> io::Result<u16> {
        let pkt = PacketBuilder::new(op::RESERVE)
            .u16(self.opts.reserve_arg)
            .u16(0x1E)
            .pad_to(12)
            .finish();
        let r = self.request(&pkt)?;
        let session = r.u16_at(0).unwrap_or(0);
        self.log(format!("reserved printer, session {session:#06X}"));
        self.session = Some(session);
        Ok(session)
    }

    pub fn release(&mut self) -> io::Result<()> {
        if let Some(s) = self.session.take() {
            let pkt = PacketBuilder::new(op::RELEASE).u16(s).finish();
            self.request(&pkt)?;
        }
        Ok(())
    }

    fn job_info_packet(&self, session: u16) -> Vec<u8> {
        let utf16 = |s: &str| s.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>();
        let (host, user, title) = (utf16(&self.opts.host), utf16(&self.opts.user), utf16(&self.opts.title));
        let mut b = PacketBuilder::new(op::SET_JOB_INFO)
            .u16(0)
            .u16(0)
            .u32(0)
            .u16(host.len() as u16)
            .u16(user.len() as u16)
            .u16(title.len() as u16)
            .u16(0)
            .u8(0)
            .u8(0)
            .u16(session)
            .u16(0)
            .u16(0)
            .u16(0)
            .bytes(&[0; 6]);
        for _ in 0..10 {
            b = b.u32(0);
        }
        b.bytes(&host).bytes(&user).bytes(&title).finish()
    }

    /// Poll status until it returns to `idle` after having changed, or time out.
    fn wait_page_done(&mut self, idle: &[u8]) -> io::Result<()> {
        let start = Instant::now();
        let mut changed = false;
        let mut last = idle.to_vec();
        loop {
            sleep(self.opts.poll_interval);
            let s = self.status()?;
            if s.body != last {
                self.log(format!("status changed after {:?}", start.elapsed()));
                last = s.body.clone();
            }
            if s.body != idle {
                changed = true;
            } else if changed {
                return Ok(());
            }
            if start.elapsed() > self.opts.page_timeout {
                self.log("page timeout reached; continuing".into());
                return Ok(());
            }
        }
    }

    /// Print a job. `pages` yields each page's packet sequence (from the filter).
    pub fn print_job<I>(&mut self, pages: I) -> io::Result<u32>
    where
        I: IntoIterator<Item = io::Result<Vec<u8>>>,
    {
        let id = self.device_id()?;
        self.log(format!("device id: {id}"));
        let st = self.status()?;
        self.log(format!("extended status: {}", hex(&st.body)));

        let session = self.reserve()?;
        let result = self.print_reserved(session, pages);
        if let Err(e) = self.release() {
            self.log(format!("release failed: {e}"));
        }
        result
    }

    fn print_reserved<I>(&mut self, session: u16, pages: I) -> io::Result<u32>
    where
        I: IntoIterator<Item = io::Result<Vec<u8>>>,
    {
        if self.opts.send_job_info {
            let pkt = self.job_info_packet(session);
            if let Err(e) = self.request(&pkt) {
                self.log(format!("SetJobInfo failed (continuing): {e}"));
            }
        }
        if self.opts.clear_before_job {
            self.request(&packet::simple(op::CLEAR_ERROR))?;
            self.request(&packet::simple(op::DISCARD_DATA))?;
        }
        self.request(&PacketBuilder::new(op::SET_PRINTED_PAGES).u32(0).finish())?;

        let mut printed = 0u32;
        for page in pages {
            let page = page?;
            printed += 1;
            let idle = self.status()?.body;
            self.log(format!("page {printed}: idle status {}", hex(&idle)));
            let packets = packet::split(&page).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let mut started = false;
            for p in packets {
                let code = packet::header(p).unwrap().0;
                if code == op::VIDEO_DATA && !started {
                    started = true;
                    if self.opts.start_print {
                        let sp = PacketBuilder::new(op::START_PRINT).u16(printed as u16).finish();
                        self.request(&sp)?;
                    }
                }
                self.send(p)?;
            }
            if self.opts.go_online {
                let pkt = PacketBuilder::new(op::GO_ONLINE)
                    .u32(0xADEA_DBEE)
                    .u16(self.opts.go_online_arg)
                    .u8(0)
                    .u8(1)
                    .pad_to(20)
                    .finish();
                self.request(&pkt)?;
            }
            self.wait_page_done(&idle)?;
            self.request(&PacketBuilder::new(op::SET_PRINTED_PAGES).u32(printed).finish())?;
        }
        Ok(printed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Fake printer: echoes a header-only response for every request, and a changing
    /// status so page completion is detected.
    struct Fake {
        written: Vec<Vec<u8>>,
        pending: VecDeque<u8>,
        polls: u32,
    }

    impl Transport for Fake {
        fn write(&mut self, data: &[u8], _: Duration) -> io::Result<()> {
            self.written.push(data.to_vec());
            let (code, _) = packet::header(data).unwrap();
            let is_data = matches!(code, 0xC0A0 | 0xC0A4 | 0xD0A0 | 0xD0A1 | 0xD0A2 | 0xD0A4);
            if !is_data {
                let mut r = code.to_le_bytes().to_vec();
                let body: Vec<u8> = match code {
                    op::RESERVE => vec![0x34, 0x12],
                    op::BASIC_STATUS => {
                        self.polls += 1;
                        vec![if self.polls % 3 == 2 { 1 } else { 0 }]
                    }
                    _ => vec![],
                };
                r.extend(((6 + body.len()) as u16).to_le_bytes());
                r.extend([0, 0]);
                r.extend(body);
                self.pending.extend(r);
            }
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

    #[test]
    fn job_sequence() {
        let fake = Fake { written: vec![], pending: VecDeque::new(), polls: 0 };
        let mut opts = EngineOptions::default();
        opts.poll_interval = Duration::from_millis(1);
        let mut e = Engine::new(fake, opts, Box::new(|_| {}));
        let enc = crate::page::PageEncoder::new(crate::page::Paper::by_name("A5").unwrap(), Default::default());
        let lb = enc.paper.image_line_bytes();
        let page = enc.encode_page(&vec![0; lb * 16], lb, 16);
        assert_eq!(e.print_job(vec![Ok(page)]).unwrap(), 1);
        let ops: Vec<u16> = e.io.written.iter().map(|p| packet::header(p).unwrap().0).collect();
        let pos = |c| ops.iter().position(|&o| o == c).unwrap();
        assert!(pos(op::RESERVE) < pos(op::SET_PAGE_PARAMS));
        assert!(pos(op::BEGIN_DATA) < pos(op::START_PRINT));
        assert!(pos(op::START_PRINT) < pos(op::VIDEO_DATA));
        assert_eq!(*ops.last().unwrap(), op::RELEASE);
        let rel = e.io.written.last().unwrap();
        assert_eq!(&rel[4..6], &[0x34, 0x12]);
    }
}
