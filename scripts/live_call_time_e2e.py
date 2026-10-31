import argparse
from datetime import datetime
import json
import os
from pathlib import Path
import plistlib
import subprocess
import urllib.request
from zoneinfo import ZoneInfo


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--application", type=Path, required=True)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    descriptor_path = args.config / "control.json"
    descriptor = json.loads(descriptor_path.read_text())
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def status():
        request = urllib.request.Request(descriptor["endpoint"] + "/status",
                                        headers={"Authorization": "Bearer " + descriptor["token"]})
        with opener.open(request, timeout=10) as response:
            return json.load(response)

    initial = status()
    assert initial["status"]["state"] == "online"
    assert initial["events"], "Run a real Remote tool call before starting this E2E"
    event = initial["events"][-1]
    timestamp = event["timestamp"]
    metadata = plistlib.loads((args.application / "Contents/Info.plist").read_bytes())
    executable = args.application / "Contents/MacOS" / metadata["CFBundleExecutable"]
    reports = []
    for zone in ["Asia/Shanghai", "UTC", "America/St_Johns", "Asia/Kathmandu"]:
        local = datetime.fromisoformat(timestamp).astimezone(ZoneInfo(zone))
        minutes = int(local.utcoffset().total_seconds() / 60)
        sign = "-" if minutes < 0 else "+"
        offset = f"UTC{sign}{abs(minutes) // 60:02}:{abs(minutes) % 60:02}"
        expected = f"完成于 {local:%Y-%m-%d %H:%M:%S}.{local.microsecond // 1000:03} {offset}"
        destination = args.output / f"{zone.replace('/', '_')}.json"
        environment = {**os.environ, "TZ": zone, "DC_DESCRIPTOR": str(descriptor_path),
                       "DC_E2E_TIMESTAMP": timestamp, "DC_E2E_EXPECTED_TIME": expected,
                       "DC_E2E_OUTPUT": str(destination)}
        with (args.output / f"{zone.replace('/', '_')}.log").open("w") as log:
            subprocess.run([str(executable)], env=environment, cwd=executable.parent,
                           check=True, timeout=60, stdout=log, stderr=subprocess.STDOUT)
        report = json.loads(destination.read_text())
        assert report["passed"] and report["displayedText"] == expected
        assert report["backendPid"] == initial["pid"]
        assert report["sourceTimestamp"] == timestamp
        current = status()
        assert current["pid"] == initial["pid"] and current["status"]["state"] == "online"
        reports.append(report)
        print(f"PASS: {zone}: {expected}", flush=True)
    (args.output / "result.json").write_text(json.dumps({"passed": True, "cases": reports}, indent=2) + "\n")


if __name__ == "__main__":
    main()
