//! Puts the program's own icon on the exe.
//!
//! The drawing lives in `src/icon.rs` and is `include!`d rather than imported, because a build
//! script is compiled before the crate exists and cannot call into it. That file depends on
//! nothing but `std` so this works, and it means the tray, the browser tab and the file in
//! Explorer are one drawing rather than three that agree until they do not.

include!("src/icon.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }

    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let ico = out.join("deescreen.ico");
    std::fs::write(&ico, ico_bytes(&icon_rgba(), 32)).expect("write the icon");

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().expect("icon path is UTF-8"));
    // Not fatal. Without the resource compiler the exe simply keeps the default icon, and a
    // build that fails over a picture would be a worse trade than one that looks plainer.
    if let Err(e) = res.compile() {
        println!("cargo:warning=could not embed the icon ({e}) — the exe keeps the default one");
    }
}

/// Wrap raw RGBA in the .ico container, uncompressed.
///
/// Written out by hand rather than pulled in as a dependency: it is a 6-byte directory, a
/// 16-byte entry and a bitmap header, and a build script that needs an image library to place
/// 4 kB of pixels is paying more than the pixels are worth.
///
/// Two things the format insists on and neither is obvious: the bitmap is stored **bottom-up**,
/// and its declared height is **twice** the real one, because the format predates alpha and
/// still reserves room for the transparency mask that a 32-bit image does not use.
fn ico_bytes(rgba: &[u8], n: u32) -> Vec<u8> {
    let stride = (n * 4) as usize;
    let mask_stride = (n.div_ceil(32) * 4) as usize; // 1bpp, rows padded to 4 bytes
    let image_len = 40 + stride * n as usize + mask_stride * n as usize;

    let mut v = Vec::with_capacity(22 + image_len);
    v.extend_from_slice(&0u16.to_le_bytes()); // reserved
    v.extend_from_slice(&1u16.to_le_bytes()); // 1 = icon
    v.extend_from_slice(&1u16.to_le_bytes()); // one image

    v.push(n as u8); // 0 would mean 256
    v.push(n as u8);
    v.push(0); // palette size, 0 for truecolour
    v.push(0); // reserved
    v.extend_from_slice(&1u16.to_le_bytes()); // colour planes
    v.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
    v.extend_from_slice(&(image_len as u32).to_le_bytes());
    v.extend_from_slice(&22u32.to_le_bytes()); // the image starts right after this entry

    // BITMAPINFOHEADER
    v.extend_from_slice(&40u32.to_le_bytes());
    v.extend_from_slice(&n.to_le_bytes());
    v.extend_from_slice(&(n * 2).to_le_bytes()); // colour rows + mask rows
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&32u16.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    v.extend_from_slice(&0u32.to_le_bytes()); // size, may be 0 for BI_RGB
    for _ in 0..4 {
        v.extend_from_slice(&0u32.to_le_bytes()); // resolution and palette counts
    }

    // BGRA, bottom row first
    for y in (0..n as usize).rev() {
        for x in 0..n as usize {
            let p = &rgba[(y * n as usize + x) * 4..][..4];
            v.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    // The mask is unused at 32bpp, but leaving it out makes the file the wrong length.
    v.extend_from_slice(&vec![0u8; mask_stride * n as usize]);
    v
}
