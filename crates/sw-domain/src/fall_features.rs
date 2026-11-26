//! Causal input to the temporal fall model from one tracked person in a capture epoch.

use crate::{Box2d, Keypoint};

pub const FALL_FEATURE_CONTRACT: &str = "stillwatch-causal-pose-v2";
pub const FALL_FEATURE_CHANNELS: usize = 128;
pub const FALL_FEATURE_STEPS: usize = 21;
const JOINTS: usize = 17;
const JOINT_CHANNELS: usize = 7;
const GLOBAL_OFFSET: usize = JOINTS * JOINT_CHANNELS;
const GRID_HZ: f64 = 5.0;
const HISTORY_SECONDS: f64 = (FALL_FEATURE_STEPS - 1) as f64 / GRID_HZ;
const MAX_HOLD: f64 = 0.4;
const RESET_GAP: f64 = 0.6;
const MIN_CONFIDENCE: f64 = 0.4;
const PTS_EPSILON: f64 = 1e-9;

#[derive(Clone, Copy, Debug, serde::Deserialize)]
pub struct FallPoseSample {
    pub source_seconds: f64,
    pub source_epoch: u64,
    pub track_id: u64,
    pub region: Box2d,
    pub keypoints: [Keypoint; JOINTS],
    pub association_uncertain: bool,
}

#[derive(Clone, Copy, Debug, serde::Deserialize)]
pub struct FallFeatureAnchor {
    pub source_seconds: f64,
    pub source_epoch: u64,
    pub track_id: u64,
    pub width: u32,
    pub height: u32,
}

pub struct FallFeatureWindow {
    /// Channel-major tensor for a single batch with shape [1, 128, 21].
    pub channels: [[f32; FALL_FEATURE_STEPS]; FALL_FEATURE_CHANNELS],
    pub source_pts: [Option<f64>; FALL_FEATURE_STEPS],
}

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum FallFeatureError {
    #[error("fall features require a finite anchor and nonzero frame dimensions")]
    InvalidAnchor,
    #[error("fall observations are not strictly increasing in source PTS")]
    UnorderedPose,
    #[error("fall observation has an invalid box, confidence or feature magnitude")]
    InvalidPose,
}

/// Build the same backward-only grid as the training extractor. Callers
/// group samples by camera and capture generation. Track and epoch filtering
/// here prevents features that borrow history from another observed person.
pub fn fall_feature_window(
    poses: &[FallPoseSample],
    anchor: FallFeatureAnchor,
) -> Result<FallFeatureWindow, FallFeatureError> {
    if !anchor.source_seconds.is_finite() || anchor.width == 0 || anchor.height == 0 {
        return Err(FallFeatureError::InvalidAnchor);
    }
    let relevant = |pose: &FallPoseSample| {
        pose.source_epoch == anchor.source_epoch
            && pose.track_id == anchor.track_id
            && pose.source_seconds.is_finite()
            && pose.source_seconds <= anchor.source_seconds + PTS_EPSILON
    };
    let oldest = anchor.source_seconds - HISTORY_SECONDS - MAX_HOLD - RESET_GAP - PTS_EPSILON;
    let mut first = 0;
    let mut end = 0;
    let mut previous: Option<&FallPoseSample> = None;
    for (index, pose) in poses.iter().enumerate().filter(|(_, pose)| relevant(pose)) {
        if previous.is_some_and(|prior| pose.source_seconds <= prior.source_seconds) {
            return Err(FallFeatureError::UnorderedPose);
        }
        if pose.source_seconds < oldest
            || pose.association_uncertain
            || previous.is_some_and(|prior| {
                prior.association_uncertain
                    || pose.source_seconds - prior.source_seconds > RESET_GAP + PTS_EPSILON
            })
        {
            first = index;
        }
        previous = Some(pose);
        end = index + 1;
    }

    let mut output = FallFeatureWindow {
        channels: [[0.0; FALL_FEATURE_STEPS]; FALL_FEATURE_CHANNELS],
        source_pts: [None; FALL_FEATURE_STEPS],
    };
    let mut observations = poses[first..end]
        .iter()
        .filter(|pose| relevant(pose))
        .peekable();
    let mut held: Option<&FallPoseSample> = None;
    let mut predecessor = None;
    let mut used_pts = None;
    for step in 0..FALL_FEATURE_STEPS {
        let tick = anchor.source_seconds - (FALL_FEATURE_STEPS - 1 - step) as f64 / GRID_HZ;
        while observations
            .peek()
            .is_some_and(|pose| pose.source_seconds <= tick + PTS_EPSILON)
        {
            predecessor = held;
            let pose = observations
                .next()
                .expect("a current observation was present");
            if !pose.region.valid()
                || pose.region.width() <= 0.0
                || pose.region.height() <= 0.0
                || pose
                    .keypoints
                    .iter()
                    .any(|Keypoint(_, _, c)| c.is_finite() && !(0.0..=1.0).contains(c))
            {
                return Err(FallFeatureError::InvalidPose);
            }
            held = Some(pose);
        }
        let Some(pose) = held.filter(|pose| tick - pose.source_seconds <= MAX_HOLD + PTS_EPSILON)
        else {
            output.channels[GLOBAL_OFFSET + 5][step] = 1.0;
            continue;
        };
        let fresh = used_pts != Some(pose.source_seconds);
        let values = measure_pose(pose, predecessor, anchor, tick, fresh);
        for (channel, value) in output.channels.iter_mut().zip(values) {
            let value = value as f32;
            if !value.is_finite() {
                return Err(FallFeatureError::InvalidPose);
            }
            channel[step] = value;
        }
        used_pts = Some(pose.source_seconds);
        output.source_pts[step] = used_pts;
    }
    Ok(output)
}

