use std::{
    io::{self, BufReader, BufWriter, Read, Write},
    time::Instant,
};

use anyhow::{Context, Result, bail, ensure};
use sw_domain::Observation;
use sw_inference::{
    Image, InputProfile, Model, ModelId, ModelPack, Preprocessor, Runtime,
    decode_fire_with_proposals,
};
use sw_protocol::worker::{
    CONFIG_MAX_BYTES, ExecutionTiming, FireProposalResult, FrameHeader, HEADER_LEN,
    InferenceConfig, InferenceResult, PtsQuality, TIMED_INFERENCE_READY, Tasks,
};

use crate::inference::{cuda_provider, micros, write_result};

pub fn run(variant: &str) -> Result<()> {
    let high = match variant {
        "960" => true,
        "tiles" => false,
        _ => bail!("expected fire scale variant 960 or tiles"),
    };
    let mut input = BufReader::new(io::stdin().lock());
    let mut output = BufWriter::new(io::stdout().lock());
    let mut length = [0; 4];
    input.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    ensure!((1..=CONFIG_MAX_BYTES).contains(&length));
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    let config: InferenceConfig = serde_json::from_slice(&body)?;
    ensure!(
        config.live_profile.is_none(),
        "diagnostics are outside live admission"
    );
    let pack = ModelPack::open(&config.models, &config.manifest_sha256)?;
    ensure!(
        pack.profile() == InputProfile::Rectangular,
        "requires dynamic Fire graph"
    );
    let runtime = Runtime::load(
        &config.library,
        &config.library_sha256,
        config.threads,
        config.cuda.map(cuda_provider),
    )?;
    let mut model = pack.load(&runtime, ModelId::Fire)?;
    let mut prepare = if high {
        Preprocessor::diagnostic_fire_scale()
    } else {
        Preprocessor::new(InputProfile::Rectangular)
    };
    output.write_all(&TIMED_INFERENCE_READY)?;
    output.flush()?;

    let mut pixels = Vec::new();
    let mut crop = Vec::new();
    loop {
        let mut id = [0; 16];
        if input.read(&mut id[..1])? == 0 {
            return Ok(());
        }
        let read_started = Instant::now();
        input.read_exact(&mut id[1..])?;
        let mut kind = [0];
        input.read_exact(&mut kind)?;
        ensure!(Tasks::from_byte(kind[0])? == Tasks::Fire);
        let mut header = [0; HEADER_LEN];
        input.read_exact(&mut header)?;
        let header = FrameHeader::decode(&header)?;
        ensure!(header.pts_quality == PtsQuality::Native && !header.corrupt);
        let capture_ts = header.source_micros.context("missing source time")? as f64 / 1_000_000.0;
        pixels.resize(header.byte_length()? as usize, 0);
        input.read_exact(&mut pixels)?;
        let started = Instant::now();
        let mut timing = ExecutionTiming {
            input_read_us: micros(started.duration_since(read_started)),
            ..ExecutionTiming::default()
        };
        let (observations, proposals) = if high {
            let image = Image::rgb(header.width, header.height, &pixels)?;
            evaluate(
                &mut model,
                &mut prepare,
                image,
                capture_ts,
                high,
                &mut timing,
            )?
        } else {
            let mut observations = Vec::new();
            let mut proposals = Vec::new();
            for tile in tiles(header.width, header.height) {
                let copy = Instant::now();
                tile.copy_from(&pixels, header.width, &mut crop);
                let image = Image::rgb(tile.width, tile.height, &crop)?;
                timing.preprocess_us += micros(copy.elapsed());
                let (mut detections, mut weak) = evaluate(
                    &mut model,
                    &mut prepare,
                    image,
                    capture_ts,
                    high,
                    &mut timing,
                )?;
                detections
                    .iter_mut()
                    .chain(&mut weak)
                    .for_each(|o| tile.remap(o));
                observations.extend(detections);
                proposals.extend(weak);
            }
            let merge = Instant::now();
            let observations = dedup(observations, &[], 0.70, 300);
            let proposals = dedup(proposals, &observations, 0.45, 8);
            timing.fire_decode_us += micros(merge.elapsed());
            (observations, proposals)
        };
        let response = FireProposalResult {
            inference: InferenceResult {
                request: uuid::Uuid::from_bytes(id),
                elapsed_ms: micros(started.elapsed()) / 1000,
                pose: None,
                fire: Some(observations),
            },
            fire_proposals: proposals,
        };
        let serialize = Instant::now();
        body.clear();
        serde_json::to_writer(&mut body, &response)?;
        timing.serialize_us = micros(serialize.elapsed());
        write_result(&mut output, &body, Some(timing))?;
    }
}

