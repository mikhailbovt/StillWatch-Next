use std::sync::Arc;
use sw_domain::{Box2d, SCENE_SIGNATURE_BYTES, SceneSignature, Visibility};

use sw_protocol::{ControlledFireRegion, Dimensions, MaskRect, REGION_UNITS};

const COLS: usize = 8;
const ROWS: usize = 5;
const CELLS: usize = COLS * ROWS;
pub(crate) const FINGERPRINT_BYTES: usize = SCENE_SIGNATURE_BYTES;

#[derive(Clone, Copy)]
pub(crate) struct SceneSample {
    pub visibility: Visibility,
    pub fingerprint: [u8; FINGERPRINT_BYTES],
}

#[cfg(any(test, feature = "diagnostics"))]
pub(crate) fn measure_rgb(width: u32, height: u32, image: &[u8]) -> SceneSample {
    measure_masked(width, height, image, &[])
}

pub(crate) fn measure_masked(
    width: u32,
    height: u32,
    image: &[u8],
    masks: &[PixelMask],
) -> SceneSample {
    let width = width as usize;
    let height = height as usize;
    let mut cells = [0; CELLS];
    let mut overall = 0u32;
    let mut valid = 0;
    let mut valid_cells = 0;
    let mut dark = 0;
    let mut bright = 0;
    let mut contrast = 0u32;
    let mut textured = 0;
    for (cell, luma) in cells.iter_mut().enumerate() {
        let mut pixels = [0; 16];
        let mut energy = 0;
        let mut count = 0;
        for i in 0..pixels.len() {
            let x = ((cell % COLS * 8 + i % 4 * 2 + 1) * width / (COLS * 8)).min(width - 1);
            let y = ((cell / COLS * 8 + i / 4 * 2 + 1) * height / (ROWS * 8)).min(height - 1);
            if covered(masks, x, y) {
                continue;
            }
            let index = (y * width + x) * 3;
            let value = luma_at(&image[index..index + 3]);
            pixels[count] = value;
            count += 1;
            energy += value;
            dark += usize::from(value <= 12);
            bright += usize::from(value >= 247);
        }
        if count == 0 {
            continue;
        }
        let pixels = &pixels[..count];
        let spread = pixels.iter().copied().max().unwrap_or_default()
            - pixels.iter().copied().min().unwrap_or_default();
        contrast += spread;
        textured += usize::from(spread >= 24);
        *luma = (energy / count as u32) as u8;
        overall += energy;
        valid += count;
        valid_cells += 1;
    }
    let mean = overall / valid.max(1) as u32;
    let visibility = if valid == 0
        || dark * 100 >= valid * 95
        || bright * 100 >= valid * 95
        || contrast < 5 * valid_cells as u32
        || mean < 12
    {
        Visibility::Poor
    } else if dark * 100 >= valid * 70
        || bright * 100 >= valid * 70
        || textured * 5 < valid_cells
        || valid < CELLS * 8
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

#[derive(Clone, Copy)]
pub(crate) struct PixelMask {
    left: usize,
    top: usize,
    right: usize,
    bottom: usize,
}

pub(crate) fn pixel_masks(masks: &[MaskRect], width: u32, height: u32) -> Arc<[PixelMask]> {
    masks
        .iter()
        .map(|mask| PixelMask {
            left: (u32::from(mask.left) * width / REGION_UNITS) as usize,
            top: (u32::from(mask.top) * height / REGION_UNITS) as usize,
            right: (u32::from(mask.right) * width).div_ceil(REGION_UNITS) as usize,
            bottom: (u32::from(mask.bottom) * height).div_ceil(REGION_UNITS) as usize,
        })
        .collect::<Vec<_>>()
        .into()
}

fn covered(masks: &[PixelMask], x: usize, y: usize) -> bool {
    masks
        .iter()
        .any(|mask| x >= mask.left && x < mask.right && y >= mask.top && y < mask.bottom)
}

fn luma_at(rgb: &[u8]) -> u32 {
    (u32::from(rgb[0]) * 77 + u32::from(rgb[1]) * 150 + u32::from(rgb[2]) * 29) >> 8
}

pub(crate) struct VisibleFrame<'a> {
    pub width: usize,
    pub height: usize,
    pub rgb: &'a [u8],
    pub masks: &'a [PixelMask],
    pub overview: Visibility,
}

impl<'a> From<&'a crate::CameraFrame> for VisibleFrame<'a> {
    fn from(frame: &'a crate::CameraFrame) -> Self {
        Self {
            width: frame.frame.header.width as usize,
            height: frame.frame.header.height as usize,
            rgb: &frame.frame.rgb,
            masks: &frame.masks,
            overview: frame.visibility,
        }
    }
}

