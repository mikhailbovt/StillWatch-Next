"""Native media checks for the two workloads sharing one frozen source manifest."""

from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

import av

from fast_rc_contract import sha256, write_json
from qualify_steady import read_sources


def video(path, width, intensity):
    with av.open(str(path), "w") as output:
        stream = output.add_stream("mpeg4", rate=15)
        stream.width, stream.height, stream.pix_fmt = width, 48, "yuv420p"
        for _ in range(3):
            frame = av.VideoFrame(width, 48, "yuv420p")
            for plane, color in zip(frame.planes, (intensity, 128, 128), strict=True):
                plane.update(bytes([color]) * plane.buffer_size)
            output.mux(stream.encode(frame))
        output.mux(stream.encode())
    return {"file": path.name, "digest": sha256(path)}


class SourceFixtures(unittest.TestCase):
    def test_canary_replaces_the_media_without_changing_the_declared_stream(self):
        with TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "sources.json"
            steady = video(root / "steady.mp4", 64, 32)
            canary = video(root / "canary.mp4", 64, 128)
            other_size = video(root / "other.mp4", 80, 128)
            source = steady | {"id": "camera-1", "codec": "mpeg4", "width": 64, "height": 48,
                               "fps": 15, "phase": "aligned", "delay_s": 0}
            profile = {"workload": {"camera_count": 1}}

            def load(file, *, use_canary):
                target = source | {"canary_media": file}
                if path.exists():
                    path.unlink()
                write_json(path, {"sources": [target], "viewers": []})
                return read_sources(path, profile, use_canary_media=use_canary)[0]["camera-1"]["media"]

            self.assertEqual(load(canary, use_canary=False), root / "steady.mp4")
            self.assertEqual(load(canary, use_canary=True), root / "canary.mp4")
            self.assertEqual(load(None, use_canary=True), root / "steady.mp4")
            with self.assertRaisesRegex(ValueError, "Declared media parameters differ"):
                load(other_size, use_canary=True)
            with self.assertRaisesRegex(ValueError, "Unverified fixture media"):
                load(canary | {"digest": steady["digest"]}, use_canary=True)
            with self.assertRaisesRegex(ValueError, "must keep the declared stream parameters"):
                load(canary | {"width": 80}, use_canary=True)


if __name__ == "__main__":
    unittest.main()
