import argparse
import json
from pathlib import Path
import subprocess
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("config", type=Path)
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    descriptor = json.loads((args.config / "control.json").read_text())
    args.output.mkdir(parents=True, exist_ok=True)
    for index in range(args.samples):
        output = args.output / f"footprint-{index}.json"
        subprocess.run(["/usr/bin/footprint", "--noCategories", "-p", str(descriptor["pid"]), "-j", str(output)], check=True, stdout=subprocess.DEVNULL)
        time.sleep(1)


if __name__ == "__main__":
    main()
