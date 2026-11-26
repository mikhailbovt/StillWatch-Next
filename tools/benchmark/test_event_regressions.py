"""Regressions for the Fast RC event results carried between stages."""

from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest

from fast_rc import Run
from fast_rc_contract import CONTRACTS, checked_json, evidence, write_json
from validate_report import Validation


class EventRegressionTests(unittest.TestCase):
    def setUp(self):
        self.validator = Validation.__new__(Validation)
        self.validator.run = {"candidate_digest": "sha256:" + "1" * 64,
                              "profile_digest": "sha256:" + "2" * 64, "inputs": {}}
        self.validator.profile = {"models": {"policy_version": "local-policy-5"}}
        self.binding = self.validator.run.copy()
        self.binding.pop("inputs")
        self.binding["policy_version"] = "local-policy-5"
        self.required = sorted(checked_json(CONTRACTS["quality"])["required_regressions"])
        folder = TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.validator.root = self.root = Path(folder.name).resolve()

    def sample_source(self):
        earlier = self.root / "submitted" / "native-trace.json"
        frozen = self.root / "inputs" / "event-source" / earlier.name
        for path in (earlier, frozen):
            path.parent.mkdir(parents=True)
            write_json(path, {"frames": [0, 500_000], "incidents": 1})
        role = "event-source/" + earlier.name
        capture = evidence(self.root, frozen)
        return evidence(earlier.parent, earlier), {"artifacts": {role: capture}, "evidence": [capture]}

    def outcome(self, *cases, proof=None):
        return self.validator.read_event_parity(self.binding | {"cases": list(cases)}, proof or {})

    def test_every_case_must_have_a_measured_match(self):
        source, proof = self.sample_source()
        rows = [{"id": name, "reference": {"incidents": 1, "ids": [name]},
                 "observed": {"ids": [name], "incidents": 1}, "evidence": [source]} for name in self.required]
        _, observed, verdict = self.outcome(*rows, proof=proof)
        self.assertEqual(verdict, "PASS")
        self.assertEqual(observed, {"passed": self.required, "failed": [], "not_run": []})
        rows[-1]["observed"] = {}
        _, observed, verdict = self.outcome(*rows, proof=proof)
        self.assertEqual(verdict, "INCOMPLETE")
        self.assertEqual(observed["not_run"], self.required[-1:])

    def test_observed_mismatch_keeps_failure_with_other_unmeasured_cases(self):
        source, proof = self.sample_source()
        case = {"id": self.required[0], "reference": {"live_delivery": True},
                "observed": {"live_delivery": 1}, "evidence": [source]}
        _, observed, verdict = self.outcome(case, {"id": self.required[1]}, proof=proof)
        self.assertEqual(verdict, "FAIL")
        self.assertEqual(observed["failed"], self.required[:1])
        self.assertEqual(observed["not_run"], self.required[1:])

    def test_duplicate_unknown_and_misbound_cases_are_rejected(self):
        case = {"id": self.required[0], "reference": {"events": 2}, "observed": {"events": 2}}
        with self.assertRaisesRegex(ValueError, "duplicate"):
            self.outcome(case, case)
        with self.assertRaisesRegex(ValueError, "Unexpected"):
            self.outcome(case | {"id": "unplanned"})
        with self.assertRaisesRegex(ValueError, "different configuration"):
            self.validator.read_event_parity(self.binding | {"policy_version": "other", "cases": [case]}, {})

    def test_inline_matches_do_not_replace_source_artifacts(self):
        case = {"id": self.required[0], "reference": {"events": 2}, "observed": {"events": 2}}
        _, observed, verdict = self.outcome(case)
        self.assertEqual(verdict, "INCOMPLETE")
        self.assertEqual(observed["not_run"], self.required)

    def test_proof_uses_the_same_frozen_ledger_in_both_stages(self):
        first, second = self.root / "frozen.json", self.root / "replacement.json"
        data = self.binding | {"cases": []}
        write_json(first, data)
        write_json(second, data)
        pin = evidence(self.root, first)
        self.validator.run["inputs"]["event_parity"] = pin
        self.validator.run["event_parity_digest"] = pin["digest"]
        source = evidence(self.root, second)
        with self.assertRaisesRegex(ValueError, "frozen event parity"):
            self.validator.event_regression_check({"artifacts": {"event-output": source}, "evidence": [source]})
        _, observed, verdict = self.validator.event_regression_check(
            {"artifacts": {"event-output": pin}, "evidence": [pin]})
        self.assertEqual(verdict, "INCOMPLETE")
        self.assertEqual(observed["not_run"], self.required)

    def test_runner_freezes_trace_and_validator_rejects_changes_to_copy(self):
        submitted = self.root / "submitted"
        inputs = self.root / "inputs"
        for target in (submitted, inputs):
            target.mkdir()
        trace = submitted / "trace.json"
        write_json(trace, {"received": 1})
        row = {"id": self.required[0], "reference": {"received": 1}, "observed": {"received": 1},
               "evidence": [evidence(submitted, trace)]}
        data = self.binding | {"cases": [row]}
        write_json(submitted / "parity.json", data)
        run = Run.__new__(Run)
        run.args = SimpleNamespace(event_parity=submitted / "parity.json")
        run.root = self.root
        run.run = self.validator.run
        frozen = run.freeze_event_artifacts()
        proof = {"artifacts": {role: evidence(self.root, path) for role, path in frozen.items()}}
        proof["evidence"] = list(proof["artifacts"].values())
        trace.write_text("{}", encoding="utf-8")
        _, observed, _ = self.validator.event_regression_check(proof)
        self.assertEqual(observed["passed"], self.required[:1])
        (inputs / "event-source" / trace.name).write_text("{}", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "Evidence digest differs"):
            self.outcome(row, proof=proof)
        with self.assertRaisesRegex(ValueError, "Evidence digest differs"):
            run.freeze_event_artifacts()


if __name__ == "__main__":
    unittest.main()
