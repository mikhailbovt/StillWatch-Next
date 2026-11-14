pub const SCENE_SIGNATURE_BYTES: usize = 48;

#[derive(Clone, Copy, Debug)]
pub struct SceneSignature(pub [u8; SCENE_SIGNATURE_BYTES]);

impl SceneSignature {
    pub fn matches(self, reference: &[u8]) -> bool {
        if reference.len() != SCENE_SIGNATURE_BYTES || reference[..8] != self.0[..8] {
            return false;
        }
        let exposure = |signature: &[u8]| {
            signature[8..]
                .iter()
                .map(|&pixel| i32::from(pixel))
                .sum::<i32>()
                / 40
        };
        let offset = exposure(&self.0) - exposure(reference);
        reference[8..]
            .iter()
            .zip(&self.0[8..])
            .filter(|(a, b)| (i32::from(**b) - i32::from(**a) - offset).abs() > 35)
            .count()
            < 16
    }
}