fn evaluate(
    model: &mut Model,
    prepare: &mut Preprocessor,
    image: Image<'_>,
    capture_ts: f64,
    high: bool,
    timing: &mut ExecutionTiming,
) -> Result<(Vec<Observation>, Vec<Observation>)> {
    let preprocess = Instant::now();
    let (tensor, geometry) = prepare.prepare(image);
    let inference = Instant::now();
    timing.preprocess_us += micros(inference.duration_since(preprocess));
    let decode = |values: &[f32]| {
        let started = Instant::now();
        timing.fire_run_us += micros(started.duration_since(inference));
        let result = decode_fire_with_proposals(values, image, geometry, capture_ts);
        timing.fire_decode_us += micros(started.elapsed());
        result
    };
    Ok(if high {
        model.diagnose_fire_scale(geometry.tensor_size(), tensor, decode)??
    } else {
        model.run(geometry.tensor_size(), tensor, decode)??
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tile {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

fn tiles(width: u32, height: u32) -> impl Iterator<Item = Tile> {
    let tile_w = (width * 3).div_ceil(5);
    let tile_h = (height * 3).div_ceil(5);
    let columns = [0, width - tile_w];
    let rows = [0, height - tile_h];
    rows.into_iter()
        .take(1 + usize::from(rows[0] != rows[1]))
        .flat_map(move |y| {
            columns
                .into_iter()
                .take(1 + usize::from(columns[0] != columns[1]))
                .map(move |x| Tile {
                    x,
                    y,
                    width: tile_w,
                    height: tile_h,
                })
        })
}

impl Tile {
    fn copy_from(self, pixels: &[u8], source_width: u32, crop: &mut Vec<u8>) {
        let stride = source_width as usize * 3;
        let x = self.x as usize * 3;
        let length = self.width as usize * 3;
        crop.clear();
        crop.reserve(length * self.height as usize);
        for y in self.y..self.y + self.height {
            let start = y as usize * stride + x;
            crop.extend_from_slice(&pixels[start..start + length]);
        }
    }

    fn remap(self, observation: &mut Observation) {
        let region = &mut observation.region;
        region.x1 += f64::from(self.x);
        region.x2 += f64::from(self.x);
        region.y1 += f64::from(self.y);
        region.y2 += f64::from(self.y);
    }
}

fn dedup(
    mut candidates: Vec<Observation>,
    accepted: &[Observation],
    iou: f64,
    limit: usize,
) -> Vec<Observation> {
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept = Vec::<Observation>::new();
    for candidate in candidates {
        if !kept
            .iter()
            .chain(accepted)
            .any(|other| candidate.label == other.label && candidate.region.iou(other.region) > iou)
        {
            kept.push(candidate);
            if kept.len() == limit {
                break;
            }
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use sw_domain::{Box2d, ObservationLabel};

    #[test]
    fn overlapping_tiles_restore_source_coordinates_and_keep_distinct_regions() {
        let tiles: Vec<_> = tiles(11, 20).collect();
        assert_eq!(
            (tiles[3].x, tiles[3].y, tiles[3].width, tiles[3].height),
            (4, 8, 7, 12)
        );
        let source: Vec<_> = (0..220).flat_map(|pixel| [pixel; 3]).collect();
        let mut crop = Vec::new();
        tiles[3].copy_from(&source, 11, &mut crop);
        assert_eq!((crop[0], crop[crop.len() - 1]), (92, 219));
        let mut observations = Vec::new();
        for (tile, x, y, label, score) in [
            (tiles[0], 4.0, 8.0, ObservationLabel::Fire, 0.6),
            (tiles[3], 0.0, 0.0, ObservationLabel::Fire, 0.8),
            (tiles[3], 0.0, 0.0, ObservationLabel::Smoke, 0.7),
            (tiles[3], 4.0, 0.0, ObservationLabel::Fire, 0.6),
        ] {
            let mut observation = Observation {
                capture_ts: 3.0,
                region: Box2d {
                    x1: x,
                    y1: y,
                    x2: x + 3.0,
                    y2: y + 3.0,
                },
                score,
                label,
                track_id: None,
                keypoints: Vec::new(),
                limited: false,
                truncated: false,
                association_uncertain: false,
                torso_histogram: Vec::new(),
            };
            tile.remap(&mut observation);
            observations.push(observation);
        }
        assert_eq!(observations[0].region, observations[1].region);
        let kept = dedup(observations, &[], 0.70, 300);
        assert_eq!(kept.len(), 3);
        assert_eq!(
            (kept[0].score, kept[1].label, kept[2].region.x1),
            (0.8, ObservationLabel::Smoke, 8.0)
        );
    }
}
