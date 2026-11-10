//! Finding fx2lafw devices, uploading firmware and opening them (DEV-1..5).

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use nusb::MaybeFuture;
use nusb::transfer::{ControlIn, ControlOut, ControlType, Recipient};

use crate::Error;
use crate::protocol::*;

const CONTROL_TIMEOUT: Duration = Duration::from_millis(500);
/// How long a freshly loaded device may take to come back (libsigrok: 3000 ms).
const RENUM_TIMEOUT: Duration = Duration::from_millis(3000);
/// libsigrok waits this long before looking for the renumerated device.
const RENUM_FIRST_WAIT: Duration = Duration::from_millis(300);

/// A device model this driver knows.
#[derive(Debug)]
pub struct Profile {
    pub vid: u16,
    pub pid: u16,
    pub vendor: &'static str,
    pub model: &'static str,
    pub firmware: &'static str,
}

pub static PROFILES: &[Profile] = &[Profile {
    vid: 0x0925,
    pid: 0x3881,
    vendor: "Saleae",
    model: "Logic",
    firmware: "fx2lafw-saleae-logic.fw",
}];

/// A supported device seen on the bus (DEV-1).
#[derive(Debug, Clone)]
pub struct FoundDevice {
    pub info: nusb::DeviceInfo,
    pub profile: &'static Profile,
    /// The device already runs fx2lafw (manufacturer "sigrok", product "fx2lafw").
    pub has_firmware: bool,
    /// Bus and port path in sigrok style, e.g. `usb/1-8` or `usb/1-1.4`.
    pub location: String,
}

impl FoundDevice {
    pub fn name(&self) -> String {
        format!("{} {} (fx2lafw)", self.profile.vendor, self.profile.model)
    }
}

/// `usb/<bus>-<port>.<port>`; buses are numbered from 1 in the order the OS
/// lists them (Windows has no bus numbers, only long location paths).
fn location_of(info: &nusb::DeviceInfo, buses: &[String]) -> String {
    let chain: Vec<String> = info.port_chain().iter().map(|p| p.to_string()).collect();
    let bus = buses
        .iter()
        .position(|b| b == info.bus_id())
        .map(|i| (i + 1).to_string())
        .unwrap_or_else(|| info.bus_id().to_string());
    format!("usb/{bus}-{}", chain.join("."))
}

/// List supported devices (DEV-1, DEV-3).
pub fn list_devices() -> Result<Vec<FoundDevice>, Error> {
    let buses: Vec<String> = match nusb::list_buses().wait() {
        Ok(b) => b.map(|b| b.bus_id().to_string()).collect(),
        Err(_) => Vec::new(),
    };
    let mut out = Vec::new();
    for info in nusb::list_devices().wait()? {
        let Some(profile) = PROFILES
            .iter()
            .find(|p| p.vid == info.vendor_id() && p.pid == info.product_id())
        else {
            continue;
        };
        log::debug!(
            "{:04x}:{:04x} manufacturer {:?} product {:?}",
            info.vendor_id(),
            info.product_id(),
            info.manufacturer_string(),
            info.product_string()
        );
        // fx2lafw reports manufacturer "sigrok", product "fx2lafw". Windows
        // may not cache the manufacturer string, so accept it missing.
        let has_firmware = matches!(info.manufacturer_string(), None | Some("sigrok"))
            && info.product_string() == Some("fx2lafw");
        out.push(FoundDevice {
            location: location_of(&info, &buses),
            info,
            profile,
            has_firmware,
        });
    }
    Ok(out)
}

/// Folders searched for firmware, in order (DEV-5).
pub fn default_firmware_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = std::env::var_os("LOGICVIEW_FIRMWARE_DIR") {
        dirs.push(PathBuf::from(d));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dirs.push(dir.join("firmware"));
        dirs.push(dir.join("share").join("sigrok-firmware"));
    }
    for base in [
        std::env::var_os("ProgramFiles"),
        std::env::var_os("ProgramFiles(x86)"),
    ]
    .into_iter()
    .flatten()
    {
        let base = PathBuf::from(base);
        dirs.push(base.join(r"sigrok\PulseView\share\sigrok-firmware"));
        dirs.push(base.join(r"sigrok\sigrok-cli\share\sigrok-firmware"));
    }
    dirs
}

