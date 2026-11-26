from contextlib import closing
import hashlib
import json
import math
from pathlib import Path
import sqlite3
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

from compare_fire_backends import reviewed_regions, sample_slot
from evaluate import QUALITY_CONTRACT, load_manifest, opening_event, opening_rows, rust_batch, score_run
from event_scoring import cluster_uncertainty, coverage_intervals, episodes, score_clip
from fast_rc_contract import DURATION_CLOCKS, verified_quiet_window
from legacy_replay import measured_clip
from validate_report import canary_matches
from validate_steady import Steady


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
    def test_staged_replay_media_has_the_frozen_digest(self):
        with TemporaryDirectory() as directory:
            media = Path(directory)
            path = media / "building.mp4"
            original = b"frozen source bytes"
            path.write_bytes(b"a concurrent replacement")
            video = clip(15) | {"sha256": hashlib.sha256(original).hexdigest()}
            with self.assertRaisesRegex(ValueError, "Staged replay differs from the frozen input building.mp4"):
                rust_batch(None, [video], media, None, None, None, (), configured=False)

    def test_fire_policy_comparison_excludes_a_review_in_the_wrong_region(self):
        annotated = episodes({"events": [annotation("fire", 3, [0.5, 0.5, 0.9, 0.9])]})
        wrong = {"decision": "review", "region": {"x1": 0, "y1": 0, "x2": 70, "y2": 70}}
        target = {"decision": "clear", "region": {"x1": 100, "y1": 100, "x2": 180, "y2": 180}}
        trace = {"regions": [wrong, target]}
        self.assertEqual(reviewed_regions(trace, 200, 200, annotated, 0.3), [])
        target["decision"] = "review"
        self.assertEqual(reviewed_regions(trace, 200, 200, annotated, 0.3), ["fire"])

    def test_smoke_annotation_requires_visible_smoke_at_the_opening(self):
        roi = [0.46, 0.18, 0.72, 0.60]
        smoke = dict(annotation("doorway", 6, roi), visible_target="smoke", strata=["other"])
        source = dict(clip(6), events=[smoke])
        opening = event(8.61, epoch="1", camera_id="corridor", roi=roi, flame_visible=True)
        recording = {"events": [opening], "camera_id": "corridor", "coverage": measured()}

        def status():
            return score_clip(source, recording, LIMITS)["episodes"][0]["status"]

        self.assertEqual(status(), "unverified_grounding")
        opening["smoke_visible"] = False
        self.assertEqual(status(), "miss")
        opening["smoke_visible"] = True
        self.assertEqual(status(), "timely")
        region = {"decision": "review", "region": {"x1": 46, "y1": 18, "x2": 72, "y2": 60},
                  "facts": {"flame_visible": True, "smoke_visible": False}}
        self.assertEqual(reviewed_regions({"regions": [region]}, 100, 100, episodes(source), 0.3), [])
        region["facts"]["smoke_visible"] = True
        self.assertEqual(reviewed_regions({"regions": [region]}, 100, 100, episodes(source), 0.3), ["doorway"])

    def test_stalled_viewer_fails_despite_decoding_at_the_start(self):
        steady = Steady.__new__(Steady)
        steady.short = False
        steady.workload = {"viewer_sessions": 2, "snapshot_requests_per_minute": 8, "evidence_fps": 1}
        steady.budgets = {"viewer_recovery_ms": 15_000}
        steady.sources = {"sources": [], "viewers": []}
        steady.cameras = ["camera"]
        steady.clock = DURATION_CLOCKS["linux"]
        steady.limits = {"minimum_seconds": 30}
        steady.start = {"cameras": {"camera": "id"}, "viewer_frames": {"0": 100, "1": 100}, "elapsed_clock": steady.clock}
        steady.end = {"elapsed_s": 30, "elapsed_clock": steady.clock}
        steady.observations = {"isolated": True, "exit_code": 0}
        steady.rows = [{"tick": tick, "elapsed_s": tick, "viewer_frames": {"0": 100 + tick, "1": 100 + tick},
                        "snapshots": tick * 8 // 60, "snapshots_ok": tick * 8 // 60} for tick in range(31)]
        self.assertEqual(steady.media()[2], "PASS")
        for row in steady.rows[5:]:
            row["viewer_frames"]["1"] = 104
        _, observed, verdict = steady.media()
        self.assertEqual(verdict, "FAIL")
        self.assertEqual(observed["stalled_viewers"], ["1"])

    def test_fixed_replay_phase_changes_slots_at_exact_microsecond_boundaries(self):
        window_start = 2_000_000
        phase_offset = 66_666
        times = [2_000_000, 2_066_665, 2_066_666, 2_566_665, 2_566_666, 3_066_666]
        self.assertEqual([sample_slot(at, window_start, 2, phase_offset) for at in times],
                         [None, None, 0, 0, 1, 2])
        self.assertEqual(sample_slot(25_000_000, window_start, 2, 0), 46)

    def test_legacy_replay_grounds_only_the_first_candidate_and_matching_pose(self):
        observed = {"x1": 10, "y1": 20, "x2": 30, "y2": 40}
        expanded = {"x1": 0, "y1": 0, "x2": 80, "y2": 100}
        person = {"track_id": 4, "association_uncertain": False, "capture_ts": 4.5, "box": observed}
        candidate = {"candidate_id": "opening", "category": "medical", "epoch": 1, "last_ts": 4.5,
                     "reason": "fall_like", "box": expanded, "track_id": 4,
                     "motion": {"identity_uncertain": False, "track_id": 4}}
        event = {"incident_id": "incident", "category": "medical", "camera_id": "camera", "epoch": 1,
                 "detected_ts": 4.5, "reason": "changed", "box": expanded,
                 "candidate_refs": [{"candidate_id": "opening"}]}
        rows = [{"source_ts": ts, "width": 100, "height": 100, "inferred_routes": ["pose", "fire"],
                 "observations": {"pose": []}, "candidates": []} for ts in (4, 4.5, 5)]
        rows[1].update(candidates=[candidate], observations={"pose": [person]})
        result = {"events": [event], "camera_id": "camera", "inference_calls": {"pose": 3, "fire": 3},
                  "source_clock": {"mode": "native_pts", "nominal_frames": 1, "invalid_timestamps": 0,
                                   "regressed_timestamps": 0}}
        coverage = json.loads(QUALITY_CONTRACT.read_text(encoding="utf-8"))["coverage"]
        with TemporaryDirectory() as directory:
            trace = Path(directory) / "observations.jsonl"

            def replay():
                raw = "".join(json.dumps(row) + "\n" for row in rows).encode()
                trace.write_bytes(raw)
                result["trace_sha256"] = hashlib.sha256(raw).hexdigest()
                return measured_clip(result, trace, {"duration_s": 1}, {"pose": 2, "fire": 2}, coverage)

            summary = replay()
            opening, = summary["events"]
            self.assertEqual(opening["opened_clip_us"], "500000")
            self.assertEqual(opening["reason"], "fall_like")
            self.assertEqual(opening["roi"], [0.1, 0.2, 0.3, 0.4])
            self.assertEqual(summary["coverage"]["source_time"]["pose"]["eligible_seconds"], 1.0)
            person["association_uncertain"] = True
            self.assertNotIn("roi", replay()["events"][0])

    def test_negative_rates_keep_gap_openings_outside_the_verified_denominator(self):
        run = {"events": [event(2, "verified", epoch="1"), event(20, "gap", epoch="1"),
                          event(3, "elsewhere", epoch="2")],
               "coverage": measured(intervals=((0, 10), (30, 50)))}
        summary = score_run([clip(None)], {"building.mp4": run}, LIMITS)["fire_test"]
        self.assertEqual(summary["events_in_negative_clips"], {"review": 3})
        self.assertEqual(summary["events_outside_verified_negative_exposure"], {"review": 2})
        rates = summary["verified_negative_opening_rates"]
        exposure = 30 / 3600
        self.assertEqual(rates["review"]["estimated_per_camera_hour"], 1 / exposure)
        expected_count = rates["review"]["poisson_one_sided_upper_per_hour_iid"] * exposure
        self.assertAlmostEqual(math.exp(-expected_count) * (1 + expected_count), 0.05)
        self.assertAlmostEqual(rates["urgent"]["poisson_one_sided_upper_per_hour_iid"], -math.log(0.05) / exposure)

    def test_grounded_negative_exposure_requires_the_capture_camera_and_epoch(self):
        negative = dict(clip(None), category="fire", annotation="negative", events=[])
        openings = [event(3, camera_id="fixture", epoch="1")]
        run = {"events": openings, "coverage": measured()}
        for fields in ({}, {"camera_id": "fixture", "coverage": measured(epoch=None)}):
            with self.subTest(fields=fields):
                scored = score_clip(negative, run | fields, LIMITS)
                self.assertEqual(scored["negative_events"], {"review": 1})
                self.assertEqual(scored["verified_negative_events"], {})
                self.assertEqual(scored["verified_negative_seconds"], 0)
                self.assertEqual(scored["unknown_negative_seconds"], 50)
                self.assertTrue(scored["below_negative_coverage_floor"])

        scored = score_clip(negative, run | {"camera_id": "fixture"}, LIMITS)
        self.assertEqual(scored["verified_negative_events"], {"review": 1})
        self.assertEqual(scored["verified_negative_seconds"], 50)
        self.assertFalse(scored["below_negative_coverage_floor"])

    def test_bootstrap_resamples_linked_recordings_as_one_cluster(self):
        def row(name, scene, session, status, reviews):
            source = {"file": name, "sha256": name,
                      "lineage": {"scene_id": scene, "session_id": session, "camera_id": name,
                                  "original_segments": [{"sha256": name}]}}
            return source, {"episodes": [{"status": status, "left_censored": False}] if status else [],
                            "verified_negative_seconds": 0 if status else 3600, "verified_negative_events": {"review": reviews}}

        rows = [row("a", "one", "first", "timely", 0), row("b", "one", "second", "timely", 0),
                row("c", "two", "second", "timely", 0), row("d", "three", "third", "miss", 0),
                row("n1", "one", "fourth", None, 2), row("n2", "three", "fifth", None, 0)]
        options = {"confidence_level": 0.95, "cluster_bootstrap_seed": 2147483647, "cluster_bootstrap_replicates": 1000}
        clustered = cluster_uncertainty(rows, ("review", "urgent"), options)
        self.assertEqual(clustered, cluster_uncertainty(reversed(rows), ("review", "urgent"), options))
        self.assertEqual(clustered["source_clusters"], 2)
        self.assertEqual(clustered["recall_clusters"], 2)
        self.assertEqual(clustered["timely_recall_percentile_descriptive"], [0.0, 1.0])
        self.assertEqual(clustered["verified_negative_rates_percentile_descriptive"], {"review": [0.0, 2.0], "urgent": [0.0, 0.0]})
        self.assertIsNone(cluster_uncertainty(rows[:3], ("review",), options)["timely_recall_percentile_descriptive"])

    def test_manifest_requires_schema_category_and_group_disjointness(self):
        video = {"file": "first.mp4", "sha256": "1" * 64, "group": "shared", "category": "fire",
                 "duration_s": 20, "cloud_consent": False, "split": "development", "annotation": "positive",
                 "lineage": {"source_id": "test", "scene_id": "A", "camera_id": "A", "session_id": "A",
                             "original_segments": [{"sha256": "1" * 64, "start_s": 0, "duration_s": 20}],
                             "transform": "identity"},
                 "events": [annotation("fire", 3, [0.1, 0.1, 0.2, 0.2])], "negative_strata": []}
        manifest = {"schema_version": 2, "dataset_id": "test",
                    "sources": {"test": {"origin": "local", "rights": "local_evaluation", "license_or_permission": "test"}},
                    "clips": [video]}
        contract = json.loads(QUALITY_CONTRACT.read_text(encoding="utf-8"))
        with TemporaryDirectory() as temporary:
            path = Path(temporary) / "dataset.json"

            def load(development=None):
                path.write_text(json.dumps(manifest), encoding="utf-8")
                return load_manifest(path, "unused-for-v2", contract, development or {})

            self.assertEqual(load()[0], [video])
            video["extra"] = True
            with self.assertRaisesRegex(ValueError, "Invalid dataset"):
                load()
            video.pop("extra")
            video["category"] = "medical"
            with self.assertRaisesRegex(ValueError, "Episode category differs"):
                load()
            video["category"] = "fire"
            lineage = dict(video["lineage"], scene_id="B", camera_id="B", session_id="B",
                           original_segments=[{"sha256": "2" * 64, "start_s": 0, "duration_s": 20}])
            manifest["clips"].append(dict(video, file="second.mp4", sha256="2" * 64, split="holdout", lineage=lineage))
            with self.assertRaisesRegex(ValueError, "Source lineage crosses evaluation splits: .*group"):
                load()
            manifest["clips"].pop()
            video["category"] = "medical"
            video["lineage"]["subject_ids"] = ["subject-A"]
            video["events"] = [dict(video["events"][0], category="medical", subject_id="subject-A")]
            video["subject_frames"] = {"3000000": {"subject-A": [0.1, 0.1, 0.2, 0.2]}}
            self.assertEqual(load()[0], [video])
            video["subject_frames"]["3000000"]["unknown-subject"] = [0.2, 0.2, 0.3, 0.3]
            with self.assertRaisesRegex(ValueError, "Invalid subject frame annotation"):
                load()
            del video["subject_frames"]["3000000"]["unknown-subject"]
            video.update(split="holdout", sha256="3" * 64)
            development = {"corpora": [{"id": "known", "manifest": "manifest-unavailable.json",
                                         "seen_media_sha256": ["1" * 64]}]}
            with self.assertRaisesRegex(ValueError, "Previously evaluated media is reused"):
                load(development)

    def test_holdout_exposure_is_unique_within_each_hazard_category(self):
        def holdout_clip(name, digest, start):
            return {"file": name, "sha256": digest, "group": "room", "split": "holdout",
                    "category": "fire", "annotation": "negative", "duration_s": 20.2, "cloud_consent": False,
                    "negative_strata": ["controlled_flame"], "events": [],
                    "lineage": {"source_id": "source", "scene_id": "room", "camera_id": "camera",
                                "session_id": "session", "transform": "cut",
                                "original_segments": [{"sha256": "1" * 64, "start_s": start, "duration_s": 20.2}]}}

        first = holdout_clip("first.mp4", "2" * 64, 0.1)
        adjacent = holdout_clip("adjacent.mp4", "3" * 64, 20.3)
        manifest = {"schema_version": 2, "dataset_id": "test",
                    "sources": {"source": {"origin": "local", "rights": "local_evaluation",
                                           "license_or_permission": "test"}}}
        contract = json.loads(QUALITY_CONTRACT.read_text(encoding="utf-8"))
        with TemporaryDirectory() as temporary:
            path = Path(temporary) / "dataset.json"

            def load(second):
                path.write_text(json.dumps(manifest | {"clips": [first, second]}), encoding="utf-8")
                return load_manifest(path, "unused-for-v2", contract, {})[0]

            self.assertEqual(load(adjacent), [first, adjacent])
            medical = dict(first, file="medical.mp4", category="medical", negative_strata=["adl"])
            with self.assertRaisesRegex(ValueError, "Evaluation group mixes hazard categories"):
                load(medical)
            medical["group"] = "medical_room"
            self.assertEqual(load(medical), [first, medical])
            with self.assertRaisesRegex(ValueError, "Duplicate holdout clip media"):
                load(dict(adjacent, sha256=first["sha256"]))
            with self.assertRaisesRegex(ValueError, "Overlapping original holdout footage"):
                load(holdout_clip("overlap.mp4", "4" * 64, 20.299999))

    def test_holdout_rejects_development_lineage_with_new_media_digests(self):
        rights = {"origin": "local", "rights": "local_evaluation", "license_or_permission": "test"}
        sources = {"one": rights, "other": rights}
        original = {"file": "past.mp4", "sha256": "1" * 64, "group": "adl-prior", "category": "medical",
                    "duration_s": 12, "cloud_consent": False, "split": "development", "annotation": "negative",
                    "events": [], "negative_strata": ["adl"],
                    "lineage": {"source_id": "one", "scene_id": "room", "camera_id": "front", "session_id": "old",
                                "subject_ids": ["person-a"], "transform": "identity",
                                "original_segments": [{"sha256": "1" * 64, "start_s": 0, "duration_s": 12}]}}
        candidate = dict(original, file="new.mp4", sha256="2" * 64, group="adl-new", split="holdout")
        candidate["lineage"] = dict(original["lineage"], scene_id="elsewhere", camera_id="other-camera", session_id="new",
                                    original_segments=[{"sha256": "3" * 64, "start_s": 0, "duration_s": 12}])
        manifest = {"schema_version": 2, "dataset_id": "new-data", "sources": sources, "clips": [candidate]}
        contract = json.loads(QUALITY_CONTRACT.read_text(encoding="utf-8"))
        with TemporaryDirectory() as temporary:
            root = Path(temporary)
            archived = root / "past.json"
            archived.write_text(json.dumps(manifest | {"clips": [original]}), encoding="utf-8")
            dev = {"id": "past", "manifest": archived.name, "sha256": hashlib.sha256(archived.read_bytes()).hexdigest()}
            indexed = dict(dev, manifest="unavailable.json", seen_lineage=[["one", "subject", "person-a"]])
            path = root / "holdout.json"

            def load(corpus, source):
                updated = candidate | {"lineage": candidate["lineage"] | {"source_id": source}}
                path.write_text(json.dumps(manifest | {"clips": [updated]}), encoding="utf-8")
                with patch("evaluate.DEVELOPMENT_CORPORA", root / "index.json"):
                    return load_manifest(path, "unused-for-v2", contract, {"corpora": [corpus]})[0]

            for corpus in (dev, indexed):
                with self.subTest(corpus=corpus["manifest"]):
                    with self.assertRaisesRegex(ValueError, "Development source lineage is reused.*person-a"):
                        load(corpus, "one")
                    self.assertEqual(load(corpus, "other")[0]["lineage"]["source_id"], "other")

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

    def test_grounded_hit_cannot_qualify_with_a_gap_in_its_window(self):
        roi = [0.1, 0.1, 0.4, 0.4]
        video = {**clip(10), "events": [annotation("onset", 10, roi)]}
        opening = event(11, camera_id="fixture", epoch="1", roi=roi)
        run = {"events": [opening], "camera_id": "fixture"}
        for intervals in (((0, 9), (10, 50)), ((0, 12), (13, 50))):
            with self.subTest(intervals=intervals):
                sampled = run | {"coverage": measured(intervals=intervals)}
                summary = score_run([video], {video["file"]: sampled}, LIMITS)
                result = summary["fire_test"]
                self.assertEqual(result["matched"], 0)
                self.assertEqual(result["invalid_coverage"], ["onset"])
                self.assertEqual(result["incomplete_positive_windows"], ["onset"])
        scored = score_clip(video, run | {"coverage": measured(intervals=((0, 12),))}, LIMITS)
        self.assertEqual(scored["episodes"][0]["matched_event_id"], "incident-a")
        complete = score_clip(video, run | {"coverage": measured()}, LIMITS)
        self.assertEqual(complete["episodes"][0]["status"], "timely")

    def test_grounded_visible_at_start_tracks_only_the_matching_reaction(self):
        roi = [0.1, 0.1, 0.4, 0.4]
        truth = dict(annotation("visible", 0, roi), onset={"last_negative_s": None, "first_visible_s": 0})
        video = {**clip(0), "events": [truth]}
        wrong = event(1, "wrong-region", camera_id="fixture", epoch="1", roi=[0.6, 0.6, 0.8, 0.8])
        match = event(3, camera_id="fixture", epoch="1", roi=roi)
        run = {"events": [wrong, match], "camera_id": "fixture", "coverage": measured()}
        summary = score_run([video], {video["file"]: run}, LIMITS)["fire_test"]
        (visible,) = summary["visible_at_start"]
        self.assertEqual(summary["matched"], 0)
        self.assertEqual(visible["first_recorded_opening_source_s"], 1)
        self.assertEqual(visible["reaction_from_clip_start_s"], 3)
        self.assertEqual(visible["status"], "timely")

    def test_coverage_compares_sample_gaps_at_native_microsecond_precision(self):
        snapshots = [dict(sample(at, count), pose_source_s=at, pose_frames=count, pose_fps=5)
                     for at, count in ((4.0, 20), (4.4, 22), (4.800001, 24))]
        coverage = coverage_intervals(snapshots, "pose", 16, 0.8, 2)
        self.assertEqual(coverage["intervals_s"], [[4.0, 4.4]])
        self.assertEqual(coverage["samples_accepted_in_intervals"], 2)

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
        self.assertEqual(no_trace["negative_clips_below_coverage_floor"], ["building.mp4"])

    def test_opening_timestamp_and_roi_are_immutable_after_evidence_update(self):
        first = {"id": "same", "scope": {"camera_id": "cam", "epoch": 1, "scene_revision": 2},
                 "category": "fire", "last_source_us": 22_000_000, "opening_observed_host_utc": 110,
                 "attention": "review", "detection_reason": "flame"}
        updated = dict(first, last_source_us=28_000_000, evidence={"version": 2})
        immutable = json.dumps([{"type": "fire", "flame_visible": True, "smoke_visible": False},
                                {"x1": 10, "y1": 16, "x2": 50, "y2": 40}])
        later = json.dumps([{"type": "fire", "flame_visible": False, "smoke_visible": True},
                            {"x1": 35, "y1": 24, "x2": 55, "y2": 46}])
        incident = b"\x02" * 16
        sources = {"1": (2_000_000, (100, 80))}
        with closing(sqlite3.connect(":memory:")) as db:
            db.execute("CREATE TABLE change_events(sequence INTEGER PRIMARY KEY, incident_id, opened, occurred_at, incident)")
            db.executemany("INSERT INTO change_events(incident_id, opened, occurred_at, incident) VALUES (?, ?, ?, ?)",
                           ((incident, 1, 123, json.dumps(first)), (incident, 0, 456, json.dumps(updated))))
            self.assertIsNone(opening_rows(db)[0][2])
            db.execute("CREATE TABLE hazard_observations(incident_id, source_us, observed_host_utc, facts)")
            db.executemany("INSERT INTO hazard_observations VALUES (?, ?, ?, ?)",
                           ((incident, 22_000_000, 110, immutable), (incident, 28_000_000, 420, later)))
            row, utc, facts = opening_rows(db)[0]
            db.execute("UPDATE hazard_observations SET observed_host_utc=111 WHERE source_us=22000000")
            self.assertIsNone(opening_rows(db)[0][2])
        result = opening_event(json.loads(row), utc, sources, facts)
        self.assertEqual(result["roi"], [0.1, 0.2, 0.5, 0.5])
        self.assertIs(result["flame_visible"], True)
        self.assertIs(result["smoke_visible"], False)
        self.assertEqual(result["opened_clip_us"], "20000000")
        self.assertEqual(result["opened_source_s"], 20)
        self.assertEqual(result["opening_recorded_utc_ms"], 123)
        self.assertNotIn("roi", opening_event(json.loads(row), utc, {}, facts))

    def test_medical_subject_requires_an_unambiguous_box_at_native_pts(self):
        roi = [0.1, 0.1, 0.45, 0.55]
        landed_roi = [0.1, 0.75, 0.6, 0.95]
        truth = dict(annotation("fall", 10, landed_roi), category="medical", subject_id="subject-A", strata=["fall"])
        clip_data = {**clip(10), "category": "medical", "events": [truth]}
        opening = dict(event(11), category="medical", camera_id="run-camera", epoch="1", roi=roi,
                       track_id="3", identity_uncertain=False, opened_clip_us="11000000")
        coverage = {"source_time": {"pose": measured()["source_time"]["fire"]}}

        def check(subject_frames, status, subject=None, **values):
            run = {"events": [dict(opening, subject_id="subject-A", **values)], "camera_id": "run-camera", "coverage": coverage}
            score_run([dict(clip_data, subject_frames=subject_frames)], {clip_data["file"]: run}, LIMITS)
            self.assertEqual(run["scoring"]["episodes"][0]["status"], status)
            self.assertEqual(run["events"][0].get("subject_id"), subject)

        check({"11000000": {"subject-A": roi, "subject-B": [0.6, 0.1, 0.9, 0.7]}}, "timely", "subject-A")
        check({"11000000": {"subject-B": roi}}, "miss", "subject-B")
        check({"11000000": {"subject-A": roi, "subject-B": roi}}, "unverified_grounding")
        check({"10999999": {"subject-A": roi}}, "unverified_grounding")
        check({"11000000": {"subject-A": roi}}, "unverified_grounding", identity_uncertain=True)

    def test_canary_requires_a_frame_observed_after_the_fault(self):
        plan = {"camera": "cam", "category": "fire", "source_epoch": 1, "earliest_source_us": 0, "latest_source_us": 2_000}
        opening = {"camera": "cam", "category": "fire", "source_epoch": "1", "source_us": 800,
                   "utc_ms": 125, "receipt_utc_ms": 142, "live": True, "evidence_state": "persisted",
                   "acknowledged": False, "opening_delayed": False}
        with self.assertRaisesRegex(ValueError, "opening frame"):
            canary_matches([opening], plan, 100)
        self.assertEqual(canary_matches([dict(opening, opening_observed_host_utc_ms=99)], plan, 100), [])
        self.assertEqual(canary_matches([dict(opening, opening_observed_host_utc_ms=126)], plan, 100), [])
        fresh = dict(opening, opening_observed_host_utc_ms=101)
        self.assertEqual(canary_matches([fresh], plan, 100), [fresh])
        self.assertEqual(canary_matches([dict(fresh, opening_delayed=True)], plan, 100), [])

    def test_viewer_quiet_precondition_uses_continuous_accepted_fire_frames(self):
        snapshots = [{"epoch": "2", "capture_state": "streaming",
                      "analysis": {"state": "ready", "source_epoch": "2", "fire_regions": 0, "smoke_regions": 0,
                                   "fire_frames": str(index + 1), "last_fire_source_us": str(index * 500_000)}}
                     for index in range(7)]
        self.assertTrue(verified_quiet_window(snapshots, 3_000_000))
        self.assertFalse(verified_quiet_window(snapshots[1:], 3_000_000))
        self.assertFalse(verified_quiet_window(snapshots[:3] + snapshots[4:], 3_000_000))
        snapshots[3]["analysis"]["smoke_regions"] = 1
        self.assertFalse(verified_quiet_window(snapshots, 3_000_000))


if __name__ == "__main__":
    unittest.main()
