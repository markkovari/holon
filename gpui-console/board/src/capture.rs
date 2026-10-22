//! Taken from inside: a process may read its own windows, but another one (`screencapture`) needs
//! the Screen Recording grant and fails with "could not create image from window".

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

fn entry(raw: *const std::ffi::c_void) -> CFDictionary<CFString, core_foundation::base::CFType> {
    // SAFETY: `raw` is a dictionary in a window list the caller holds; get-rule retains, so this only borrows.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { CFDictionary::wrap_under_get_rule(raw as _) }
}

fn key(raw: CFStringRef) -> CFString {
    // SAFETY: `raw` is one of CoreGraphics' constant keys, valid for the life of the process.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { CFString::wrap_under_get_rule(raw) }
}

fn keys() -> (CFStringRef, CFStringRef, CFStringRef) {
    // SAFETY: `extern static`s CoreGraphics initialises and never writes.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    unsafe { (kCGWindowOwnerPID, kCGWindowLayer, kCGWindowNumber) }
}

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

pub fn own_window_png(path: &str) -> Result<(u32, u32), String> {
    let (rgba, w, h) = own_window_rgba()?;
    let file = std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().and_then(|mut wr| wr.write_image_data(&rgba)).map_err(|e| e.to_string())?;
    Ok((w as u32, h as u32))
}
