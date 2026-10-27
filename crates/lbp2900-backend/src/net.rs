//! Printing through a print server that shares the printer over LPD (e.g. a Windows PC
//! with the LBP2900 on its USB port).
//!
//! The server only forwards bytes to the printer and never returns its answers, so the
//! whole CAPT conversation is written up front in blind mode (see
//! [`EngineOptions::blind`]) and handed to the system's CUPS `lpd` backend as a raw
//! (`l`) job.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::Duration;

use capt::spool::SpoolReader;
use capt::{Engine, EngineOptions, Transport};

use crate::{Log, CUPS_BACKEND_FAILED, CUPS_BACKEND_OK};

const LPD_BACKEND: &str = "/usr/libexec/cups/backend/lpd";

/// Write-only transport collecting the conversation in a file.
struct FileTransport(File);

impl Transport for FileTransport {
    fn write(&mut self, data: &[u8], _: Duration) -> io::Result<()> {
        self.0.write_all(data)
    }
    fn read(&mut self, _: &mut [u8], _: Duration) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "one-way network queue"))
    }
}

/// Encode `pages` as one blind CAPT conversation and send it to `target`
/// (`lpd://HOST/QUEUE`). `job` is the CUPS `job user title copies options` arguments.
pub fn send_pages<I>(target: &str, job: &[String], mut opts: EngineOptions, pages: I, log: &Log, echo: bool) -> io::Result<u32>
where
    I: IntoIterator<Item = io::Result<Vec<u8>>>,
{
    opts.blind = true;
    let dir = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let path = dir.join(format!("lbp2900-{}.capt", std::process::id()));
    let result = (|| {
        let file = File::create(&path)?;
        let l = log.clone();
        let mut engine = Engine::new(FileTransport(file), opts, Box::new(move |m| {
            if echo {
                println!("{m}");
            }
            l.line(m)
        }));
        engine.print_job(pages)
    })();
    let result = result.and_then(|pages| {
        let size = fs::metadata(&path)?.len();
        log.line(&format!("blind CAPT stream: {pages} page(s), {size} bytes → {target}"));
        submit(target, job, &path, log).map(|()| pages)
    });
    let _ = fs::remove_file(&path);
    result
}

/// Run the system `lpd` backend on the finished stream as a raw job.
fn submit(target: &str, job: &[String], path: &std::path::Path, log: &Log) -> io::Result<()> {
    let uri = if target.contains('?') { target.to_string() } else { format!("{target}?format=l") };
    log.line(&format!("sending through {LPD_BACKEND} {uri}"));
    let status = Command::new(LPD_BACKEND)
        .args(&job[..5])
        .arg(path)
        .env("DEVICE_URI", &uri)
        .status()?;
    log.line(&format!("lpd backend finished: {status}"));
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("lpd backend failed ({status})")))
    }
}

pub fn print_via_lpd<R: Read>(target: &str, job: &[String], opts: EngineOptions, spool: &mut SpoolReader<R>, log: &Log) -> ExitCode {
    eprintln!("INFO: Preparing pages for {target}…");
    let pages = std::iter::from_fn(|| spool.next_page().transpose());
    match send_pages(target, job, opts, pages, log, false) {
        Ok(n) => {
            log.line(&format!("job done, {n} page(s) sent"));
            eprintln!("INFO: Sent {n} page(s) to {target}");
            ExitCode::from(CUPS_BACKEND_OK)
        }
        Err(e) => {
            log.line(&format!("job failed: {e}"));
            eprintln!("ERROR: sending to {target} failed: {e}");
            ExitCode::from(CUPS_BACKEND_FAILED)
        }
    }
}
