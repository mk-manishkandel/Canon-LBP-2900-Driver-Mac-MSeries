//! CUPS backend `lbp2900` for the Canon LBP2900.
//!
//! * no arguments          → device discovery (CUPS)
//! * job user title copies options [file] → print a job stream from `rastertocapt`
//! * `diag …`              → diagnostics for testing by hand (see `diag help`)

mod config;
mod testpage;
mod usb;

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use capt::packet::{self, hex, op};
use capt::page::{PageEncoder, PageSettings, Paper};
use capt::spool::SpoolReader;
use capt::{Engine, EngineOptions};

// CUPS backend exit codes.
const CUPS_BACKEND_OK: u8 = 0;
const CUPS_BACKEND_FAILED: u8 = 1;
const CUPS_BACKEND_RETRY: u8 = 6;

const LOG_DIRS: &[&str] = &["/Users/Shared/LBP2900/logs", "/private/var/tmp/lbp2900"];

/// Job log: every line goes to stderr (CUPS error_log) and to a log file if possible.
#[derive(Clone)]
struct Log(Arc<Mutex<Option<File>>>);

impl Log {
    fn open(name: &str) -> (Log, Option<PathBuf>) {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        for dir in LOG_DIRS {
            if fs::create_dir_all(dir).is_err() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o777));
            }
            let path = PathBuf::from(dir).join(format!("{name}-{stamp}.log"));
            if let Ok(f) = OpenOptions::new().create(true).append(true).open(&path) {
                return (Log(Arc::new(Mutex::new(Some(f)))), Some(path));
            }
        }
        (Log(Arc::new(Mutex::new(None))), None)
    }

    fn line(&self, msg: &str) {
        eprintln!("DEBUG: {msg}");
        if let Some(f) = self.0.lock().unwrap().as_mut() {
            let _ = writeln!(f, "{msg}");
        }
    }
}

fn engine_options(log: &Log) -> EngineOptions {
    let mut opts = EngineOptions::default();
    for w in config::load(&mut opts) {
        log.line(&format!("config: {w}"));
    }
    log.line(&format!("options: {opts:?}"));
    opts
}

fn discover() -> ExitCode {
    for p in usb::find_printers() {
        let id = if p.device_id.is_empty() {
            format!("MFG:Canon;MDL:{};CMD:CAPT;", p.model)
        } else {
            p.device_id.clone()
        };
        println!(
            "direct {} \"Canon {}\" \"Canon {} (LBP2900 Rust driver)\" \"{}\" \"\"",
            p.uri(),
            p.model,
            p.model,
            id.replace('"', "'")
        );
    }
    ExitCode::from(CUPS_BACKEND_OK)
}

/// Find the printer named by a `lbp2900://Canon/MODEL?serial=…` URI.
fn find_for_uri(uri: Option<&str>) -> Option<usb::Found> {
    let serial = uri.and_then(|u| u.split_once("serial=")).map(|(_, s)| s.split('&').next().unwrap_or(s).to_string());
    let mut printers = usb::find_printers();
    if let Some(s) = serial {
        if let Some(i) = printers.iter().position(|p| p.serial == s) {
            return Some(printers.swap_remove(i));
        }
        // Serial strings can differ between discovery runs; accept a lone printer.
        if printers.len() != 1 {
            return None;
        }
    }
    printers.into_iter().next()
}

fn wait_for_printer(uri: Option<&str>, log: &Log) -> Option<usb::Found> {
    for attempt in 0..60 {
        if let Some(p) = find_for_uri(uri) {
            return Some(p);
        }
        if attempt == 0 {
            eprintln!("STATE: +offline-report");
            eprintln!("INFO: Waiting for the Canon LBP2900 to be connected and switched on…");
            log.line("printer not found, waiting");
        }
        sleep(Duration::from_secs(5));
    }
    None
}

fn run_job(args: &[String]) -> ExitCode {
    let job = &args[1];
    let (log, path) = Log::open(&format!("job-{job}"));
    if let Some(p) = &path {
        log.line(&format!("log file: {}", p.display()));
    }
    log.line(&format!("lbp2900 backend {} args {:?}", env!("CARGO_PKG_VERSION"), &args[1..]));
    let uri = std::env::var("DEVICE_URI").ok();

    let input: Box<dyn Read> = match args.get(6) {
        Some(f) => match File::open(f) {
            Ok(f) => Box::new(BufReader::new(f)),
            Err(e) => {
                eprintln!("ERROR: cannot open {f}: {e}");
                return ExitCode::from(CUPS_BACKEND_FAILED);
            }
        },
        None => Box::new(BufReader::new(io::stdin())),
    };
    let mut spool = match SpoolReader::new(input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ERROR: {e} — is the printer queue using the LBP2900 Rust PPD?");
            return ExitCode::from(CUPS_BACKEND_FAILED);
        }
    };

    let Some(found) = wait_for_printer(uri.as_deref(), &log) else {
        eprintln!("ERROR: Canon LBP2900 not found on USB");
        return ExitCode::from(CUPS_BACKEND_RETRY);
    };
    eprintln!("STATE: -offline-report");
    log.line(&format!("using {} pid={:04X} serial={} id={}", found.model, found.pid, found.serial, found.device_id));
    let transport = match usb::UsbTransport::open(&found) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("ERROR: cannot open printer: {e}");
            return ExitCode::from(CUPS_BACKEND_RETRY);
        }
    };

    let mut opts = engine_options(&log);
    opts.user = args[2].clone();
    opts.title = args[3].clone();
    opts.host = std::env::var("CUPS_SERVER").ok().filter(|h| !h.starts_with('/')).unwrap_or_else(|| "mac".into());
    let l = log.clone();
    let mut engine = Engine::new(transport, opts, Box::new(move |m| l.line(m)));
    let pages = std::iter::from_fn(|| match spool.next_page() {
        Ok(Some(p)) => {
            eprintln!("INFO: Printing…");
            Some(Ok(p))
        }
        Ok(None) => None,
        Err(e) => Some(Err(e)),
    });
    match engine.print_job(pages) {
        Ok(n) => {
            log.line(&format!("job done, {n} page(s)"));
            eprintln!("INFO: Printed {n} page(s)");
            ExitCode::from(CUPS_BACKEND_OK)
        }
        Err(e) => {
            log.line(&format!("job failed: {e}"));
            eprintln!("ERROR: printing failed: {e}");
            ExitCode::from(CUPS_BACKEND_FAILED)
        }
    }
}

