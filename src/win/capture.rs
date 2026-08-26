//! Capturing the client area — `PrintWindow` first, a screen `BitBlt` as fallback.
//!
//! ## Why only the client area
//!
//! Button coordinates are client-relative, so the capture has to use the same system
//! for **a pixel measured in the PNG to be writable straight into the profile file**. Include
//! the frame and the image's (0,0) stops being the coordinate system's (0,0), which means
//! doing offset arithmetic in your head every time — and when that arithmetic is wrong, it
//! quietly presses the wrong place.
//!
//! ## Why PrintWindow comes first
//!
//! `PrintWindow` asks the window to draw itself into a DC, so the content comes through even
//! when the window is covered. A screen `BitBlt` scrapes whatever is on screen, so a covered
//! area arrives as a picture of whatever is covering it.
//!
//! Some applications (DirectX, some hardware-accelerated UI) hand `PrintWindow` a black
//! frame. So **when the result is entirely black it retries with a screen `BitBlt`**, and the
//! response's `method` says which one produced the image. Both black is the signature of a
//! locked session or a disconnected RDP — it returns the reason rather than inventing
//! a value.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HBITMAP, HDC, HGDIOBJ, RGBQUAD, ReleaseDC,
    SRCCOPY, SelectObject,
};
use windows_sys::Win32::Storage::Xps::{PW_CLIENTONLY, PrintWindow};

use super::hwnd;
use super::window::WindowInfo;

/// A value windows-sys does not expose as a constant. It makes windows drawn through
/// DirectComposition render their content (Chromium-based, WPF and so on). Without it, those
/// come out as a blank white or black panel.
const PW_RENDERFULLCONTENT: u32 = 0x0000_0002;

/// A capture — RGBA8, top-down.
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// `printwindow` | `bitblt` | `none`
    pub method: &'static str,
    /// Every pixel is black. Usually this means the capture failed, not that the screen is black.
    pub black: bool,
}

/// GDI object lifetime, so an early return part-way through does not leak.
struct Dib {
    screen: HDC,
    mem: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut c_void,
    w: i32,
    h: i32,
}

impl Dib {
    fn new(w: i32, h: i32) -> Result<Dib, String> {
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            if screen.is_null() {
                return Err("GetDC(screen) failed".into());
            }
            let mem = CreateCompatibleDC(screen);
            if mem.is_null() {
                ReleaseDC(std::ptr::null_mut(), screen);
                return Err("CreateCompatibleDC failed".into());
            }
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    // Negative = top-down. That makes the first row in memory the first row
                    // of the image, so nothing has to be flipped on the way to a PNG.
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [RGBQUAD { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbReserved: 0 }],
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bmp = CreateDIBSection(screen, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
            if bmp.is_null() || bits.is_null() {
                DeleteDC(mem);
                ReleaseDC(std::ptr::null_mut(), screen);
                return Err("CreateDIBSection failed".into());
            }
            let old = SelectObject(mem, bmp as HGDIOBJ);
            Ok(Dib { screen, mem, bmp, old, bits, w, h })
        }
    }

    /// Copy the DIB's BGRA bytes across as RGBA.
    fn to_rgba(&self) -> Vec<u8> {
        let n = (self.w as usize) * (self.h as usize) * 4;
        let src = unsafe { std::slice::from_raw_parts(self.bits as *const u8, n) };
        let mut out = vec![0u8; n];
        for (d, s) in out.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            d[0] = s[2]; // R
            d[1] = s[1]; // G
            d[2] = s[0]; // B
            d[3] = 255; // GDI does not fill in alpha — pin it to opaque
        }
        out
    }

    /// Whether every sampled pixel is pure black — the signature of a failed capture.
    /// (A DIB starts zeroed, so drawing nothing leaves exactly this.)
    fn looks_black(&self) -> bool {
        let n = (self.w as usize) * (self.h as usize) * 4;
        let src = unsafe { std::slice::from_raw_parts(self.bits as *const u8, n) };
        // Sample every 64th pixel — at 1920x1080 that is 30k samples, plenty, and costs
        // essentially nothing.
        src.chunks_exact(4)
            .step_by(64)
            .all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0)
    }

    fn clear(&self) {
        let n = (self.w as usize) * (self.h as usize) * 4;
        unsafe { std::ptr::write_bytes(self.bits as *mut u8, 0, n) };
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem, self.old);
            DeleteObject(self.bmp as HGDIOBJ);
            DeleteDC(self.mem);
            ReleaseDC(std::ptr::null_mut(), self.screen);
        }
    }
}

/// Capture the target window's whole client area.
///
/// `info` has to have been measured immediately before this call — `BitBlt` against a stale
/// origin after the window moved captures whatever is next to it instead.
pub fn capture_client(info: &WindowInfo) -> Result<Shot, String> {
    let (w, h) = info.client_size;
    if w <= 0 || h <= 0 {
        return Err(format!("client area has no size ({w}x{h}) — window may be minimized"));
    }
    // A ceiling, so latching onto the wrong window does not allocate gigabytes.
    if w > 16384 || h > 16384 {
        return Err(format!("client area too large to capture ({w}x{h})"));
    }

    let dib = Dib::new(w, h)?;
    let h_wnd: HWND = hwnd(info.handle);

    // First — PrintWindow (content comes through even when covered)
    let printed = unsafe { PrintWindow(h_wnd, dib.mem, PW_CLIENTONLY | PW_RENDERFULLCONTENT) } != 0;
    if printed && !dib.looks_black() {
        return Ok(Shot { width: w as u32, height: h as u32, rgba: dib.to_rgba(), method: "printwindow", black: false });
    }

    // Second — scrape the screen. Only correct while the window is in front.
    dib.clear();
    let (ox, oy) = info.client_origin;
    let blitted = unsafe {
        BitBlt(dib.mem, 0, 0, w, h, dib.screen, ox, oy, SRCCOPY | CAPTUREBLT)
    } != 0;
    if blitted && !dib.looks_black() {
        return Ok(Shot { width: w as u32, height: h as u32, rgba: dib.to_rgba(), method: "bitblt", black: false });
    }

    // Both black. The screen really might be black, so the image is returned as-is with
    // `black` raised, and the caller attaches the likely reason.
    Ok(Shot {
        width: w as u32,
        height: h as u32,
        rgba: dib.to_rgba(),
        method: if blitted { "bitblt" } else { "none" },
        black: true,
    })
}

/// The common causes of a black capture — carried verbatim into `/health` and capture replies.
pub const BLACK_HINT: &str = "capture came back entirely black — the console session is probably \
locked or the RDP session is disconnected. deescreen needs a logged-in, unlocked interactive \
session on the target PC. (Some GPU-rendered apps also refuse PrintWindow; in that case bring the \
window to the foreground and retry.)";
