//! macOS menu-bar UI.

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::menu::{IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::updater::{self, Release};

const QUEUE: &str = "Canon_LBP2900";
const BACKEND: &str = "/usr/libexec/cups/backend/lbp2900";
const SUPPORT: &str = "/Library/Printers/LBP2900";
const LOGS: &str = "/Users/Shared/LBP2900/logs";
const APP_PATH: &str = "/Applications/LBP2900 Driver.app";
const UPDATE_EVERY: Duration = Duration::from_secs(6 * 3600);

enum Msg {
    Status(String),
    UpdateFound(Release),
    UpToDate,
    Info(String),
    Installed,
    UpdateFailed(Release, String),
    InstallNow(String),
}

/// 22×22 template icon: a simple printer silhouette.
fn icon() -> Icon {
    let (w, h) = (22u32, 22u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let mut set = |x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        rgba[i..i + 4].copy_from_slice(&[0, 0, 0, 255]);
    };
    for y in 0..h {
        for x in 0..w {
            let paper_in = (6..=15).contains(&x) && (2..=7).contains(&y);
            let body = (2..=19).contains(&x) && (8..=15).contains(&y);
            let slot = (5..=16).contains(&x) && (12..=13).contains(&y);
            let paper_out = (6..=15).contains(&x) && (14..=19).contains(&y) && (x == 6 || x == 15 || y == 19);
            if (paper_in && (x == 6 || x == 15 || y == 2)) || (body && !slot) || paper_out {
                set(x, y);
            }
        }
    }
    Icon::from_rgba(rgba, w, h).expect("icon")
}

fn run_capture(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run a shell command as administrator (macOS password prompt).
fn run_admin(shell: &str) -> Result<String, String> {
    let script = format!(
        "do shell script \"{}\" with administrator privileges",
        shell.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let out = Command::new("/usr/bin/osascript").args(["-e", &script]).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Banner notification. macOS attributes osascript notifications to Script Editor,
/// so clicking one opens it; use [`alert`] for anything the user should act on.
fn notify(text: &str) {
    let script = format!(
        "display notification \"{}\" with title \"LBP2900 Driver\"",
        text.replace('"', "'")
    );
    let _ = Command::new("/usr/bin/osascript").args(["-e", &script]).spawn();
}

/// Small alert window with an OK button (does not block the menu).
fn alert(text: &str) {
    let script = format!(
        "display alert \"LBP2900 Driver\" message \"{}\"",
        text.replace('"', "'")
    );
    let _ = Command::new("/usr/bin/osascript").args(["-e", &script]).spawn();
}

/// Ask whether to install `version` now; false for "Later" or no answer.
fn ask_install(version: &str) -> bool {
    let script = format!(
        "display dialog \"Canon LBP2900 driver {version} is available (you have {}). Install it now?\" \
         with title \"LBP2900 Driver\" buttons {{\"Later\", \"Install\"}} default button \"Install\" \
         with icon note giving up after 600",
        updater::CURRENT
    );
    Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("button returned:Install"))
        .unwrap_or(false)
}

fn printer_status() -> String {
    let usb = run_capture("/usr/sbin/ioreg", &["-p", "IOUSB", "-w0"]).unwrap_or_default();
    let connected = usb.contains("LBP2900") || usb.contains("LBP3000");
    let queue = run_capture("/usr/bin/lpstat", &["-p", QUEUE]).unwrap_or_default();
    let q = if queue.contains("idle") {
        "queue idle"
    } else if queue.contains("printing") {
        "printing"
    } else if queue.contains("disabled") {
        "queue paused"
    } else if queue.trim().is_empty() {
        "no queue"
    } else {
        "queue ready"
    };
    format!("Printer: {} · {q}", if connected { "connected" } else { "not connected" })
}

fn spawn<F: FnOnce(&Sender<Msg>) + Send + 'static>(tx: &Sender<Msg>, f: F) {
    let tx = tx.clone();
    thread::spawn(move || f(&tx));
}

fn check_updates(tx: &Sender<Msg>, manual: bool) {
    spawn(tx, move |tx| match updater::latest() {
        Ok(r) if updater::is_newer(&r.version, updater::CURRENT) => {
            let _ = tx.send(Msg::UpdateFound(r));
        }
        Ok(_) => {
            if manual {
                let _ = tx.send(Msg::UpToDate);
            }
        }
        Err(e) => {
            if manual {
                let _ = tx.send(Msg::Info(format!("Update check failed: {e}")));
            }
        }
    });
}

fn install_update(tx: &Sender<Msg>, rel: Release) {
    spawn(tx, move |tx| {
        notify(&format!("Downloading {}…", rel.version));
        // Not temp_dir(): an app started by an installer inherits its sandbox TMPDIR,
        // which is deleted once that install finishes.
        let dir = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/Users/Shared".into()))
            .join("Library/Caches/io.github.mk-manishkandel.lbp2900");
        let dest = dir.join(format!("LBP2900-Driver-{}.pkg", rel.version));
        let result = std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))
            .and_then(|()| updater::download(&rel.pkg_url, &dest))
            .and_then(|()| {
                let cmd = format!("/usr/sbin/installer -pkg '{}' -target /", dest.display());
                run_admin(&cmd).map_err(|e| format!("Install failed: {e}"))
            });
        let _ = std::fs::remove_file(&dest);
        let _ = tx.send(match result {
            Ok(_) => Msg::Installed,
            Err(e) => Msg::UpdateFailed(rel, e),
        });
    });
}

