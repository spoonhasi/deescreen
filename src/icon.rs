// The 32x32 icon, as raw RGBA. See the `mod icon;` declaration in main.rs for why this
// file is shaped the way it is: build.rs include!s it, so it may hold no `//!` doc
// comment and may depend on nothing but std.

/// Draw the 32x32 RGBA icon — a mouse cursor over a dark screen.
pub fn icon_rgba() -> Vec<u8> {
    const N: usize = 32;
    let mut px = vec![0u8; N * N * 4];
    let mut set = |x: usize, y: usize, c: [u8; 4]| {
        if x < N && y < N {
            let i = (y * N + x) * 4;
            px[i..i + 4].copy_from_slice(&c);
        }
    };

    let frame = [0x1e, 0x29, 0x3b, 0xff]; // dark slate — the monitor body
    let screen = [0x38, 0xbd, 0xf8, 0xff]; // sky blue — the screen
    let cursor = [0xff, 0xff, 0xff, 0xff];

    // the body, with the corners cut
    for y in 3..25 {
        for x in 2..30 {
            let corner = !(4..=27).contains(&x) && !(5..=22).contains(&y);
            if !corner {
                set(x, y, frame);
            }
        }
    }
    // the screen
    for y in 6..21 {
        for x in 5..27 {
            set(x, y, screen);
        }
    }
    // the stand
    for y in 25..28 {
        for x in 13..19 {
            set(x, y, frame);
        }
    }
    for x in 8..24 {
        set(x, 28, frame);
        set(x, 29, frame);
    }
    // the cursor arrow, over the screen
    for (i, y) in (11..22).enumerate() {
        let w = (i / 2 + 1).min(7);
        for x in 15..15 + w {
            set(x, y, cursor);
        }
    }

    px
}
