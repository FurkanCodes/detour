//! Artwork: the world-map background.

use eframe::egui::{Color32, ColorImage};

const LAND: &str = include_str!("../../../assets/map/land-110m.txt");

/// Latitudes shown on the map; Antarctica and the far north are cropped.
pub const MAP_NORTH: f32 = 84.0;
pub const MAP_SOUTH: f32 = -58.0;

fn rings() -> Vec<Vec<(f32, f32)>> {
    LAND.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|line| {
            line.split_whitespace()
                .filter_map(|pair| {
                    let (lon, lat) = pair.split_once(',')?;
                    Some((lon.parse().ok()?, lat.parse().ok()?))
                })
                .collect()
        })
        .filter(|r: &Vec<(f32, f32)>| r.len() >= 3)
        .collect()
}

/// Rasterizes the land masses (equirectangular, 180°W..180°E,
/// `MAP_NORTH`..`MAP_SOUTH`) into a white image whose alpha is land
/// coverage, anti-aliased. Tint it when drawing. Runs once at startup.
pub fn world_map(width: usize) -> ColorImage {
    let height = (width as f32 * (MAP_NORTH - MAP_SOUTH) / 360.0).round() as usize;
    let coverage = rasterize(&rings(), width, height);
    let pixels = coverage
        .into_iter()
        .map(|c| Color32::from_white_alpha((c.clamp(0.0, 1.0) * 255.0) as u8))
        .collect();
    ColorImage::new([width, height], pixels)
}

/// Even-odd scanline fill with 4 sub-rows per pixel and exact horizontal
/// coverage. Returns per-pixel coverage in 0..=1.
fn rasterize(rings: &[Vec<(f32, f32)>], width: usize, height: usize) -> Vec<f32> {
    const SUB: usize = 4;
    let to_px = |(lon, lat): (f32, f32)| {
        (
            (lon + 180.0) / 360.0 * width as f32,
            (MAP_NORTH - lat) / (MAP_NORTH - MAP_SOUTH) * height as f32,
        )
    };
    let edges: Vec<((f32, f32), (f32, f32))> = rings
        .iter()
        .flat_map(|ring| {
            let pts: Vec<(f32, f32)> = ring.iter().copied().map(to_px).collect();
            (0..pts.len())
                .map(move |i| (pts[i], pts[(i + 1) % pts.len()]))
                .collect::<Vec<_>>()
        })
        .filter(|(a, b)| a.1 != b.1)
        .collect();

    let mut cov = vec![0f32; width * height];
    let mut xs = Vec::new();
    for row in 0..height {
        for sub in 0..SUB {
            let y = row as f32 + (sub as f32 + 0.5) / SUB as f32;
            xs.clear();
            for &((x0, y0), (x1, y1)) in &edges {
                if (y0 <= y) != (y1 <= y) {
                    xs.push(x0 + (y - y0) / (y1 - y0) * (x1 - x0));
                }
            }
            xs.sort_unstable_by(f32::total_cmp);
            for span in xs.as_chunks::<2>().0 {
                let (a, b) = (span[0].max(0.0), span[1].min(width as f32));
                if b <= a {
                    continue;
                }
                let line = &mut cov[row * width..(row + 1) * width];
                let (ia, ib) = (a as usize, (b as usize).min(width - 1));
                let w = 1.0 / SUB as f32;
                if ia == ib {
                    line[ia] += (b - a) * w;
                    continue;
                }
                line[ia] += (ia as f32 + 1.0 - a) * w;
                for c in &mut line[ia + 1..ib] {
                    *c += w;
                }
                line[ib] += (b - ib as f32) * w;
            }
        }
    }
    cov
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rasterizer_fills_a_square_with_soft_edges() {
        // A 40° square on a 36-px wide map: 4 px wide, about 3.9 px tall.
        let ring = vec![(0.0, 0.0), (40.0, 0.0), (40.0, -40.0), (0.0, -40.0)];
        let (width, height) = (36usize, 14usize);
        let cov = rasterize(&[ring], width, height);
        let at = |lon: f32, lat: f32| {
            let x = ((lon + 180.0) / 360.0 * width as f32) as usize;
            let y = ((MAP_NORTH - lat) / (MAP_NORTH - MAP_SOUTH) * height as f32) as usize;
            cov[y * width + x]
        };
        assert!(at(20.0, -20.0) > 0.99, "inside is filled");
        assert!(at(-50.0, 30.0) < 0.01, "outside is empty");
        let expected = 4.0 * 40.0 / (MAP_NORTH - MAP_SOUTH) * height as f32;
        let total: f32 = cov.iter().sum();
        // Sub-row sampling quantizes each horizontal edge to 1/4 px.
        assert!((total - expected).abs() < 0.5, "area {total}, expected {expected}");
        assert!(cov.iter().any(|c| *c > 0.05 && *c < 0.95), "edges are anti-aliased");
    }

    #[test]
    fn world_map_has_land_where_expected() {
        let map = world_map(720);
        let (w, h) = (map.size[0], map.size[1]);
        let alpha = |lon: f32, lat: f32| {
            let x = ((lon + 180.0) / 360.0 * w as f32) as usize;
            let y = ((MAP_NORTH - lat) / (MAP_NORTH - MAP_SOUTH) * h as f32) as usize;
            map.pixels[y * w + x].a()
        };
        assert!(alpha(33.0, 39.0) > 200, "central Turkey is land");
        assert!(alpha(-100.0, 40.0) > 200, "central USA is land");
        assert!(alpha(-30.0, 30.0) < 20, "mid Atlantic is water");
        assert!(alpha(160.0, -30.0) < 20, "Tasman Sea is water");
    }
}
