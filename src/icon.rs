use tray_icon::Icon;

const SIZE: u32 = 36; // 18pt @2x
const SAMPLES: u32 = 4;

/// Half-width of the shield at height v (0 = top, 1 = bottom), in [-1, 1] units.
fn shield_width(v: f32) -> f32 {
    if v < 0.0 || v > 1.0 {
        0.0
    } else if v < 0.45 {
        0.82
    } else {
        0.82 * (1.0 - ((v - 0.45) / 0.55).powf(1.7))
    }
}

fn inside(u: f32, v: f32, inset: f32) -> bool {
    // Shrink the shape uniformly around its centre to get the inner edge.
    let scale = 1.0 - inset;
    let (u, v) = (u / scale, 0.5 + (v - 0.5) / scale);
    u.abs() <= shield_width(v)
}

/// Template image of a shield: outlined when disconnected, filled when
/// connected. macOS tints template images to match the menu bar.
pub fn shield(filled: bool) -> Icon {
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let mut hits = 0;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let px = x as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
                    // Leave a 3px margin top/bottom.
                    let u = (px / SIZE as f32) * 2.0 - 1.0;
                    let v = (py - 3.0) / (SIZE as f32 - 6.0);
                    let outer = inside(u, v, 0.0);
                    let on = if filled { outer } else { outer && !inside(u, v, 0.22) };
                    hits += on as u32;
                }
            }
            let alpha = (hits * 255 / (SAMPLES * SAMPLES)) as u8;
            rgba.extend_from_slice(&[0, 0, 0, alpha]);
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("valid icon")
}
