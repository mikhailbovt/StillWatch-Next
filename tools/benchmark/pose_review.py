"""Export paired native Pose receipts for human identity review before training."""

import hashlib
import json


TRAINING_PROFILES = {"cpu_fp32_rect_640": "rect640", "cpu_fp32_square_640": "square640"}


def export_pose_review(trace, trace_record, frames, origins, clip, output):
    """Write the raw frame format consumed by the Training Pack identity binder."""
    with trace.open(encoding="utf-8") as stream:
        header = json.loads(stream.readline())
    model = header["pose_balanced"]
    profile = TRAINING_PROFILES.get(model["profile"])
    if profile is None:
        raise ValueError(f"Unrecognized PoseBalanced training profile {model['profile']}")
    if not frames:
        return {"status": "unavailable", "reason": "no_accepted_pose"}

    first, _ = frames[0]
    scope_fields = ("camera_id", "epoch", "scene_revision", "generation")
    scope = tuple(first[key] for key in scope_fields)
    source = origins.get(str(first["epoch"]))
    if source is None or source[1] is None:
        raise ValueError(f"Missing native PTS origin or frame size for {clip['file']}")
    origin_us, dimensions = source
    records = []
    previous_ordinal = 0
    previous_pts = -1
    for task, pose in frames:
        if tuple(task[key] for key in scope_fields) != scope:
            raise ValueError(f"Identity review requires a single capture scope for {clip['file']}")
        ordinal, pts = task["ordinal"], task["source_us"] - origin_us
        if (ordinal <= previous_ordinal or pts <= previous_pts
                or (pose["width"], pose["height"]) != dimensions):
            raise ValueError(f"Inconsistent native pose timing or frame geometry for {clip['file']}")
        previous_ordinal, previous_pts = ordinal, pts
        detections = []
        seen = set()
        for person in pose["confirmed"]:
            track = person["track_id"]
            if track in seen or len(person["keypoints"]) != 17:
                raise ValueError("Duplicate track or invalid COCO-17 observation")
            seen.add(track)
            detections.append({
                "detection_id": track,
                "region": [person["box"][edge] for edge in ("x1", "y1", "x2", "y2")],
                "keypoints": person["keypoints"],
                "association_uncertain": person["association_uncertain"],
                "limited": person["limited"],
            })
        records.append({
            "frame_index_zero_based": ordinal - 1,
            "source_epoch": task["epoch"],
            "source_seconds": pts / 1_000_000,
            "source_us": task["source_us"],
            "slot": task["slot"],
            "long_gap_reset": pose["long_gap_reset"],
            "detections": detections,
        })

    payload = "".join(json.dumps(row, allow_nan=False) + "\n" for row in records).encode("utf-8")
    receipt = {
        "schema": "stillwatch-native-pose-review-v1",
        "video_sha256": clip["sha256"],
        "observations_sha256": hashlib.sha256(payload).hexdigest(),
        "analysis_trace": trace_record,
        "pose_model_sha256": model["sha256"],
        "pose_profile": profile,
        "model_pack_profile": model["profile"],
        "model_manifest_sha256": header["model_manifest_sha256"],
        "provider": model["provider"],
        "runtime_sha256": model["runtime_sha256"],
        "cuda": model["cuda"],
        "scope": dict(zip(scope_fields, scope, strict=True)),
        "source_origin_us": origin_us,
        "width": dimensions[0],
        "height": dimensions[1],
        "frames_analyzed": len(records),
        "detections": sum(len(row["detections"]) for row in records),
        "core_tracking_executed": True,
        "physical_identity_reviewed": False,
    }
    sidecar = output.with_suffix(".receipt.json")
    with output.open("xb") as stream:
        stream.write(payload)
    with sidecar.open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(receipt, stream, indent=2, allow_nan=False)
        stream.write("\n")
    return {"status": "awaiting_identity_review", "file": output.name, "receipt": sidecar.name,
            "sha256": receipt["observations_sha256"]}
