use sw_domain::{Box2d, Keypoint, Observation, ObservationLabel};

use crate::{Image, InferenceError, Letterbox, ModelId, appearance::torso_histogram};

const MAX_DETECTIONS: usize = 300;

struct Detection {
    score: f32,
    label: ObservationLabel,
    anchor: usize,
    coordinates: [f32; 4],
}

pub fn decode(
    id: ModelId,
    values: &[f32],
    image: Image<'_>,
    geometry: Letterbox,
    capture_ts: f64,
) -> Result<Vec<Observation>, InferenceError> {
    let fire = id == ModelId::Fire;
    let anchors = geometry.tensor_size().anchors();
    if values.len() != id.channels() * anchors
        || !capture_ts.is_finite()
        || image.width != geometry.width
        || image.height != geometry.height
    {
        return Err(InferenceError::Tensor);
    }
    let value = |column, anchor| values[column * anchors + anchor];
    let mut candidates = Vec::new();
    for anchor in 0..anchors {
        let first = value(4, anchor);
        let (label, score) = if !fire {
            (ObservationLabel::Person, first)
        } else {
            let second = value(5, anchor);
            if second > first {
                (ObservationLabel::Fire, second)
            } else {
                (ObservationLabel::Smoke, first)
            }
        };
        if !score.is_finite() || !(0.0..=1.0).contains(&score) {
            return Err(InferenceError::Tensor);
        }
        if score <= id.legacy_confidence() {
            continue;
        }
        let (x, y) = (value(0, anchor), value(1, anchor));
        let (half_width, half_height) = (value(2, anchor) * 0.5, value(3, anchor) * 0.5);
        let coordinates = [
            x - half_width,
            y - half_height,
            x + half_width,
            y + half_height,
        ];
        if coordinates.iter().any(|v| !v.is_finite()) || half_width < 0.0 || half_height < 0.0 {
            return Err(InferenceError::Tensor);
        }
        candidates.push(Detection {
            score,
            label,
            anchor,
            coordinates,
        });
    }
    candidates.sort_unstable_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.anchor.cmp(&b.anchor))
    });
    let threshold = if fire { 0.70 } else { 0.50 };
    let mut kept = Vec::<Detection>::new();
    for candidate in candidates {
        if kept.iter().any(|other| {
            other.label == candidate.label
                && iou(other.coordinates, candidate.coordinates) > threshold
        }) {
            continue;
        }
        kept.push(candidate);
        if kept.len() == MAX_DETECTIONS {
            break;
        }
    }
    kept.into_iter()
        .map(|candidate| {
            let [x1, y1, x2, y2] = candidate.coordinates;
            let region = Box2d {
                x1: geometry.x(x1),
                y1: geometry.y(y1),
                x2: geometry.x(x2),
                y2: geometry.y(y2),
            };
            let mut observation = Observation {
                capture_ts,
                region,
                score: f64::from(candidate.score),
                label: candidate.label,
                track_id: None,
                keypoints: Vec::new(),
                limited: false,
                truncated: false,
                association_uncertain: false,
                torso_histogram: Vec::new(),
            };
            if !fire {
                observation.keypoints = (0..17)
                    .map(|joint| {
                        let column = 5 + joint * 3;
                        let x = geometry.x(value(column, candidate.anchor));
                        let y = geometry.y(value(column + 1, candidate.anchor));
                        let confidence = if x > 0.0
                            && x < image.width as f64
                            && y > 0.0
                            && y < image.height as f64
                        {
                            f64::from(value(column + 2, candidate.anchor))
                        } else {
                            0.0
                        };
                        Keypoint(x, y, confidence)
                    })
                    .collect();
                observation.limited = region.height() < 40.0;
                observation.truncated = region.x1 <= 3.0
                    || region.y1 <= 3.0
                    || region.x2 >= image.width as f64 - 3.0
                    || region.y2 >= image.height as f64 - 3.0;
                observation.torso_histogram = torso_histogram(image, &observation.keypoints);
            }
            if !observation.valid() {
                return Err(InferenceError::Tensor);
            }
            Ok(observation)
        })
        .collect()
}

fn iou(a: [f32; 4], b: [f32; 4]) -> f32 {
    let intersection =
        (a[2].min(b[2]) - a[0].max(b[0])).max(0.0) * (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    let area = |p: [f32; 4]| (p[2] - p[0]) * (p[3] - p[1]);
    let union = area(a) + area(b) - intersection;
    if union > 0.0 {
        intersection / union
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use crate::{InputProfile, Preprocessor};

    use super::*;

    #[test]
    fn fire_nms_keeps_overlapping_different_classes_and_uses_strict_threshold() {
        let pixels = vec![0; 32 * 32 * 3];
        let image = Image::rgb(32, 32, &pixels).unwrap();
        const ANCHORS: usize = 8400;
        let (_, geometry) = Preprocessor::new(InputProfile::Square).prepare(image);
        let mut tensor = vec![0.0; 6 * ANCHORS];
        for anchor in 0..4 {
            tensor[anchor] = 320.0;
            tensor[ANCHORS + anchor] = 320.0;
            tensor[2 * ANCHORS + anchor] = 160.0;
            tensor[3 * ANCHORS + anchor] = 160.0;
        }
        tensor[4 * ANCHORS] = 0.95;
        tensor[4 * ANCHORS + 1] = 0.75;
        tensor[5 * ANCHORS + 2] = 0.85;
        tensor[5 * ANCHORS + 3] = 0.20;
        let observations = decode(ModelId::Fire, &tensor, image, geometry, 0.0).unwrap();
        assert_eq!(
            observations.iter().map(|o| o.label).collect::<Vec<_>>(),
            vec![ObservationLabel::Smoke, ObservationLabel::Fire]
        );
        assert_eq!(observations[0].region.width(), 8.0);
    }
}
