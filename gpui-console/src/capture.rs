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

/// The id of this process's first normal window, if it has one on screen.
fn own_window() -> Option<u32> {
    let pid = std::process::id() as i64;
    let info = copy_window_info(kCGWindowListOptionOnScreenOnly, kCGNullWindowID)?;
    for i in 0..info.len() {
        let dict: CFDictionary<CFString, core_foundation::base::CFType> =
            unsafe { CFDictionary::wrap_under_get_rule(*info.get(i)? as _) };
        let number = |key: CFStringRef| {
            dict.find(unsafe { CFString::wrap_under_get_rule(key) })
                .and_then(|v| v.downcast::<CFNumber>())
                .and_then(|n| n.to_i64())
        };
        let (owner, layer) = unsafe { (number(kCGWindowOwnerPID), number(kCGWindowLayer)) };
        if owner == Some(pid) && layer == Some(0) {
            return unsafe { number(kCGWindowNumber) }.map(|n| n as u32);
        }
    }
    None
}

/// Writes the window as a PNG at the display's own resolution. Returns its size in pixels.
pub fn own_window_png(path: &str) -> Result<(u32, u32), String> {
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
    let file = std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().and_then(|mut wr| wr.write_image_data(&rgba)).map_err(|e| e.to_string())?;
    Ok((w as u32, h as u32))
}
