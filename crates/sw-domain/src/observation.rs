use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct Box2d {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

impl Box2d {
    pub fn valid(self) -> bool {
        [self.x1, self.y1, self.x2, self.y2]
            .into_iter()
            .all(f64::is_finite)
            && self.x2 >= self.x1
            && self.y2 >= self.y1
    }

    pub fn width(self) -> f64 {
        (self.x2 - self.x1).max(0.0)
    }
    pub fn height(self) -> f64 {
        (self.y2 - self.y1).max(0.0)
    }
    pub fn area(self) -> f64 {
        self.width() * self.height()
    }
    pub fn center(self) -> (f64, f64) {
        ((self.x1 + self.x2) * 0.5, (self.y1 + self.y2) * 0.5)
    }

    pub fn intersection(self, other: Self) -> f64 {
        (self.x2.min(other.x2) - self.x1.max(other.x1)).max(0.0)
            * (self.y2.min(other.y2) - self.y1.max(other.y1)).max(0.0)
    }

    pub fn iou(self, other: Self) -> f64 {
        let intersection = self.intersection(other);
        intersection / (self.area() + other.area() - intersection).max(1.0)
    }

    pub fn contained_overlap(self, other: Self) -> f64 {
        self.intersection(other) / self.area().min(other.area()).max(1.0)
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
            x2: self.x2.max(other.x2),
            y2: self.y2.max(other.y2),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationLabel {
    Person,
    Smoke,
    Fire,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Keypoint(pub f64, pub f64, pub f64);

impl Keypoint {
    pub fn distance(self, other: Self) -> f64 {
        (self.0 - other.0).hypot(self.1 - other.1)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Observation {
    pub capture_ts: f64,
    #[serde(rename = "box")]
    pub region: Box2d,
    pub score: f64,
    pub label: ObservationLabel,
    #[serde(default)]
    pub track_id: Option<u64>,
    #[serde(default)]
    pub keypoints: Vec<Keypoint>,
    #[serde(default)]
    pub limited: bool,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub association_uncertain: bool,
    #[serde(default)]
    pub torso_histogram: Vec<f64>,
}

impl Observation {
    pub fn valid(&self) -> bool {
        self.capture_ts.is_finite()
            && self.region.valid()
            && (0.0..=1.0).contains(&self.score)
            && self.keypoints.len() <= 17
            && self.keypoints.iter().all(|Keypoint(x, y, confidence)| {
                x.is_finite() && y.is_finite() && (0.0..=1.0).contains(confidence)
            })
            && (self.torso_histogram.is_empty()
                || (self.torso_histogram.len() == 16
                    && self
                        .torso_histogram
                        .iter()
                        .all(|value| (0.0..=1.0).contains(value))
                    && (self.torso_histogram.iter().sum::<f64>() - 1.0).abs() <= 1e-6))
    }
}
