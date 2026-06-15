//! Serial port discovery, read loop, and hot-plug handling.
//!
//! Runs on a dedicated thread. Emits parsed [`Frame`]s plus connection state
//! changes through a [`crossbeam_channel::Sender`]. The thread loops forever:
//! find a port → open → read until error → wait & rescan.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use parking_lot::Mutex;
use serialport::SerialPortType;

use crate::protocol::{Frame, FrameReader, BAUD};

#[derive(Debug)]
pub enum SerialEvent {
    Connected(String),
    Disconnected { reason: String, retrying_in: Duration },
    Frame(Frame),
}

/// Writable handle to the current serial session. Cloned cheaply; holds `None`
/// while disconnected. Used to push LED commands (`L`/`B`/`H`) back to the board
/// (see LED-Steuerung.md). The read loop owns one clone of the port; this owns
/// another (via `try_clone`) so writes don't contend with the blocking read.
#[derive(Clone, Default)]
pub struct SerialLink {
    port: Arc<Mutex<Option<Box<dyn serialport::SerialPort>>>>,
}

impl SerialLink {
    /// Write one newline-terminated command line. Returns false (and drops the
    /// handle) if disconnected or the write fails.
    pub fn write_line(&self, line: &str) -> bool {
        let mut guard = self.port.lock();
        let Some(port) = guard.as_mut() else { return false };
        let mut buf = Vec::with_capacity(line.len() + 1);
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
        match port.write_all(&buf).and_then(|_| port.flush()) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("serial write failed: {e}");
                *guard = None;
                false
            }
        }
    }

    /// True while a board is connected and writable.
    pub fn is_connected(&self) -> bool {
        self.port.lock().is_some()
    }

    fn set(&self, port: Box<dyn serialport::SerialPort>) {
        *self.port.lock() = Some(port);
    }
    fn clear(&self) {
        *self.port.lock() = None;
    }
}

/// Handle for the UI to nudge the serial worker — pressing "Retry now"
/// bumps this counter, and the worker skips its sleep on its next loop.
#[derive(Clone, Default)]
pub struct RetryKicker {
    seq: Arc<AtomicU64>,
}

impl RetryKicker {
    pub fn kick(&self) {
        self.seq.fetch_add(1, Ordering::Release);
    }
    fn snapshot(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }
}

const KNOWN_USB_VIDS: &[u16] = &[
    0x2341, // Arduino LLC
    0x1A86, // CH340/CH341
    0x0403, // FTDI
    0x10C4, // Silicon Labs CP210x
    0x0D28, // Atmel (Leonardo etc.)
];

pub fn find_arduino_port() -> Option<String> {
    let ports = serialport::available_ports().ok()?;
    for p in ports {
        if let SerialPortType::UsbPort(usb) = &p.port_type {
            if KNOWN_USB_VIDS.contains(&usb.vid) {
                return Some(p.port_name);
            }
            if let Some(prod) = &usb.product {
                if prod.to_lowercase().contains("arduino") {
                    return Some(p.port_name);
                }
            }
        }
    }
    None
}

pub fn spawn(tx: Sender<SerialEvent>) -> (RetryKicker, SerialLink) {
    let kicker = RetryKicker::default();
    let link = SerialLink::default();
    let thread_kicker = kicker.clone();
    let thread_link = link.clone();
    std::thread::Builder::new()
        .name("slidr-serial".into())
        .spawn(move || {
            let mut backoff = Duration::from_millis(750);
            loop {
                let attempted = match find_arduino_port() {
                    Some(port) => {
                        backoff = Duration::from_millis(750); // reset on success-of-find
                        let res = run_session(&port, &tx, &thread_link);
                        thread_link.clear(); // session over: writes are no-ops again
                        match res {
                            Ok(()) => {
                                let _ = tx.send(SerialEvent::Disconnected {
                                    reason: "Device unplugged".into(),
                                    retrying_in: backoff,
                                });
                                Some(port)
                            }
                            Err(e) => {
                                let msg = friendly(&e.to_string());
                                log::info!("serial session ended on {port}: {e}");
                                let _ = tx.send(SerialEvent::Disconnected {
                                    reason: format!("{port}: {msg}"),
                                    retrying_in: backoff,
                                });
                                backoff = (backoff * 2).min(Duration::from_secs(5));
                                Some(port)
                            }
                        }
                    }
                    None => {
                        let _ = tx.send(SerialEvent::Disconnected {
                            reason: "No Slidr device found".into(),
                            retrying_in: backoff,
                        });
                        None
                    }
                };
                let _ = attempted;

                // Interruptible sleep — wake immediately if the user clicked Retry.
                let baseline = thread_kicker.snapshot();
                let deadline = Instant::now() + backoff;
                loop {
                    if thread_kicker.snapshot() != baseline {
                        backoff = Duration::from_millis(750);
                        break;
                    }
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }
                    std::thread::sleep((deadline - now).min(Duration::from_millis(100)));
                }
            }
        })
        .expect("spawn serial thread");
    (kicker, link)
}

fn friendly(err: &str) -> String {
    // Translate the common platform error strings to something a human can act on.
    let l = err.to_lowercase();
    if l.contains("zugriff verweigert") || l.contains("access is denied") || l.contains("permissiondenied") {
        "Access denied — the port is held by another program (or you lack permission)".into()
    } else if l.contains("nicht gefunden") || l.contains("not found") || l.contains("nosuchdevice") {
        "Device disappeared".into()
    } else {
        err.to_string()
    }
}

fn run_session(port_name: &str, tx: &Sender<SerialEvent>, link: &SerialLink) -> anyhow::Result<()> {
    let mut sp = serialport::new(port_name, BAUD)
        .timeout(Duration::from_millis(100))
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .flow_control(serialport::FlowControl::None)
        .open()?;

    // Toggle DTR to reset Arduino's bootloader path consistently.
    let _ = sp.write_data_terminal_ready(true);
    let _ = sp.write_request_to_send(false);

    // A second handle on the same port for outbound LED commands. The read loop
    // below keeps `sp`; the clone goes to the shared link for the actuator.
    match sp.try_clone() {
        Ok(writer) => link.set(writer),
        Err(e) => log::warn!("serial: cannot clone port for writes ({e}); LED output disabled"),
    }

    let _ = tx.send(SerialEvent::Connected(port_name.to_string()));

    let mut reader = FrameReader::new();
    let mut buf = [0u8; 1024];
    loop {
        match sp.read(&mut buf) {
            Ok(0) => continue,
            Ok(n) => {
                for frame in reader.push(&buf[..n]) {
                    if tx.send(SerialEvent::Frame(frame)).is_err() {
                        return Ok(());
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(e.into()),
        }
    }
}