struct Ui {
    _tray: TrayIcon,
    status: MenuItem,
    update: MenuItem,
    check: MenuItem,
    add_queue: MenuItem,
    test_page: MenuItem,
    logs: MenuItem,
    project: MenuItem,
    quit: MenuItem,
}

fn build_ui() -> Ui {
    let menu = Menu::new();
    let title = MenuItem::new(format!("LBP2900 Driver {}", updater::CURRENT), false, None);
    let status = MenuItem::new("Printer: checking…", false, None);
    let update = MenuItem::new("No update available", false, None);
    let check = MenuItem::new("Check for Updates", true, None);
    let add_queue = MenuItem::new("Set Up Printer Queue…", true, None);
    let test_page = MenuItem::new("Print Test Page (direct)", true, None);
    let logs = MenuItem::new("Open Logs Folder", true, None);
    let project = MenuItem::new("Open Project Page", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let (s1, s2, s3) = (PredefinedMenuItem::separator(), PredefinedMenuItem::separator(), PredefinedMenuItem::separator());
    let items: [&dyn IsMenuItem; 12] =
        [&title, &status, &s1, &update, &check, &s2, &add_queue, &test_page, &logs, &project, &s3, &quit];
    menu.append_items(&items).expect("menu");
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Canon LBP2900 driver")
        .with_icon(icon())
        .with_icon_as_template(true)
        .build()
        .expect("tray icon");
    Ui { _tray: tray, status, update, check, add_queue, test_page, logs, project, quit }
}

pub fn run() {
    let mut event_loop = EventLoopBuilder::new().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    let (tx, rx): (Sender<Msg>, Receiver<Msg>) = mpsc::channel();
    let menu_rx = MenuEvent::receiver();
    let mut ui: Option<Ui> = None;
    let mut pending: Option<Release> = None;
    let mut asked: Option<String> = None;
    let mut next_status = Instant::now();
    let mut next_update = Instant::now() + Duration::from_secs(10);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(300));
        if let Event::NewEvents(StartCause::Init) = event {
            ui = Some(build_ui());
        }
        let Some(ui) = ui.as_ref() else { return };

        let now = Instant::now();
        if now >= next_status {
            next_status = now + Duration::from_secs(5);
            spawn(&tx, |tx| {
                let _ = tx.send(Msg::Status(printer_status()));
            });
        }
        if now >= next_update {
            next_update = now + UPDATE_EVERY;
            check_updates(&tx, false);
        }

        while let Ok(msg) = rx.try_recv() {
            match msg {
                Msg::Status(s) => ui.status.set_text(s),
                Msg::UpdateFound(r) => {
                    ui.update.set_text(format!("Install Update {}…", r.version));
                    ui.update.set_enabled(true);
                    // Ask once per version; the menu item stays available either way.
                    if asked.as_deref() != Some(r.version.as_str()) {
                        asked = Some(r.version.clone());
                        let version = r.version.clone();
                        spawn(&tx, move |tx| {
                            if ask_install(&version) {
                                let _ = tx.send(Msg::InstallNow(version));
                            }
                        });
                    }
                    pending = Some(r);
                }
                Msg::InstallNow(version) => {
                    if let Some(r) = pending.take_if(|r| r.version == version) {
                        ui.update.set_text(format!("Installing {}…", r.version));
                        ui.update.set_enabled(false);
                        install_update(&tx, r);
                    }
                }
                Msg::UpToDate => alert(&format!("You have the latest version ({}).", updater::CURRENT)),
                Msg::Info(s) => alert(&s),
                Msg::UpdateFailed(r, e) => {
                    ui.update.set_text(format!("Install Update {}… (last try failed)", r.version));
                    ui.update.set_enabled(true);
                    alert(&format!("Update {} failed: {e}", r.version));
                    pending = Some(r);
                }
                Msg::Installed => {
                    notify("Update installed — restarting");
                    let _ = Command::new("/usr/bin/open").args(["-n", APP_PATH]).spawn();
                    *control_flow = ControlFlow::Exit;
                }
            }
        }

        while let Ok(ev) = menu_rx.try_recv() {
            let id = ev.id();
            if id == ui.quit.id() {
                *control_flow = ControlFlow::Exit;
            } else if id == ui.check.id() {
                check_updates(&tx, true);
            } else if id == ui.update.id() {
                if let Some(r) = pending.take() {
                    ui.update.set_text(format!("Installing {}…", r.version));
                    ui.update.set_enabled(false);
                    install_update(&tx, r);
                }
            } else if id == ui.add_queue.id() {
                spawn(&tx, |tx| {
                    let msg = match run_admin(&format!("'{SUPPORT}/setup-queue.sh'")) {
                        Ok(out) => out.lines().last().unwrap_or("Printer queue set up").to_string(),
                        Err(e) => format!("Queue setup failed: {e}"),
                    };
                    let _ = tx.send(Msg::Info(msg));
                });
            } else if id == ui.test_page.id() {
                spawn(&tx, |tx| {
                    let msg = match run_admin(&format!("'{BACKEND}' diag testpage")) {
                        Ok(_) => "Test page sent".to_string(),
                        Err(e) => format!("Test page failed: {e} (see logs)"),
                    };
                    let _ = tx.send(Msg::Info(msg));
                });
            } else if id == ui.logs.id() {
                let _ = std::fs::create_dir_all(LOGS);
                let _ = Command::new("/usr/bin/open").arg(LOGS).spawn();
            } else if id == ui.project.id() {
                let _ = Command::new("/usr/bin/open").arg(format!("https://github.com/{}", updater::REPO)).spawn();
            }
        }
    });
}
