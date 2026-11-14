use sw_domain::{Box2d, Visibility};
use sw_protocol::{ControlledFireRegion, Dimensions, REGION_UNITS};

use crate::Frame;

const COLS: usize = 8;
const ROWS: usize = 5;
const CELLS: usize = COLS * ROWS;
pub(crate) const FINGERPRINT_BYTES: usize = 8 + CELLS;

#[derive(Clone, Copy)]
pub(crate) struct SceneSample {
    pub visibility: Visibility,
    pub fingerprint: [u8; FINGERPRINT_BYTES],
}

pub(crate) fn measure(frame: &Frame) -> SceneSample {
    let header = frame.header;
    measure_rgb(header.width, header.height, &frame.rgb)
}

pub(crate) fn measure_rgb(width: u32, height: u32, image: &[u8]) -> SceneSample {
    let width = width as usize;
    let height = height as usize;
    let mut cells = [0; CELLS];
    let mut overall = 0u32;
    let mut dark = 0;
    let mut bright = 0;
    let mut contrast = 0u32;
    let mut textured = 0;
    for (cell, luma) in cells.iter_mut().enumerate() {
        let mut pixels = [0; 16];
        let mut energy = 0;
        for (i, value) in pixels.iter_mut().enumerate() {
            let x = ((cell % COLS * 8 + i % 4 * 2 + 1) * width / (COLS * 8)).min(width - 1);
            let y = ((cell / COLS * 8 + i / 4 * 2 + 1) * height / (ROWS * 8)).min(height - 1);
            let index = (y * width + x) * 3;
            let rgb = &image[index..index + 3];
            *value =
                (u32::from(rgb[0]) * 77 + u32::from(rgb[1]) * 150 + u32::from(rgb[2]) * 29) >> 8;
            energy += *value;
            dark += usize::from(*value <= 12);
            bright += usize::from(*value >= 247);
        }
        let spread = pixels.iter().copied().max().unwrap_or_default()
            - pixels.iter().copied().min().unwrap_or_default();
        contrast += spread;
        textured += usize::from(spread >= 24);
        *luma = (energy / 16) as u8;
        overall += energy;
    }
    let total = CELLS * 16;
    let mean = overall / total as u32;
    let visibility = if dark * 100 >= total * 95
        || bright * 100 >= total * 95
        || contrast < 5 * CELLS as u32
        || mean < 12
    {
        Visibility::Poor
    } else if dark * 100 >= total * 70
        || bright * 100 >= total * 70
        || textured < CELLS / 5
        || mean < 24
    {
        Visibility::Limited
    } else {
        Visibility::Good
    };
    let mut fingerprint = [0; FINGERPRINT_BYTES];
    fingerprint[..4].copy_from_slice(&(width as u32).to_le_bytes());
    fingerprint[4..8].copy_from_slice(&(height as u32).to_le_bytes());
    fingerprint[8..].copy_from_slice(&cells);
    SceneSample {
        visibility,
        fingerprint,
    }
}

pub(crate) fn different(reference: &[u8], current: &[u8; FINGERPRINT_BYTES]) -> bool {
    if reference.len() != FINGERPRINT_BYTES || reference[..8] != current[..8] {
        return true;
    }
    // Remove global exposure offset. A dark frame by itself suspends use of
    // the context; a spatial change observed with good visibility invalidates it.
    let average = |fingerprint: &[u8]| {
        fingerprint[8..].iter().map(|&v| i32::from(v)).sum::<i32>() / CELLS as i32
    };
    let exposure = average(current) - average(reference);
    let shifted = reference[8..]
        .iter()
        .zip(&current[8..])
        .filter(|(a, b)| (i32::from(**b) - i32::from(**a) - exposure).abs() > 35)
        .count();
    shifted >= CELLS * 2 / 5
}

pub(crate) fn overlap(
    region: Box2d,
    dimensions: Dimensions,
    zones: &[ControlledFireRegion],
) -> Option<f64> {
    if region.area() <= 0.0 || zones.is_empty() {
        return None;
    }
    let width = f64::from(dimensions.width) / f64::from(REGION_UNITS);
    let height = f64::from(dimensions.height) / f64::from(REGION_UNITS);
    zones
        .iter()
        .map(|zone| {
            let mut polygon: Vec<_> = zone
                .polygon
                .iter()
                .map(|p| [f64::from(p.x) * width, f64::from(p.y) * height])
                .collect();
            for (axis, bound, lower) in [
                (0, region.x1, true),
                (0, region.x2, false),
                (1, region.y1, true),
                (1, region.y2, false),
            ] {
                let Some(&last) = polygon.last() else {
                    return 0.0;
                };
                let inside = |point: [f64; 2]| {
                    if lower {
                        point[axis] >= bound
                    } else {
                        point[axis] <= bound
                    }
                };
                let mut old = last;
                let mut output = Vec::with_capacity(polygon.len() + 1);
                for point in polygon {
                    if inside(point) != inside(old) {
                        let mix = (bound - old[axis]) / (point[axis] - old[axis]);
                        let mut crossing = [0.0; 2];
                        crossing[axis] = bound;
                        crossing[1 - axis] =
                            old[1 - axis] + mix * (point[1 - axis] - old[1 - axis]);
                        output.push(crossing);
                    }
                    if inside(point) {
                        output.push(point);
                    }
                    old = point;
                }
                polygon = output;
            }
            let area = polygon
                .iter()
                .zip(polygon.iter().cycle().skip(1))
                .map(|(a, b)| a[0] * b[1] - a[1] * b[0])
                .sum::<f64>()
                .abs()
                * 0.5;
            (area / region.area()).clamp(0.0, 1.0)
        })
        .max_by(f64::total_cmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sw_protocol::{ControlledMode, ControlledSource, RegionPoint};

    #[test]
    fn reviewed_polygon_uses_area_and_persistent_camera_changes_invalidate_context() {
        let polygon = vec![
            RegionPoint { x: 0, y: 0 },
            RegionPoint { x: 10_000, y: 0 },
            RegionPoint { x: 0, y: 10_000 },
        ];
        let zone = ControlledFireRegion {
            source: ControlledSource::Candle,
            mode: ControlledMode::Steady,
            polygon,
        };
        let region = Box2d {
            x1: 0.0,
            y1: 0.0,
            x2: 80.0,
            y2: 60.0,
        };
        assert_eq!(
            overlap(
                region,
                Dimensions {
                    width: 80,
                    height: 60
                },
                &[zone]
            ),
            Some(0.5)
        );
        let mut a = [0u8; FINGERPRINT_BYTES];
        let mut b = a;
        a[8..].fill(90);
        b[8..].fill(110);
        assert!(!different(&a, &b));
        b[8..28].fill(210);
        assert!(different(&a, &b));
        b = a;
        b[0] = 1;
        assert!(different(&a, &b));
    }
}
