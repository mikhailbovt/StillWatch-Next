//! Read one native feature reference window per line and emit its tensor and PTS.

use std::io::{BufRead, BufWriter, Write};
use sw_domain::{FALL_FEATURE_CONTRACT, FallFeatureAnchor, FallPoseSample, fall_feature_window};

#[derive(serde::Deserialize)]
struct Request {
    anchor: FallFeatureAnchor,
    observations: Vec<FallPoseSample>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut output = BufWriter::new(std::io::stdout().lock());
    for line in std::io::stdin().lock().lines() {
        let request: Request = serde_json::from_str(&line?)?;
        let window = fall_feature_window(&request.observations, request.anchor)?;
        serde_json::to_writer(
            &mut output,
            &serde_json::json!({
                "contract": FALL_FEATURE_CONTRACT,
                "channels": window.channels.as_slice(),
                "source_pts": window.source_pts,
            }),
        )?;
        writeln!(output)?;
        output.flush()?;
    }
    Ok(())
}
