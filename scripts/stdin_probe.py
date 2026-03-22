#!/usr/bin/env python3
import sys


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: stdin_probe.py <logfile>", file=sys.stderr)
        return 2

    path = sys.argv[1]
    with open(path, "a", buffering=1) as f:
        f.write("READY\n")
        for line in sys.stdin:
            f.write("LINE:" + line)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