fn parse_hex(s: &str) -> Option<Vec<u8>> {
    let s: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    (s.len() % 2 == 0).then(|| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect())
}

fn diag(args: &[String]) -> ExitCode {
    let cmd = args.get(2).map(String::as_str).unwrap_or("help");
    let (log, path) = Log::open(&format!("diag-{cmd}"));
    let open = || -> Option<Engine<usb::UsbTransport>> {
        let found = find_for_uri(None)?;
        println!("printer: {} pid={:04X} serial={} id={}", found.model, found.pid, found.serial, found.device_id);
        match usb::UsbTransport::open(&found) {
            Ok(t) => {
                let l = log.clone();
                Some(Engine::new(t, engine_options(&log), Box::new(move |m| {
                    println!("{m}");
                    l.line(m)
                })))
            }
            Err(e) => {
                println!("cannot open printer: {e}");
                None
            }
        }
    };
    let result: io::Result<()> = match cmd {
        "list" => {
            let printers = usb::find_printers();
            if printers.is_empty() {
                println!("no Canon CAPT printer found on USB");
            }
            for p in printers {
                println!("{}  pid={:04X}  id={}", p.uri(), p.pid, p.device_id);
            }
            Ok(())
        }
        "status" => match open() {
            Some(mut e) => (|| {
                e.device_id()?;
                for code in [op::EXTENDED_STATUS, op::PRINTER_INFO, op::ENGINE_VERSION, op::CARTRIDGE_STATUS] {
                    if let Err(err) = e.request(&packet::simple(code)) {
                        println!("{} failed: {err}", op::name(code));
                    }
                }
                Ok(())
            })(),
            None => Err(io::ErrorKind::NotFound.into()),
        },
        "raw" => match (open(), args.get(3).and_then(|h| parse_hex(h))) {
            (Some(mut e), Some(pkt)) => e.request(&pkt).map(|r| println!("response body: {}", hex(&r.body))),
            (_, None) => {
                println!("usage: lbp2900 diag raw \"A0 A8 04 00\"");
                Ok(())
            }
            _ => Err(io::ErrorKind::NotFound.into()),
        },
        "testpage" => {
            let paper = Paper::by_name(args.get(3).map(String::as_str).unwrap_or("A4")).unwrap_or(&capt::page::PAPERS[0]);
            let (bitmap, lb, lines) = testpage::render(paper);
            let page = PageEncoder::new(paper, PageSettings::default()).encode_page(&bitmap, lb, lines);
            println!("test page: {} {} bytes/line × {} lines → {} bytes of CAPT data", paper.name, lb, lines, page.len());
            match open() {
                Some(mut e) => e.print_job([Ok(page)]).map(|n| println!("printed {n} page")),
                None => Err(io::ErrorKind::NotFound.into()),
            }
        }
        "dump" => match args.get(3).map(File::open) {
            Some(Ok(f)) => (|| {
                let mut s = SpoolReader::new(BufReader::new(f))?;
                let mut n = 0;
                while let Some(page) = s.next_page()? {
                    n += 1;
                    println!("page {n}: {} bytes", page.len());
                    let pkts = packet::split(&page).map_err(io::Error::other)?;
                    for p in pkts.iter().filter(|p| packet::header(p).unwrap().0 != op::VIDEO_DATA) {
                        println!("  {} {}", op::name(packet::header(p).unwrap().0), hex(p));
                    }
                    let video = pkts.iter().filter(|p| packet::header(p).unwrap().0 == op::VIDEO_DATA).count();
                    println!("  + {video} VideoData packet(s)");
                }
                Ok(())
            })(),
            _ => {
                println!("usage: lbp2900 diag dump <job-stream-file>");
                Ok(())
            }
        },
        _ => {
            println!(
                "lbp2900 {} diagnostics\n\
                 \n  diag list               list connected Canon CAPT printers\
                 \n  diag status             query device id and status\
                 \n  diag testpage [A4|Letter]  print the built-in test page directly\
                 \n  diag raw \"A0 A8 04 00\"  send one CAPT request and show the reply\
                 \n  diag dump <file>        decode a job stream written by rastertocapt\
                 \n\nLogs: {}",
                env!("CARGO_PKG_VERSION"),
                LOG_DIRS[0]
            );
            Ok(())
        }
    };
    if let Some(p) = path {
        println!("log: {}", p.display());
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            println!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.len() {
        1 => discover(),
        _ if args[1] == "diag" || args[1] == "--help" => diag(&args),
        6 | 7 => run_job(&args),
        _ => {
            eprintln!("Usage: lbp2900 job-id user title copies options [file]\n       lbp2900 diag help");
            ExitCode::from(CUPS_BACKEND_FAILED)
        }
    }
}
