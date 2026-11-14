import sqlite3
from contextlib import closing
import json
import unittest

from evaluate import opening_event, score_run
from event_scoring import coverage_intervals, score_clip


LIMITS = {"before_onset_tolerance_s": 2, "max_delay_s": 8, "minimum_roi_iou": 0.3,
          "min_verified_negative_fraction": 0.99}


def clip(onset, duration=50):
    return {"file": "building.mp4", "group": "fire_test", "duration_s": duration, "onset_s": onset}


def event(at, id="incident-a", **fields):
    return {"id": id, "category": "fire", "opened_source_s": at, "attention": "review", **fields}


def sample(at, count, *, source="replaying", state="ready", epoch="1"):
    return {"capture_state": source, "analysis_state": state, "source_epoch": epoch,
            "fire_source_s": at, "fire_frames": count, "fire_fps": 2}


def measured(epoch="1", intervals=((0, 50),)):
    return {"source_time": {"fire": {"epoch": epoch, "intervals_s": intervals,
                                    "eligible_seconds": sum(end - start for start, end in intervals)}}}


def annotation(id, start, roi):
    return {"id": id, "onset": {"last_negative_s": start - 1, "first_visible_s": start},
            "category": "fire", "roi": roi, "strata": ["flame"]}


class EventScoring(unittest.TestCase):
    def test_short_follow_up_and_visible_at_start_are_separate(self):
        night = {**clip(34, 39.333333), "last_negative_s": 32}
        course = score_clip(night, {"events": []}, LIMITS)["episodes"][0]
        self.assertEqual(course["status"], "right_censored")
        self.assertEqual(course["window_s"], [30, 42])
        day = clip(0, 26.166667)
        scored = score_clip(day, {"events": [event(13.666666)]}, LIMITS)
        self.assertTrue(scored["episodes"][0]["left_censored"])
        self.assertEqual(scored["episodes"][0]["status"], "miss")
        self.assertEqual(scored["episodes"][0]["reaction_from_clip_start_s"], 13.666666)
        self.assertEqual(scored["late_events"], 1)

    def test_one_result_cannot_credit_an_hour_and_epoch_does_not_bridge(self):
        snapshots = [sample(None, 0, state="starting"), sample(0.2, 1),
                     sample(0.2, 1, state="degraded"), sample(0.0, 1, epoch="2"),
                     sample(0.5, 2, epoch="2"), sample(1.0, 3, source="completed", epoch="2")]
        coverage = coverage_intervals(snapshots, "fire", 3600, 0.8, 2)
        self.assertEqual(coverage["epoch"], "2")
        self.assertEqual(coverage["intervals_s"], [[0.0, 1.0]])
        scored = score_clip(clip(None, 3600), {"events": [], "coverage": {"source_time": {"fire": coverage}}}, LIMITS)
        self.assertEqual(scored["nominal_negative_seconds"], 3600)
        self.assertEqual(scored["verified_negative_seconds"], 1)
        self.assertEqual(scored["unknown_negative_seconds"], 3599)
        self.assertTrue(scored["below_negative_coverage_floor"])

    def test_coverage_gap_cannot_be_reported_as_a_model_miss(self):
        coverage = measured(intervals=((0, 20), (22, 49)))
        scored = score_clip(clip(20), {"events": [], "coverage": coverage}, LIMITS)
        self.assertEqual(scored["episodes"][0]["status"], "invalid_coverage")
        scored = score_clip(clip(20), {"events": [], "coverage": measured()}, LIMITS)
        self.assertEqual(scored["episodes"][0]["status"], "miss")

    def test_grounding_and_one_to_one_matching(self):
        left, right = [0.1, 0.1, 0.4, 0.4], [0.6, 0.6, 0.8, 0.8]
        specs = {**clip(15), "events": [annotation("first", 15, left), annotation("second", 16, left)]}
        opening = event(17, camera_id="run-camera", epoch="1", roi=left)
        wrong_epoch = event(17.5, "old", camera_id="run-camera", epoch="2", roi=left)
        wrong_roi = event(18, "wrong-roi", camera_id="run-camera", epoch="1", roi=right)
        wrong_camera = event(18.5, "wrong-camera", camera_id="other", epoch="1", roi=left)
        run = {"events": [opening, wrong_epoch, wrong_roi, wrong_camera], "camera_id": "run-camera", "coverage": measured()}
        report = score_clip(specs, run, LIMITS)
        self.assertEqual(sum(item["status"] == "timely" for item in report["episodes"]), 1)
        self.assertEqual(report["duplicates_in_window"], 0)
        self.assertEqual(report["unmatched_in_window"], 3)

    def test_missing_grounding_and_old_coverage_are_visible(self):
        corpus = {**clip(10), "events": [annotation("onset", 10, [0.1, 0.1, 0.4, 0.4])]}
        result = score_clip(corpus, {"events": [event(11)], "camera_id": "run-camera", "coverage": measured()}, LIMITS)
        self.assertEqual(result["episodes"][0]["status"], "unverified_grounding")
        summary = score_run([clip(None)], {"building.mp4": {"events": [], "coverage": {"samples": 200}}}, LIMITS)
        no_trace = summary["fire_test"]
        self.assertGreater(no_trace["nominal_negative_camera_hours"], 0)
        self.assertEqual(no_trace["verified_negative_camera_hours"], 0)

    def test_opening_timestamp_is_immutable_after_evidence_update(self):
        first = {"id": "same", "scope": {"camera_id": "cam", "epoch": 1, "scene_revision": 2},
                 "category": "fire", "last_source_us": 22_000_000, "attention": "review", "detection_reason": "flame"}
        updated = dict(first, last_source_us=28_000_000, evidence={"version": 2})
        with closing(sqlite3.connect(":memory:")) as db:
            db.execute("CREATE TABLE change_events(sequence INTEGER PRIMARY KEY, opened, occurred_at, incident)")
            db.executemany("INSERT INTO change_events(opened, occurred_at, incident) VALUES (?, ?, ?)",
                           ((1, 123, json.dumps(first)), (0, 456, json.dumps(updated))))
            row, utc = db.execute("SELECT incident, occurred_at FROM change_events WHERE opened=1 ORDER BY sequence").fetchone()
        result = opening_event(json.loads(row), utc, {"1": 2_000_000})
        self.assertEqual(result["opened_source_s"], 20)
        self.assertEqual(result["opening_recorded_utc_ms"], 123)


if __name__ == "__main__":
    unittest.main()
