// Procedural app icon, shared by build.rs (exe icon) and the app (window and
// tray icons). Plain std, no dependencies.

/// A gradient shield with a check mark, as straight RGBA, `size` x `size`.
pub fn shield_rgba(size: u32) -> Vec<u8> {
    const SS: u32 = 4; // supersampling per axis
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut shield, mut mark) = (0u32, 0u32);
            let mut color = [0.0f32; 3];
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = ((px * SS + sx) as f32 + 0.5) / (size * SS) as f32 * 2.0 - 1.0;
                    let y = ((py * SS + sy) as f32 + 0.5) / (size * SS) as f32 * 2.0 - 1.0;
                    if inside_shield(x, y) {
                        shield += 1;
                        let t = ((y + 0.8) / 1.7).clamp(0.0, 1.0);
                        let (a, b) = ([0.30f32, 0.90, 0.68], [0.12f32, 0.62, 0.50]);
                        for i in 0..3 {
                            color[i] += a[i] + (b[i] - a[i]) * t;
                        }
                        if on_check(x, y) {
                            mark += 1;
                        }
                    }
                }
            }
            let n = (SS * SS) as f32;
            if shield == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            let s = shield as f32;
            let m = mark as f32 / s;
            for c in color {
                let base = c / s;
                out.push(((base + (1.0 - base) * m) * 255.0).round() as u8);
            }
            out.push((s / n * 255.0).round() as u8);
        }
    }
    out
}

/// The same shield as a macOS menu-bar "template" image: black, with the
/// check mark cut out, using only alpha. macOS recolours it to suit the light
/// or dark menu bar. `on` is solid; off is faded, so the two states read at a
/// glance without relying on colour.
#[allow(dead_code)] // build.rs shares this file but only needs `shield_rgba`
pub fn shield_template_rgba(size: u32, on: bool) -> Vec<u8> {
    const SS: u32 = 4;
    let fade = if on { 1.0 } else { 0.4 };
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let mut covered = 0u32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = ((px * SS + sx) as f32 + 0.5) / (size * SS) as f32 * 2.0 - 1.0;
                    let y = ((py * SS + sy) as f32 + 0.5) / (size * SS) as f32 * 2.0 - 1.0;
                    if inside_shield(x, y) && !on_check(x, y) {
                        covered += 1;
                    }
                }
            }
            let alpha = covered as f32 / (SS * SS) as f32 * fade;
            out.extend_from_slice(&[0, 0, 0, (alpha * 255.0).round() as u8]);
        }
    }
    out
}

fn inside_shield(x: f32, y: f32) -> bool {
    const TOP: f32 = -0.82;
    const HALF: f32 = 0.72;
    const R: f32 = 0.2;
    const SHOULDER: f32 = 0.05;
    const TIP: f32 = 0.92;
    let ax = x.abs();
    if y < TOP || y > TIP || ax > HALF {
        return false;
    }
    if y > SHOULDER {
        let t = (y - SHOULDER) / (TIP - SHOULDER);
        return ax <= HALF * (1.0 - t * t).max(0.0).sqrt();
    }
    if y < TOP + R && ax > HALF - R {
        let (dx, dy) = (ax - (HALF - R), y - (TOP + R));
        return dx * dx + dy * dy <= R * R;
    }
    true
}

fn on_check(x: f32, y: f32) -> bool {
    const W: f32 = 0.085;
    segment_distance(x, y, -0.32, 0.0, -0.08, 0.26) <= W
        || segment_distance(x, y, -0.08, 0.26, 0.34, -0.22) <= W
}

fn segment_distance(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    let (cx, cy) = (ax + t * dx, ay + t * dy);
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(px: &[u8], size: u32, x: u32, y: u32) -> u8 {
        px[((y * size + x) * 4 + 3) as usize]
    }

    #[test]
    fn template_is_black_with_the_check_cut_out_and_fades_when_off() {
        let size = 44;
        let on = shield_template_rgba(size, true);
        let off = shield_template_rgba(size, false);
        assert!(on.as_chunks::<4>().0.iter().all(|p| p[..3] == [0, 0, 0]));
        // A point on the shield body is solid when on and faded when off.
        assert_eq!(alpha(&on, size, 5 + 22 - 5, 12), 255);
        assert!(alpha(&off, size, 17, 12) > 90 && alpha(&off, size, 17, 12) < 110);
        // The check mark (around x=-0.08,y=0.26 in unit space) is transparent.
        let (cx, cy) = (((-0.08f32 + 1.0) / 2.0 * 44.0) as u32, ((0.26f32 + 1.0) / 2.0 * 44.0) as u32);
        assert_eq!(alpha(&on, size, cx, cy), 0);
        // Outside the shield is empty.
        assert_eq!(alpha(&on, size, 0, 0), 0);
    }
}
