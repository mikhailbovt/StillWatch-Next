"""Check that a quiet interval carries independent elapsed-time evidence."""

import unittest

from fast_rc_contract import native_clock
from validate_steady import Steady, measured_wall_counts


class SteadyClockTests(unittest.TestCase):
    def test_validator_requires_the_host_clock_at_both_ends(self):
        steady = Steady.__new__(Steady)
        steady.clock = native_clock()[0]
        steady.limits = {"minimum_seconds": 3600}
        steady.start = {"cameras": ["camera-1"]}
        steady.end = {"elapsed_s": 3600.01}
        steady.rows = [{"elapsed_s": second} for second in range(3601)]
        steady.cameras = ["camera-1"]
        steady.observations = {"isolated": True, "exit_code": 0}
        self.assertEqual(steady.interval()[2], "INCOMPLETE")
        steady.start["elapsed_clock"] = steady.clock
        steady.end["elapsed_clock"] = "UTC"
        self.assertEqual(steady.interval()[2], "INCOMPLETE")
        steady.end["elapsed_clock"] = steady.clock
        self.assertEqual(steady.interval()[2], "PASS")

    def test_elapsed_rate_would_catch_a_slow_native_window(self):
        before = {"epoch": "1", "elapsed_clock_s": 1000.0, "analysis": {"pose_frames": "0", "fire_frames": "50"}}
        after = {"epoch": "1", "elapsed_clock_s": 1050.0, "analysis": {"pose_frames": "250", "fire_frames": "150"}}
        self.assertEqual(measured_wall_counts(before, after, 60), (300, 120))
        after["elapsed_clock_s"] = 1065.0
        pose, fire = measured_wall_counts(before, after, 60)
        self.assertLess(pose, 285)
        self.assertLess(fire, 114)
        after["epoch"] = "2"
        self.assertIsNone(measured_wall_counts(before, after, 60))


if __name__ == "__main__":
    unittest.main()
