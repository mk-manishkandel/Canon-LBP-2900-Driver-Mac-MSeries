# Canon LBP2900 driver for Apple Silicon Macs (Rust)

An open-source CUPS driver for the Canon LBP2900 / LBP2900B, written in Rust,
for current macOS on Apple Silicon (and Intel). Canon's last Mac driver
(CAPT v10.0.9) has no LBP2900 model; this driver speaks the printer's CAPT
protocol directly over USB.

> **Status: experimental.** The compression, page format and USB transport are
> implemented from a study of Canon's driver, but the printer conversation has not
> yet been confirmed on real hardware. Expect a few test releases before it prints.

No Canon code or files are included. The protocol was learned by studying Canon's
public macOS driver package; see [docs/re-notes.md](docs/re-notes.md).

## What gets installed

| Path | What |
|---|---|
| `/usr/libexec/cups/backend/lbp2900` | USB backend (talks CAPT to the printer) |
| `/Library/Printers/LBP2900/filter/rastertocapt` | CUPS filter (raster → compressed CAPT pages) |
| `/Library/Printers/PPDs/Contents/Resources/Canon-LBP2900-Rust.ppd.gz` | Printer description |
| `/Applications/LBP2900 Driver.app` | Menu-bar app: status, updates, test page, logs |
| `/Library/Printers/LBP2900/uninstall.sh` | Uninstaller |

## Install

1. Download `LBP2900-Driver-<version>.pkg` from the
   [latest release](../../releases/latest).
2. The package is not signed with an Apple Developer ID, so macOS blocks a
   double-click. Either
   - run `sudo installer -pkg ~/Downloads/LBP2900-Driver-*.pkg -target /` in Terminal, or
   - double-click it, then go to **System Settings → Privacy & Security** and
     click **Open Anyway**.
3. Connect and switch on the printer. The installer creates the queue
   **Canon_LBP2900**; if the printer was off, use **Set Up Printer Queue…** in the
   menu-bar app afterwards.

## Updating

The menu-bar app (printer icon) checks this repository's releases. Pick
**Install Update <version>…**, enter your password, and it installs the new package.

## Testing and sending feedback

Run these in Terminal with the printer connected and switched on:

```sh
# Is the printer found? What does it report?
sudo /usr/libexec/cups/backend/lbp2900 diag list
sudo /usr/libexec/cups/backend/lbp2900 diag status

# Print a built-in test page directly, bypassing CUPS
sudo /usr/libexec/cups/backend/lbp2900 diag testpage
```

Then print something normal (e.g. a PDF from Preview to *Canon_LBP2900*).

Logs are written to `/Users/Shared/LBP2900/logs/` (menu-bar app → **Open Logs Folder**).
Every exchange with the printer is logged in hex. Please attach these logs to a
GitHub issue together with what the printer did (lights, noises, paper fed or not).

### Tuning switches

`/Library/Printers/LBP2900/lbp2900.conf.example` lists switches for the steps
of the printer conversation that are not yet confirmed. Copy it to
`lbp2900.conf` in the same folder (with `sudo`) and change only what an issue
asks you to.

### Quick comparison with Canon's driver

Canon's v10.0.9 driver includes the LBP3000, a close sibling of the LBP2900. If
you want, install it and add the printer with the *Canon LBP3000* driver. If that
prints, please mention it in an issue. It helps confirm the protocol. Remove it
again before using this driver (both claim the USB device).

## Uninstall

```sh
sudo /Library/Printers/LBP2900/uninstall.sh
```

## Building

The Rust code builds and tests on any platform (`cargo test`). The installer is
built on macOS by `packaging/build-pkg.sh`. GitHub Actions runs it on every push
and publishes a release for every `v*` tag:

```sh
# bump version in Cargo.toml, then
git tag v0.1.1 && git push origin main v0.1.1
```

## Layout

- `crates/capt`: CAPT protocol: packets, compression, page encoding, raster reader, engine
- `crates/rastertocapt`: CUPS filter
- `crates/lbp2900-backend`: CUPS USB backend + `diag` commands
- `crates/lbp2900-app`: menu-bar app and updater
- `ppd/`, `packaging/`: printer description and installer
