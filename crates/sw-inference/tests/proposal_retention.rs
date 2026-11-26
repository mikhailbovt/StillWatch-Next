//! Weak proposals retained by the production decoder.

use sw_domain::ObservationLabel;
use sw_inference::{
    Image, InputProfile, ModelId, Preprocessor, decode, decode_fire_with_proposals,
};

fn anchor(tensor: &mut [f32], anchors: usize, index: usize, row: [f32; 6]) {
    for (column, value) in row.into_iter().enumerate() {
        tensor[column * anchors + index] = value;
    }
}

#[test]
fn distinct_weak_flame_survives_anchors_already_covered_by_a_strong_detection() {
    let pixels = vec![0; 640 * 640 * 3];
    let image = Image::rgb(640, 640, &pixels).unwrap();
    let (_, geometry) = Preprocessor::new(InputProfile::Square).prepare(image);
    let anchors = geometry.tensor_size().anchors();
    let mut tensor = vec![0.0; anchors * 6];
    anchor(
        &mut tensor,
        anchors,
        0,
        [100.0, 100.0, 40.0, 40.0, 0.9, 0.0],
    );
    for index in 1..=8 {
        anchor(
            &mut tensor,
            anchors,
            index,
            [100.0, 100.0, 40.0, 40.0, 0.19, 0.0],
        );
    }
    anchor(
        &mut tensor,
        anchors,
        9,
        [400.0, 400.0, 40.0, 40.0, 0.0, 0.18],
    );
    let (detected, proposals) = decode_fire_with_proposals(&tensor, image, geometry, 1.0).unwrap();
    assert_eq!(detected.len(), 1);
    assert_eq!(detected[0].label, ObservationLabel::Smoke);
    assert_eq!(
        detected,
        decode(ModelId::Fire, &tensor, image, geometry, 1.0).unwrap()
    );
    assert_eq!(proposals.len(), 1);
    assert!(
        proposals.iter().any(|p| p.label == ObservationLabel::Fire),
        "A valid .18 flame was excluded by eight redundant .19 smoke anchors: {} proposals",
        proposals.len()
    );
}

#[test]
fn distinct_weak_flame_survives_duplicate_weak_smoke_anchors() {
    let pixels = vec![0; 640 * 640 * 3];
    let image = Image::rgb(640, 640, &pixels).unwrap();
    let (_, geometry) = Preprocessor::new(InputProfile::Square).prepare(image);
    let anchors = geometry.tensor_size().anchors();
    let mut tensor = vec![0.0; anchors * 6];
    for index in 0..anchors - 1 {
        anchor(
            &mut tensor,
            anchors,
            index,
            [100.0, 100.0, 40.0, 40.0, 0.19, 0.0],
        );
    }
    anchor(
        &mut tensor,
        anchors,
        anchors - 1,
        [400.0, 400.0, 40.0, 40.0, 0.0, 0.18],
    );
    let (detected, proposals) = decode_fire_with_proposals(&tensor, image, geometry, 1.0).unwrap();
    assert!(detected.is_empty());
    assert_eq!(proposals.len(), 2);
    assert!(
        proposals.iter().any(|p| p.label == ObservationLabel::Fire),
        "The proposal budget was consumed by duplicate anchors of one region"
    );
}

#[test]
fn anchors_in_letterbox_padding_do_not_consume_valid_proposal_budget() {
    let pixels = vec![0; 640 * 320 * 3];
    let image = Image::rgb(640, 320, &pixels).unwrap();
    let (_, geometry) = Preprocessor::new(InputProfile::Square).prepare(image);
    let anchors = geometry.tensor_size().anchors();
    let mut tensor = vec![0.0; anchors * 6];
    for index in 0..8 {
        // y=[20,40] lies wholly in the top 160-pixel letterbox padding.
        anchor(
            &mut tensor,
            anchors,
            index,
            [100.0, 30.0, 40.0, 20.0, 0.19, 0.0],
        );
    }
    anchor(
        &mut tensor,
        anchors,
        8,
        [400.0, 300.0, 40.0, 40.0, 0.0, 0.18],
    );
    let (detected, proposals) = decode_fire_with_proposals(&tensor, image, geometry, 1.0).unwrap();
    assert!(detected.is_empty());
    assert!(
        proposals.iter().any(|p| p.label == ObservationLabel::Fire),
        "Padding anchors were removed only after they excluded the real flame"
    );
}