impl VisibleFrame<'_> {
    pub(crate) fn region(&self, region: Box2d, flame: bool) -> Visibility {
        let width = self.width;
        let height = self.height;
        let left = region.x1.floor().max(0.0) as usize;
        let right = (region.x2.ceil() as usize).min(width);
        let top = region.y1.floor().max(0.0) as usize;
        let bottom = (region.y2.ceil() as usize).min(height);
        if right <= left || bottom <= top {
            return Visibility::Poor;
        }
        let columns = (right - left).min(12);
        let rows = (bottom - top).min(12);
        let mut valid = 0;
        let mut visible = 0;
        let mut warm = 0;
        let mut low = 255;
        let mut high = 0;
        for row in 0..rows {
            for col in 0..columns {
                let x = left + (2 * col + 1) * (right - left) / (2 * columns);
                let y = top + (2 * row + 1) * (bottom - top) / (2 * rows);
                if covered(self.masks, x, y) {
                    continue;
                }
                let offset = (y * width + x) * 3;
                let rgb = &self.rgb[offset..offset + 3];
                let luma = luma_at(rgb);
                valid += 1;
                visible += usize::from((16..=245).contains(&luma));
                warm += usize::from(rgb[0] >= 110 && rgb[0].saturating_sub(rgb[2]) >= 40);
                low = low.min(luma);
                high = high.max(luma);
            }
        }
        if valid < 4 || valid * 5 < columns * rows {
            return Visibility::Poor;
        }
        if self.overview == Visibility::Good && visible * 4 >= valid * 3 {
            return if valid * 10 >= columns * rows * 9 {
                Visibility::Good
            } else {
                Visibility::Limited
            };
        }
        if (visible >= 4 && visible * 8 >= valid && high.saturating_sub(low) >= 12)
            || (flame && warm >= 3 && high.saturating_sub(low) >= 20)
        {
            Visibility::Limited
        } else {
            Visibility::Poor
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SceneAlignment {
    Aligned,
    Uncertain,
    Changed,
}

pub(crate) fn alignment(reference: &[u8], current: &SceneSample) -> SceneAlignment {
    if current.visibility != Visibility::Good {
        return SceneAlignment::Uncertain;
    }
    if !SceneSignature(current.fingerprint).matches(reference) {
        return SceneAlignment::Changed;
    }
    // A local shift or obstruction can change a smaller group of cells than
    // the permanent invalidation test. Remove uniform exposure changes before
    // deciding whether to use the operator's polygon for this frame.
    let mut offsets = [0_i16; CELLS];
    for ((offset, &before), &after) in offsets
        .iter_mut()
        .zip(&reference[8..])
        .zip(&current.fingerprint[8..])
    {
        *offset = i16::from(after) - i16::from(before);
    }
    let (_, median, _) = offsets.select_nth_unstable(CELLS / 2);
    let median = *median;
    let residuals = offsets.map(|offset| (offset - median).abs());
    if residuals.iter().filter(|&&offset| offset >= 26).count() >= 8
        || residuals.iter().filter(|&&offset| offset >= 48).count() >= 5
    {
        SceneAlignment::Uncertain
    } else {
        SceneAlignment::Aligned
    }
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
    fn local_visibility_survives_dark_background_and_excludes_redacted_area() {
        let width = 320;
        let height = 240;
        let mut image = vec![0; width * height * 3];
        let region = Box2d {
            x1: 290.0,
            y1: 105.0,
            x2: 316.0,
            y2: 139.0,
        };
        for y in 105..139 {
            for x in 290..316 {
                let offset = (y * width + x) * 3;
                let rgb = if x < 303 {
                    [225, 131, 28]
                } else {
                    [254, 202, 49]
                };
                image[offset..offset + 3].copy_from_slice(&rgb);
            }
        }
        let width_px = width as u32;
        let height_px = height as u32;
        let overview = measure_rgb(width_px, height_px, &image).visibility;
        assert_eq!(overview, Visibility::Poor);
        let view = VisibleFrame {
            width,
            height,
            rgb: &image,
            masks: &[],
            overview,
        };
        assert_eq!(view.region(region, true), Visibility::Limited);
        assert_eq!(view.region(region, false), Visibility::Limited);
        let mask = |left, right| MaskRect {
            left,
            right,
            top: 0,
            bottom: 10_000,
        };
        let masks = pixel_masks(
            &[
                mask(0, 9_050),
                MaskRect {
                    left: 9_050,
                    right: 10_000,
                    top: 0,
                    bottom: 4_375,
                },
                MaskRect {
                    left: 9_050,
                    right: 10_000,
                    top: 5_792,
                    bottom: 10_000,
                },
            ],
            width_px,
            height_px,
        );
        assert_eq!(
            measure_masked(width_px, height_px, &image, &masks).visibility,
            Visibility::Limited
        );
        assert_eq!(
            VisibleFrame {
                masks: &masks,
                ..view
            }
            .region(region, true),
            Visibility::Limited
        );
        let masks = pixel_masks(&[mask(9_050, 10_000)], width_px, height_px);
        assert_eq!(
            VisibleFrame {
                masks: &masks,
                ..view
            }
            .region(region, true),
            Visibility::Poor
        );
        for blank in [0, 255] {
            let flat = vec![blank; width * height * 3];
            let view = VisibleFrame { rgb: &flat, ..view };
            assert_eq!(view.region(region, true), Visibility::Poor);
            assert_eq!(
                measure_rgb(width_px, height_px, &flat).visibility,
                Visibility::Poor
            );
        }
    }

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
        let aligned = |fingerprint, visibility| {
            alignment(
                &a,
                &SceneSample {
                    visibility,
                    fingerprint,
                },
            )
        };
        assert_eq!(aligned(b, Visibility::Good), SceneAlignment::Aligned);
        assert_eq!(aligned(b, Visibility::Limited), SceneAlignment::Uncertain);
        b[10..15].fill(180);
        assert_eq!(aligned(b, Visibility::Good), SceneAlignment::Uncertain);
        b[8..28].fill(210);
        assert_eq!(aligned(b, Visibility::Good), SceneAlignment::Changed);
        b = a;
        b[0] = 1;
        assert_eq!(aligned(b, Visibility::Good), SceneAlignment::Changed);
    }
}
