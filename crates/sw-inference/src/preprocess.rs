use crate::{InferenceError, InputProfile, TensorSize, pack::SIDE};

const PAD: f32 = 114.0 / 255.0;
const COEF: i32 = 1 << 11;

#[derive(Clone, Copy)]
pub struct Image<'a> {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) rgb: &'a [u8],
}

impl<'a> Image<'a> {
    pub fn rgb(width: u32, height: u32, rgb: &'a [u8]) -> Result<Self, InferenceError> {
        if !(1..=3840).contains(&width)
            || !(1..=2160).contains(&height)
            || rgb.len() != width as usize * height as usize * 3
        {
            return Err(InferenceError::Tensor);
        }
        Ok(Self {
            width: width as usize,
            height: height as usize,
            rgb,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Letterbox {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) resized_width: usize,
    pub(crate) resized_height: usize,
    pub(crate) left: usize,
    pub(crate) top: usize,
    tensor: TensorSize,
}

impl Letterbox {
    fn new(width: usize, height: usize, profile: InputProfile) -> Self {
        let gain = (SIDE as f64 / width as f64).min(SIDE as f64 / height as f64);
        let resized_width = ((width as f64 * gain).round_ties_even() as usize).clamp(1, SIDE);
        let resized_height = ((height as f64 * gain).round_ties_even() as usize).clamp(1, SIDE);
        let tensor = if profile == InputProfile::Square {
            TensorSize::SQUARE
        } else {
            TensorSize::new(
                resized_height.next_multiple_of(32),
                resized_width.next_multiple_of(32),
            )
            .expect("stride-rounded letterbox dimensions")
        };
        let [tensor_h, tensor_w] = tensor.dimensions();
        Self {
            width,
            height,
            resized_width,
            resized_height,
            left: (tensor_w - resized_width) / 2,
            top: (tensor_h - resized_height) / 2,
            tensor,
        }
    }

    pub fn tensor_size(self) -> TensorSize {
        self.tensor
    }

    pub(crate) fn x(self, value: f32) -> f64 {
        let gain = (self.resized_width as f64 / self.width as f64) as f32;
        f64::from(((value - self.left as f32) / gain).clamp(0.0, self.width as f32))
    }

    pub(crate) fn y(self, value: f32) -> f64 {
        let gain = (self.resized_height as f64 / self.height as f64) as f32;
        f64::from(((value - self.top as f32) / gain).clamp(0.0, self.height as f32))
    }
}

#[derive(Clone, Copy)]
struct Sample {
    low: usize,
    high: usize,
    weights: [i32; 2],
}

fn samples(source: usize, target: usize, vertical: bool) -> Vec<Sample> {
    let scale = source as f64 / target as f64;
    (0..target)
        .map(|i| {
            let center = ((i as f64 + 0.5) * scale - 0.5) as f32;
            let low = center.floor() as isize;
            let fraction = if vertical || (0..(source as isize - 1)).contains(&low) {
                center - low as f32
            } else {
                0.0
            };
            let coefficient = |value: f32| (value * COEF as f32).round_ties_even() as i32;
            Sample {
                low: low.clamp(0, source as isize - 1) as usize,
                high: (low + 1).clamp(0, source as isize - 1) as usize,
                weights: [coefficient(1.0 - fraction), coefficient(fraction)],
            }
        })
        .collect()
}

pub struct Preprocessor {
    profile: InputProfile,
    input: Vec<f32>,
    geometry: Option<Letterbox>,
    horizontal: Vec<Sample>,
    vertical: Vec<Sample>,
}

impl Preprocessor {
    pub fn new(profile: InputProfile) -> Self {
        Self {
            profile,
            input: Vec::with_capacity(TensorSize::SQUARE.input_values()),
            geometry: None,
            horizontal: Vec::new(),
            vertical: Vec::new(),
        }
    }
    /// Fills a reusable RGB NCHW tensor and returns the exact integer padding.
    pub fn prepare(&mut self, image: Image<'_>) -> (&[f32], Letterbox) {
        let geometry = Letterbox::new(image.width, image.height, self.profile);
        if self.geometry != Some(geometry) {
            self.input.resize(geometry.tensor.input_values(), PAD);
            self.input.fill(PAD);
            self.horizontal = samples(image.width, geometry.resized_width, false);
            self.vertical = samples(image.height, geometry.resized_height, true);
            self.geometry = Some(geometry);
        }
        let [tensor_h, tensor_w] = geometry.tensor.dimensions();
        let plane = tensor_h * tensor_w;
        let area_2x = image.width == geometry.resized_width * 2
            && image.height == geometry.resized_height * 2;
        for (row, vertical) in self.vertical.iter().enumerate() {
            let upper = vertical.low * image.width * 3;
            let lower = vertical.high * image.width * 3;
            let output = (row + geometry.top) * tensor_w + geometry.left;
            for (column, horizontal) in self.horizontal.iter().enumerate() {
                let x0 = horizontal.low * 3;
                let x1 = horizontal.high * 3;
                let [left, right] = horizontal.weights;
                let [top, bottom] = vertical.weights;
                for channel in 0..3 {
                    let rgb = image.rgb;
                    let a = i32::from(rgb[upper + x0 + channel]);
                    let b = i32::from(rgb[upper + x1 + channel]);
                    let c = i32::from(rgb[lower + x0 + channel]);
                    let d = i32::from(rgb[lower + x1 + channel]);
                    let value = if area_2x {
                        (a + b + c + d + 2) >> 2
                    } else {
                        // OpenCV's 8-bit INTER_LINEAR narrows the horizontal sums before blending rows.
                        let h0 = (a * left + b * right) >> 4;
                        let h1 = (c * left + d * right) >> 4;
                        (((top * h0) >> 16) + ((bottom * h1) >> 16) + 2) >> 2
                    };
                    self.input[channel * plane + output + column] = value as f32 / 255.0;
                }
            }
        }
        (&self.input, geometry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_channels_are_not_swapped_and_padding_tracks_geometry_changes() {
        const PLANE: usize = SIDE * SIDE;
        let mut resize = Preprocessor::new(InputProfile::Square);
        let image = Image::rgb(2, 1, &[255, 0, 1, 255, 0, 1]).unwrap();
        let (tensor, geometry) = resize.prepare(image);
        assert_eq!(geometry.top, 160);
        assert_eq!(tensor[0], PAD);
        assert_eq!(tensor[160 * SIDE], 1.0);
        assert_eq!(tensor[PLANE + 160 * SIDE], 0.0);
        assert_eq!(tensor[2 * PLANE + 160 * SIDE], 1.0 / 255.0);
        let (tensor, geometry) = resize.prepare(Image::rgb(1, 2, &[0; 6]).unwrap());
        assert_eq!(geometry.left, 160);
        assert_eq!(tensor[160 * SIDE], PAD);
    }

    #[test]
    fn rectangular_input_uses_only_the_stride_padding() {
        let mut resize = Preprocessor::new(InputProfile::Rectangular);
        let image = Image::rgb(5, 3, &[0; 45]).unwrap();
        let (tensor, geometry) = resize.prepare(image);
        assert_eq!(geometry.tensor_size().dimensions(), [384, 640]);
        assert_eq!(tensor.len(), 3 * 384 * 640);
        assert_eq!(geometry.tensor_size().anchors(), 5040);
    }
}
