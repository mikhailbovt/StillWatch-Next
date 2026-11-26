//! Source-time observations retained until a tentative track is confirmed.
use sw_domain::{Box2d, Keypoint, MedicalReason, MedicalRules, Observation, ObservationLabel};

fn person(at: f64, low: bool, score: f64) -> Observation {
    let points: [(f64, f64); 17] = if low {
        [
            (65., 180.),
            (63., 178.),
            (63., 182.),
            (60., 176.),
            (60., 184.),
            (80., 170.),
            (80., 190.),
            (100., 160.),
            (100., 200.),
            (120., 155.),
            (120., 205.),
            (145., 170.),
            (145., 190.),
            (180., 170.),
            (180., 190.),
            (215., 170.),
            (215., 190.),
        ]
    } else {
        [
            (100., 40.),
            (97., 37.),
            (103., 37.),
            (94., 40.),
            (106., 40.),
            (90., 60.),
            (110., 60.),
            (85., 90.),
            (115., 90.),
            (83., 125.),
            (117., 125.),
            (92., 130.),
            (108., 130.),
            (92., 170.),
            (108., 170.),
            (92., 215.),
            (108., 215.),
        ]
    };
    Observation {
        capture_ts: at,
        region: if low {
            Box2d {
                x1: 50.,
                y1: 150.,
                x2: 230.,
                y2: 210.,
            }
        } else {
            Box2d {
                x1: 80.,
                y1: 20.,
                x2: 120.,
                y2: 220.,
            }
        },
        score,
        label: ObservationLabel::Person,
        track_id: None,
        keypoints: points
            .into_iter()
            .map(|(x, y)| Keypoint(x, y, 0.9))
            .collect(),
        limited: false,
        truncated: false,
        association_uncertain: false,
        torso_histogram: vec![],
    }
}

fn replay(fps: i32, score: f64, visible_frames: i32) -> Vec<sw_domain::MedicalCandidate> {
    let mut rules = MedicalRules::default();
    let mut emitted = vec![];
    // One upright observation, then a visible fall and stationary low posture.
    // Afterwards the person leaves view. All samples are valid and continuous.
    for index in 0..(visible_frames + fps * 3) {
        let at = f64::from(index) / f64::from(fps);
        let observations = if index < visible_frames {
            vec![person(at, index > 0, score)]
        } else {
            vec![]
        };
        emitted.extend(rules.update(observations, at, 1).unwrap());
    }
    emitted
}

#[test]
fn high_confidence_control_preserves_onset() {
    for fps in [3, 4, 5] {
        let candidates = replay(fps, 0.6, fps + 1);
        assert!(
            candidates
                .iter()
                .any(|c| c.reason == MedicalReason::RapidPostureChange
                    && c.motion.as_ref().is_some_and(|m| m.onset_observed))
        );
    }
}

#[test]
fn confirming_same_low_confidence_track_preserves_its_observed_fall() {
    for fps in [3, 4, 5] {
        let candidates = replay(fps, 0.4, fps + 1);
        assert!(
            candidates
                .iter()
                .any(|c| c.reason == MedicalReason::RapidPostureChange
                    && c.motion.as_ref().is_some_and(|m| m.onset_observed)),
            "{fps}fps: three consistent samples confirmed the subject but the earlier descent was lost"
        );
    }
}

#[test]
fn sustained_low_confidence_fall_is_not_relabelled_unknown_origin() {
    for fps in [3, 4, 5] {
        let candidates = replay(fps, 0.4, fps * 4);
        let first = candidates
            .first()
            .expect("persistent low posture produces a candidate");
        assert!(
            first.motion.as_ref().is_some_and(|m| m.onset_observed),
            "{fps}fps: the observed onset was discarded before the track became confirmed"
        );
    }
}

#[test]
fn a_confirmed_opening_keeps_its_onset_without_backdating_publication() {
    for fps in [3.0, 4.0, 5.0] {
        let mut rules = MedicalRules::default();
        for index in 0..2 {
            let at = f64::from(index) / fps;
            assert!(
                rules
                    .update(vec![person(at, index > 0, 0.4)], at, 1)
                    .unwrap()
                    .is_empty()
            );
        }
        let at = 2.0 / fps;
        let candidates = rules.update(vec![person(at, true, 0.4)], at, 1).unwrap();
        let opening = candidates
            .iter()
            .find(|c| c.reason == MedicalReason::RapidPostureChange)
            .unwrap();
        assert_eq!(opening.first_ts, 0.0);
        assert_eq!(opening.last_ts, at);
    }
}

#[test]
fn a_missing_sample_or_new_epoch_prevents_tentative_history_from_crossing_the_boundary() {
    for changed_epoch in [false, true] {
        let mut rules = MedicalRules::default();
        assert!(
            rules
                .update(vec![person(0.0, false, 0.4)], 0.0, 1)
                .unwrap()
                .is_empty()
        );
        let interruption = if changed_epoch {
            vec![person(0.2, true, 0.4)]
        } else {
            vec![]
        };
        let epoch = if changed_epoch { 2 } else { 1 };
        assert!(rules.update(interruption, 0.2, epoch).unwrap().is_empty());
        for at in [0.4, 0.6, 0.8, 1.0] {
            assert!(
                rules
                    .update(vec![person(at, true, 0.4)], at, epoch)
                    .unwrap()
                    .is_empty()
            );
        }
    }
}
