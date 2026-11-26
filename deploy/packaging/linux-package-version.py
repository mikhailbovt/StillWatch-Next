"""Read the product version from a staged Linux Core manifest."""

import json
from pathlib import Path
import re
import sys


def main():
    if len(sys.argv) != 2:
        raise SystemExit("Usage: linux-package-version.py MANIFEST")
    try:
        data = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
        version = data["version"]
        component = r"(?:0|[1-9][0-9]*)"
        if (data["schema_version"] != 1 or data["target"] != "linux-x64" or not isinstance(version, str)
                or not re.fullmatch(rf"{component}\.{component}\.{component}", version)):
            raise ValueError
    except (KeyError, OSError, TypeError, ValueError) as error:
        raise SystemExit("The payload must declare a three-part Linux x64 product version") from error
    print(version)


if __name__ == "__main__":
    main()