pub fn find_firmware(name: &str, dirs: &[PathBuf]) -> Result<PathBuf, Error> {
    for d in dirs {
        let p = d.join(name);
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(Error::FirmwareNotFound {
        name: name.to_string(),
        searched: dirs.to_vec(),
    })
}

fn vendor_out(intf: &nusb::Interface, request: u8, value: u16, data: &[u8]) -> Result<(), Error> {
    intf.control_out(
        ControlOut {
            control_type: ControlType::Vendor,
            recipient: Recipient::Device,
            request,
            value,
            index: 0,
            data,
        },
        CONTROL_TIMEOUT,
    )
    .wait()?;
    Ok(())
}

fn vendor_in(intf: &nusb::Interface, request: u8, length: u16) -> Result<Vec<u8>, Error> {
    Ok(intf
        .control_in(
            ControlIn {
                control_type: ControlType::Vendor,
                recipient: Recipient::Device,
                request,
                value: 0,
                index: 0,
                length,
            },
            CONTROL_TIMEOUT,
        )
        .wait()?)
}

/// Load the firmware image into the FX2 RAM (DEV-2).
fn upload_firmware(found: &FoundDevice, path: &Path) -> Result<(), Error> {
    let image = std::fs::read(path).map_err(|e| Error::Io(path.to_path_buf(), e))?;
    if image.is_empty() || image.len() > FW_MAX_SIZE {
        return Err(Error::BadFirmware(path.to_path_buf()));
    }
    log::info!("uploading {} to {}", path.display(), found.location);
    let device = found.info.open().wait()?;
    let intf = device.claim_interface(0).wait()?;
    vendor_out(&intf, REQ_FIRMWARE_LOAD, CPUCS_ADDR, &[1])?;
    for (i, chunk) in image.chunks(FW_CHUNK_SIZE).enumerate() {
        vendor_out(&intf, REQ_FIRMWARE_LOAD, (i * FW_CHUNK_SIZE) as u16, chunk)?;
    }
    // Releasing the reset starts fx2lafw, which disconnects and comes back.
    vendor_out(&intf, REQ_FIRMWARE_LOAD, CPUCS_ADDR, &[0])?;
    Ok(())
}

/// An opened fx2lafw device ready to stream.
pub struct Fx2lafw {
    pub(crate) intf: nusb::Interface,
    _device: nusb::Device,
    pub location: String,
    pub name: String,
    pub fw_version: (u8, u8),
    pub revid: u8,
}

impl std::fmt::Debug for Fx2lafw {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fx2lafw")
            .field("location", &self.location)
            .field("fw_version", &self.fw_version)
            .field("revid", &self.revid)
            .finish()
    }
}

impl Fx2lafw {
    /// Whether this handle still reaches the device. A handle kept from an
    /// earlier capture goes stale when the analyzer is unplugged, even if it
    /// is plugged back into the same port (same location).
    pub fn is_alive(&self) -> bool {
        vendor_in(&self.intf, CMD_GET_FW_VERSION, 2).is_ok_and(|v| v.len() >= 2)
    }
}

/// A handle kept from an earlier capture, if it still fits `found`; a stale
/// or different one is dropped (and its interface released) here.
pub fn reuse(cached: Option<Fx2lafw>, found: &FoundDevice) -> Option<Fx2lafw> {
    let d = cached?;
    if d.location != found.location {
        return None;
    }
    if d.is_alive() {
        Some(d)
    } else {
        log::info!("{}: kept handle is stale; opening again", d.location);
        None
    }
}

/// Open the device and check that it runs fx2lafw 1.x (DEV-4).
fn try_open(found: &FoundDevice) -> Result<Fx2lafw, Error> {
    let device = found.info.open().wait()?;
    let intf = device.claim_interface(0).wait()?;
    let v = vendor_in(&intf, CMD_GET_FW_VERSION, 2)?;
    if v.len() < 2 {
        return Err(Error::Protocol("short firmware version reply".into()));
    }
    if v[0] != REQUIRED_FW_MAJOR {
        return Err(Error::FirmwareVersion {
            major: v[0],
            minor: v[1],
        });
    }
    let r = vendor_in(&intf, CMD_GET_REVID_VERSION, 1)?;
    Ok(Fx2lafw {
        intf,
        _device: device,
        location: found.location.clone(),
        name: found.name(),
        fw_version: (v[0], v[1]),
        revid: r.first().copied().unwrap_or(0),
    })
}

/// Open a device, uploading firmware first when needed (DEV-2..5).
pub fn open(found: &FoundDevice, firmware_dirs: &[PathBuf]) -> Result<Fx2lafw, Error> {
    // Ask the device first: the cached strings do not always show that
    // fx2lafw is running (on Windows they can come from the driver INF).
    // The bare FX2 boot loader rejects the version request at once.
    match try_open(found) {
        Ok(d) => return Ok(d),
        Err(e @ Error::FirmwareVersion { .. }) => return Err(e),
        Err(e) => log::info!(
            "{}: no fx2lafw answer ({e}); loading firmware",
            found.location
        ),
    }
    let path = find_firmware(found.profile.firmware, firmware_dirs)?;
    upload_firmware(found, &path)?;

    let start = Instant::now();
    thread::sleep(RENUM_FIRST_WAIT);
    let mut last_err = None;
    while start.elapsed() < RENUM_TIMEOUT {
        if let Some(d) = list_devices()?
            .into_iter()
            .find(|d| d.location == found.location)
        {
            match try_open(&d) {
                Ok(dev) => {
                    log::info!(
                        "{} came back with fx2lafw {}.{} after {} ms",
                        dev.location,
                        dev.fw_version.0,
                        dev.fw_version.1,
                        start.elapsed().as_millis()
                    );
                    return Ok(dev);
                }
                Err(e @ Error::FirmwareVersion { .. }) => return Err(e),
                Err(e) => last_err = Some(e),
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(Error::Renumerate {
        location: found.location.clone(),
        last: last_err.map(|e| e.to_string()),
    })
}
