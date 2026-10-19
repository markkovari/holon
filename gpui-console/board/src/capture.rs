//! A screenshot of this process's own front window, written as a PNG.
//!
//! Made from inside because that is the only way that needs no permission: another process (the
//! `screencapture` tool, a script) reading this window's pixels needs the Screen Recording grant the
//! terminal it runs from has not been given, and fails with "could not create image from window".
//! A process may read back its own windows. This is how the board is photographed while it is being
//! made, and how a bug in what it draws is shown rather than described.

use core_foundation::base::TCFType;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::display::{kCGNullWindowID, CGPoint, CGRect, CGSize};
use core_graphics::window::{
    copy_window_info, create_image, kCGWindowImageBestResolution,
    kCGWindowImageBoundsIgnoreFraming, kCGWindowLayer, kCGWindowListOptionIncludingWindow,
    kCGWindowListOptionOnScreenOnly, kCGWindowNumber, kCGWindowOwnerPID,
};

// The three places this file leaves safe Rust, each of which CoreFoundation's own API requires:
// reading a `kCG*` key (an `extern static`), and taking a reference the window list still owns.

/// A window list entry, as a dictionary.
fn entry(raw: *const std::ffi::c_void) -> CFDictionary<CFString, core_foundation::base::CFType> {
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { CFDictionary::wrap_under_get_rule(raw as _) } // the list owns it; this only borrows
}

/// One of the `kCG*` window-info keys, as a string.
fn key(raw: CFStringRef) -> CFString {
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { CFString::wrap_under_get_rule(raw) } // a constant CoreGraphics owns
}

/// The window-info keys read here: owner process, layer, number.
fn keys() -> (CFStringRef, CFStringRef, CFStringRef) {
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { (kCGWindowOwnerPID, kCGWindowLayer, kCGWindowNumber) } // `extern static`s, never written
}

/// The id of this process's first normal window if it has one on screen, else its first window on any
/// layer (a pop-up, which is how it is kept drawing while it is photographed).
fn own_window() -> Option<u32> {
    let pid = std::process::id() as i64;
    let (owner_key, layer_key, number_key) = keys();
    let info = copy_window_info(kCGWindowListOptionOnScreenOnly, kCGNullWindowID)?;
    let mut any = None;
    for i in 0..info.len() {
        let dict = entry(*info.get(i)?);
        let number = |k: CFStringRef| {
            dict.find(key(k)).and_then(|v| v.downcast::<CFNumber>()).and_then(|n| n.to_i64())
        };
        if number(owner_key) != Some(pid) {
            continue;
        }
        let id = number(number_key).map(|n| n as u32);
        if number(layer_key) == Some(0) {
            return id;
        }
        any = any.or(id);
    }
    any
}

/// The window's pixels at the display's own resolution, RGBA, with its width and height.
pub fn own_window_rgba() -> Result<(Vec<u8>, usize, usize), String> {
    let id = own_window().ok_or("this process has no window on screen")?;
    // `CGRectNull`: the window's own bounds, whatever they are.
    let null = CGRect::new(&CGPoint::new(f64::INFINITY, f64::INFINITY), &CGSize::new(0.0, 0.0));
    let image = create_image(
        null,
        kCGWindowListOptionIncludingWindow,
        id,
        kCGWindowImageBoundsIgnoreFraming | kCGWindowImageBestResolution,
    )
    .ok_or("the window could not be read back")?;

    let (w, h, stride) = (image.width(), image.height(), image.bytes_per_row());
    let data = image.data();
    let bytes = data.bytes();
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            // BGRA, premultiplied; a window is opaque, so the premultiplication is moot.
            let o = y * stride + x * 4;
            rgba.extend_from_slice(&[bytes[o + 2], bytes[o + 1], bytes[o], 255]);
        }
    }
    Ok((rgba, w, h))
}

/// Writes the window as a PNG at the display's own resolution. Returns its size in pixels.
pub fn own_window_png(path: &str) -> Result<(u32, u32), String> {
    let (rgba, w, h) = own_window_rgba()?;
    let file = std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().and_then(|mut wr| wr.write_image_data(&rgba)).map_err(|e| e.to_string())?;
    Ok((w as u32, h as u32))
}
