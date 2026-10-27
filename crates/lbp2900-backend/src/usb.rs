//! USB access to CAPT printers (printer-class interface, bulk endpoints).

use std::io;
use std::time::Duration;

use capt::Transport;
use rusb::{Device, DeviceHandle, Direction, GlobalContext, Recipient, RequestType, TransferType};

pub const CANON_VID: u16 = 0x04A9;
/// Known CAPT models on the P1080 engine path.
pub const KNOWN_PIDS: &[(u16, &str)] = &[(0x2676, "LBP2900"), (0x2684, "LBP3000")];

pub struct Found {
    pub device: Device<GlobalContext>,
    pub pid: u16,
    pub serial: String,
    pub device_id: String,
    pub model: String,
    iface: u8,
    alt: u8,
    ep_out: u8,
    ep_in: u8,
}

impl Found {
    pub fn uri(&self) -> String {
        format!("lbp2900://Canon/{}?serial={}", self.model, self.serial)
    }
}

fn to_io(e: rusb::Error) -> io::Error {
    let kind = match e {
        rusb::Error::Timeout => io::ErrorKind::TimedOut,
        rusb::Error::NoDevice | rusb::Error::NotFound => io::ErrorKind::NotFound,
        rusb::Error::Access => io::ErrorKind::PermissionDenied,
        rusb::Error::Busy => io::ErrorKind::ResourceBusy,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, format!("USB: {e}"))
}

/// Value of `KEY:` in an IEEE 1284 device id.
pub fn id_field<'a>(id: &'a str, keys: &[&str]) -> Option<&'a str> {
    id.split(';').find_map(|part| {
        let (k, v) = part.split_once(':')?;
        keys.iter().any(|key| k.trim().eq_ignore_ascii_case(key)).then(|| v.trim())
    })
}

fn read_device_id(h: &DeviceHandle<GlobalContext>, iface: u8, alt: u8) -> Option<String> {
    let mut buf = [0u8; 1024];
    let rt = rusb::request_type(Direction::In, RequestType::Class, Recipient::Interface);
    let n = h
        .read_control(rt, 0, 0, (iface as u16) << 8 | alt as u16, &mut buf, Duration::from_secs(2))
        .ok()?;
    if n < 2 {
        return None;
    }
    let len = (u16::from_be_bytes([buf[0], buf[1]]) as usize).clamp(2, n);
    Some(String::from_utf8_lossy(&buf[2..len]).into_owned())
}

/// All connected Canon CAPT printers we can drive.
pub fn find_printers() -> Vec<Found> {
    let mut out = Vec::new();
    let Ok(list) = rusb::devices() else { return out };
    for device in list.iter() {
        let Ok(desc) = device.device_descriptor() else { continue };
        if desc.vendor_id() != CANON_VID {
            continue;
        }
        let Ok(config) = device.active_config_descriptor().or_else(|_| device.config_descriptor(0)) else {
            continue;
        };
        // Printer-class interface with bulk IN and OUT endpoints.
        let mut ep = None;
        for iface in config.interfaces() {
            for d in iface.descriptors() {
                if d.class_code() != 7 {
                    continue;
                }
                let bulk = |dir| {
                    d.endpoint_descriptors()
                        .find(|e| e.transfer_type() == TransferType::Bulk && e.direction() == dir)
                        .map(|e| e.address())
                };
                if let (Some(o), Some(i)) = (bulk(Direction::Out), bulk(Direction::In)) {
                    ep = Some((iface.number(), d.setting_number(), o, i));
                }
            }
        }
        let Some((iface, alt, ep_out, ep_in)) = ep else { continue };
        let handle = device.open().ok();
        let device_id = handle.as_ref().and_then(|h| read_device_id(h, iface, alt)).unwrap_or_default();
        let serial = handle
            .as_ref()
            .and_then(|h| h.read_serial_number_string_ascii(&desc).ok())
            .unwrap_or_else(|| format!("{:03}-{:03}", device.bus_number(), device.address()));
        let known = KNOWN_PIDS.iter().find(|(p, _)| *p == desc.product_id()).map(|(_, m)| *m);
        let model = id_field(&device_id, &["MDL", "MODEL"])
            .map(|m| m.replace(' ', ""))
            .or_else(|| known.map(String::from))
            .unwrap_or_else(|| format!("CAPT-{:04X}", desc.product_id()));
        let is_capt = id_field(&device_id, &["CMD", "COMMAND SET"]).is_some_and(|c| c.contains("CAPT"));
        if known.is_none() && !is_capt {
            continue;
        }
        out.push(Found { device, pid: desc.product_id(), serial, device_id, model, iface, alt, ep_out, ep_in });
    }
    out
}

pub struct UsbTransport {
    handle: DeviceHandle<GlobalContext>,
    iface: u8,
    ep_out: u8,
    ep_in: u8,
}

impl UsbTransport {
    pub fn open(f: &Found) -> io::Result<UsbTransport> {
        let handle = f.device.open().map_err(to_io)?;
        let _ = handle.set_auto_detach_kernel_driver(true);
        handle.claim_interface(f.iface).map_err(to_io)?;
        if f.alt != 0 {
            handle.set_alternate_setting(f.iface, f.alt).map_err(to_io)?;
        }
        Ok(UsbTransport { handle, iface: f.iface, ep_out: f.ep_out, ep_in: f.ep_in })
    }
}

impl Drop for UsbTransport {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.iface);
    }
}

impl Transport for UsbTransport {
    fn write(&mut self, mut data: &[u8], timeout: Duration) -> io::Result<()> {
        while !data.is_empty() {
            let n = self.handle.write_bulk(self.ep_out, data, timeout).map_err(to_io)?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "USB write returned 0"));
            }
            data = &data[n..];
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        self.handle.read_bulk(self.ep_in, buf, timeout).map_err(to_io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_id() {
        let id = "MFG:Canon;MDL:LBP2900;CMD:CAPT;VER:1.0;CLS:PRINTER;";
        assert_eq!(id_field(id, &["MDL", "MODEL"]), Some("LBP2900"));
        assert_eq!(id_field(id, &["CMD"]), Some("CAPT"));
        assert_eq!(id_field(id, &["SERN"]), None);
    }
}
