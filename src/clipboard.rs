//! Put a short piece of text on the system clipboard.
//!
//! Slint exposes no clipboard API, so this talks to the platform directly. Only
//! used for the small "copy this value" buttons in the UI, so it deliberately
//! stays dependency-free: Win32 on Windows, and whichever of the usual
//! command-line helpers exists on Linux.

/// Copy `text` to the clipboard.
pub fn set_text(text: &str) -> anyhow::Result<()> {
    set_text_impl(text)
}

#[cfg(target_os = "windows")]
fn set_text_impl(text: &str) -> anyhow::Result<()> {
    use windows::Win32::Foundation::{HANDLE, HWND};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

    /// `CF_UNICODETEXT`. Spelled out rather than pulled from the OLE bindings so
    /// this needs no extra crate feature.
    const CF_UNICODETEXT: u32 = 13;

    // NUL-terminated UTF-16, which is what CF_UNICODETEXT expects.
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = std::mem::size_of_val(wide.as_slice());

    unsafe {
        OpenClipboard(HWND::default()).map_err(|e| anyhow::anyhow!("OpenClipboard: {e}"))?;
        // From here on every path must close the clipboard, so the work happens
        // in a closure and the handle is released once afterwards.
        let result = (|| -> anyhow::Result<()> {
            EmptyClipboard().map_err(|e| anyhow::anyhow!("EmptyClipboard: {e}"))?;
            let mem = GlobalAlloc(GMEM_MOVEABLE, bytes)
                .map_err(|e| anyhow::anyhow!("GlobalAlloc: {e}"))?;
            let dst = GlobalLock(mem);
            if dst.is_null() {
                anyhow::bail!("GlobalLock returned null");
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), dst as *mut u16, wide.len());
            let _ = GlobalUnlock(mem);
            // Ownership of `mem` transfers to the clipboard on success; on
            // failure it leaks one small block, which beats a double free.
            SetClipboardData(CF_UNICODETEXT, HANDLE(mem.0))
                .map_err(|e| anyhow::anyhow!("SetClipboardData: {e}"))?;
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

#[cfg(not(target_os = "windows"))]
fn set_text_impl(text: &str) -> anyhow::Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // Wayland first, then the two X11 helpers. Whichever is installed wins.
    let candidates: [(&str, &[&str]); 3] = [
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];
    for (bin, args) in candidates {
        let child = Command::new(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else { continue };
        if let Some(stdin) = child.stdin.as_mut() {
            // No trailing newline — the value is pasted into a form field.
            let _ = stdin.write_all(text.as_bytes());
        }
        drop(child.stdin.take());
        if child.wait().map(|s| s.success()).unwrap_or(false) {
            return Ok(());
        }
    }
    anyhow::bail!("no clipboard helper found (install wl-clipboard, xclip or xsel)")
}

/// Read the clipboard back. Test-only: the app never needs to read, and adding
/// it to the public surface would invite use it doesn't have.
#[cfg(all(test, target_os = "windows"))]
fn get_text() -> Option<String> {
    use windows::Win32::Foundation::{HGLOBAL, HWND};
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};

    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        OpenClipboard(HWND::default()).ok()?;
        let out = GetClipboardData(CF_UNICODETEXT).ok().and_then(|h| {
            let mem = HGLOBAL(h.0);
            let p = GlobalLock(mem) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut len = 0usize;
            while *p.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            let _ = GlobalUnlock(mem);
            Some(s)
        });
        let _ = CloseClipboard();
        out
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    /// Round-trips through the real clipboard, because that is the only thing
    /// worth asserting here — the Win32 dance either hands the OS a valid
    /// CF_UNICODETEXT block or it silently does nothing, and the UI button
    /// cannot tell the difference. Whatever the user had is put back.
    #[test]
    fn round_trips_through_the_system_clipboard() {
        let previous = get_text();

        set_text("http://localhost").expect("set_text should succeed");
        assert_eq!(get_text().as_deref(), Some("http://localhost"));

        // Non-ASCII, to prove the UTF-16 conversion is not byte-truncating.
        set_text("Grüße — ok").expect("set_text should succeed");
        assert_eq!(get_text().as_deref(), Some("Grüße — ok"));

        if let Some(prev) = previous {
            let _ = set_text(&prev);
        }
    }
}