fn usable(Keypoint(x, y, confidence): Keypoint) -> bool {
    x.is_finite() && y.is_finite() && confidence.is_finite() && confidence >= MIN_CONFIDENCE
}

fn measure_pose(
    pose: &FallPoseSample,
    previous: Option<&FallPoseSample>,
    anchor: FallFeatureAnchor,
    tick: f64,
    fresh: bool,
) -> [f64; FALL_FEATURE_CHANNELS] {
    let (cx, cy) = pose.region.center();
    let [left, right] = [pose.keypoints[11], pose.keypoints[12]];
    let (ox, oy) = if usable(left) && usable(right) {
        ((left.0 + right.0) * 0.5, (left.1 + right.1) * 0.5)
    } else {
        (cx, cy)
    };
    let height = pose.region.height();
    let scale = height.max(32.0);
    let mut features = [0.0; FALL_FEATURE_CHANNELS];
    let velocity = previous.map(|prior| {
        let elapsed = pose.source_seconds - prior.source_seconds;
        (prior, prior.region.height().max(32.0) * elapsed)
    });
    let mut valid_count = 0;
    for (index, (joint, point)) in features[..GLOBAL_OFFSET]
        .chunks_exact_mut(JOINT_CHANNELS)
        .zip(pose.keypoints)
        .enumerate()
    {
        if usable(point) {
            valid_count += 1;
            joint[0] = (point.0 - ox) / scale;
            joint[1] = (point.1 - oy) / scale;
            joint[2] = point.2;
            joint[3] = 1.0;
            joint[6] = f64::from(fresh);
            if let Some((prior, denominator)) = velocity {
                let old = prior.keypoints[index];
                if usable(old) {
                    joint[4] = (point.0 - old.0) / denominator;
                    joint[5] = (point.1 - old.1) / denominator;
                }
            }
        }
    }
    features[GLOBAL_OFFSET..].copy_from_slice(&[
        cx / f64::from(anchor.width),
        cy / f64::from(anchor.height),
        (height / f64::from(anchor.height)).ln(),
        (pose.region.width() / height).ln(),
        f64::from(valid_count) / JOINTS as f64,
        (tick - pose.source_seconds).max(0.0) / MAX_HOLD,
        f64::from(pose.association_uncertain),
        1.0,
        f64::from(fresh),
    ]);
    features
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(time: f64, shift: f64) -> FallPoseSample {
        FallPoseSample {
            source_seconds: time,
            source_epoch: 3,
            track_id: 7,
            region: Box2d {
                x1: 10.0,
                y1: 20.0,
                x2: 50.0,
                y2: 120.0,
            },
            keypoints: [Keypoint(30.0 + shift, 70.0, 0.9); JOINTS],
            association_uncertain: false,
        }
    }

    fn anchor(time: f64) -> FallFeatureAnchor {
        FallFeatureAnchor {
            source_seconds: time,
            source_epoch: 3,
            track_id: 7,
            width: 320,
            height: 240,
        }
    }

    #[test]
    fn held_samples_keep_actual_dt_velocity_and_mask_missing_joints() {
        let start = sample(0.0, 0.0);
        let mut second = sample(0.3, 9.0);
        second.keypoints[1].2 = 0.39;
        let future = sample(0.61, 50.0);
        let window = fall_feature_window(&[start, second, future], anchor(0.6)).unwrap();
        assert_eq!(
            &window.source_pts[17..],
            &[Some(0.0), Some(0.0), Some(0.3), Some(0.3)]
        );
        assert_eq!(&window.channels[6][17..], &[1.0, 0.0, 1.0, 0.0]);
        assert_eq!(&window.channels[4][19..], &[0.3, 0.3]);
        assert_eq!(window.channels[JOINT_CHANNELS + 3][20], 0.0);
        assert_eq!(window.channels[GLOBAL_OFFSET + 5][0], 1.0);
    }

    #[test]
    fn uncertainty_and_lost_coverage_prevent_stitching_histories() {
        let start = sample(0.0, 0.0);
        let mut ambiguous = sample(0.2, 5.0);
        ambiguous.association_uncertain = true;
        let recovered = sample(0.4, 10.0);
        let mut other_epoch = sample(0.5, 75.0);
        other_epoch.source_epoch += 1;
        let stale =
            fall_feature_window(&[start, ambiguous, recovered, other_epoch], anchor(1.0)).unwrap();
        assert_eq!(stale.channels[GLOBAL_OFFSET + 7][20], 0.0);
        let window =
            fall_feature_window(&[start, ambiguous, recovered, other_epoch], anchor(0.6)).unwrap();
        assert!(window.source_pts[..19].iter().all(Option::is_none));
        assert_eq!(&window.source_pts[19..], &[Some(0.4), Some(0.4)]);
        assert_eq!(window.channels[4][19], 0.0);
        let after_gap = fall_feature_window(&[start, sample(0.601, 10.0)], anchor(0.601)).unwrap();
        assert_eq!(after_gap.source_pts[19], None);
        assert_eq!(after_gap.channels[4][20], 0.0);
    }

    #[test]
    fn invalid_order_and_geometry_fail_closed() {
        let current = sample(0.2, 0.0);
        assert!(matches!(
            fall_feature_window(&[current, current], anchor(0.2)),
            Err(FallFeatureError::UnorderedPose)
        ));
        let mut broken = current;
        broken.region.x2 = broken.region.x1;
        assert!(matches!(
            fall_feature_window(&[broken], anchor(0.2)),
            Err(FallFeatureError::InvalidPose)
        ));
        assert!(fall_feature_window(&[broken], anchor(0.0)).is_ok());
    }
}
