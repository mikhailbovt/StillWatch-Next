use sw_domain::Keypoint;

use crate::Image;

type Point = (f64, f64);

pub(crate) fn torso_histogram(image: Image<'_>, joints: &[Keypoint]) -> Vec<f64> {
    if joints.len() < 13 || [5, 6, 11, 12].into_iter().any(|i| joints[i].2 < 0.4) {
        return Vec::new();
    }
    let mut points = [5, 6, 12, 11].map(|i| (joints[i].0, joints[i].1));
    let center = (
        points.iter().map(|p| p.0).sum::<f64>() / 4.0,
        points.iter().map(|p| p.1).sum::<f64>() / 4.0,
    );
    for point in &mut points {
        point.0 = center.0 + (point.0 - center.0) * 0.8;
        point.1 = center.1 + (point.1 - center.1) * 0.8;
    }
    let hull = hull(points);
    if hull.len() < 3 {
        return Vec::new();
    }
    let edges = || {
        hull.iter()
            .zip(hull.iter().cycle().skip(1))
            .take(hull.len())
    };
    let area = edges()
        .map(|(a, b)| a.0 * b.1 - a.1 * b.0)
        .sum::<f64>()
        .abs()
        * 0.5;
    if area < 64.0 {
        return Vec::new();
    }
    let left = hull.iter().map(|p| p.0.floor() as usize).min().unwrap_or(0);
    let top = hull.iter().map(|p| p.1.floor() as usize).min().unwrap_or(0);
    let right = hull
        .iter()
        .map(|p| p.0.ceil() as usize + 1)
        .max()
        .unwrap_or(0)
        .min(image.width);
    let bottom = hull
        .iter()
        .map(|p| p.1.ceil() as usize + 1)
        .max()
        .unwrap_or(0)
        .min(image.height);
    if right.saturating_sub(left) < 4 || bottom.saturating_sub(top) < 4 {
        return Vec::new();
    }

    let rounded: Vec<_> = hull
        .iter()
        .map(|p| (p.0.round_ties_even(), p.1.round_ties_even()))
        .collect();
    let polygon: Vec<_> = rounded
        .iter()
        .zip(rounded.iter().cycle().skip(1))
        .take(rounded.len())
        .collect();
    let mut bins = [0u32; 16];
    for y in top..bottom {
        for x in left..right {
            if polygon
                .iter()
                .all(|(a, b)| cross(**a, **b, (x as f64, y as f64)) >= 0.0)
            {
                let offset = (y * image.width + x) * 3;
                bins[bin(
                    image.rgb[offset],
                    image.rgb[offset + 1],
                    image.rgb[offset + 2],
                )] += 1;
            }
        }
    }
    let count = bins.iter().sum::<u32>();
    if count < 48 {
        return Vec::new();
    }
    (0usize..16)
        .map(|i| {
            let (before, after) = if i < 12 {
                ((i + 11) % 12, (i + 1) % 12)
            } else {
                (i.saturating_sub(1).max(12), (i + 1).min(15))
            };
            f64::from(bins[i] * 2 + bins[before] + bins[after]) / f64::from(count * 4)
        })
        .collect()
}

fn hull(mut points: [Point; 4]) -> Vec<Point> {
    points.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)));
    fn chain(points: impl Iterator<Item = Point>) -> Vec<Point> {
        let mut hull = Vec::with_capacity(4);
        for point in points {
            while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], point) <= 0.0
            {
                hull.pop();
            }
            hull.push(point);
        }
        hull.pop();
        hull
    }
    let mut hull = chain(points.into_iter());
    hull.extend(chain(points.into_iter().rev()));
    hull
}

fn cross(a: Point, b: Point, point: Point) -> f64 {
    (b.0 - a.0) * (point.1 - a.1) - (b.1 - a.1) * (point.0 - a.0)
}

fn bin(r: u8, g: u8, b: u8) -> usize {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let difference = f64::from(max - min);
    if max < 32 || (difference * 255.0 / f64::from(max)).round_ties_even() < 64.0 {
        return 12 + usize::from(max) / 64;
    }
    let (r, g, b) = (f64::from(r), f64::from(g), f64::from(b));
    let hue = if r == f64::from(max) {
        (g - b) / difference
    } else if g == f64::from(max) {
        (b - r) / difference + 2.0
    } else {
        (r - g) / difference + 4.0
    };
    (((hue * 30.0).round_ties_even() as i32).rem_euclid(180) / 15) as usize
}
