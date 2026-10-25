import json
import os
from pathlib import Path
import subprocess
import sys
import time
import urllib.request

import psutil


ROOT = Path(__file__).resolve().parents[1]
WORK = ROOT / ".work/tray-e2e"
EVIDENCE = ROOT / ".work/evidence"


def main():
    WORK.mkdir(parents=True,exist_ok=True)
    EVIDENCE.mkdir(parents=True,exist_ok=True)
    (WORK / "settings.json").write_text(json.dumps({"autoConnect":False}),encoding="utf-8")
    executable = ROOT / ".work/bundle" / ("desktop-commander.exe" if os.name == "nt" else "desktop-commander")
    if sys.platform == "darwin":
        executable = ROOT / ".work/macos/Desktop Commander.app/Contents/MacOS/desktop-commander"
    report = {"commit":os.environ.get("GITHUB_SHA"),"passed":False,"connected":False}
    with (EVIDENCE / "tray-stderr.log").open("w") as errors:
        child = subprocess.Popen([str(executable),"--config-dir",str(WORK),"remote"],stderr=errors)
        try:
            descriptor = WORK / "control.json"
            for _ in range(200):
                if child.poll() is not None:
                    raise AssertionError(f"Tray process exited: {child.returncode}")
                if descriptor.exists():
                    break
                time.sleep(0.1)
            connection = json.loads(descriptor.read_text())
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

            def request(operation):
                request = urllib.request.Request(connection["endpoint"]+"/"+operation,
                    method="GET" if operation == "status" else "POST",
                    headers={"Authorization":"Bearer "+connection["token"]})
                with opener.open(request,timeout=10) as response:
                    return json.load(response)

            time.sleep(1)
            assert child.poll() is None
            assert request("status")["pid"] == child.pid
            process = psutil.Process(child.pid)

            def find_gui():
                for _ in range(200):
                    guis = [p for p in process.children() if p.name().lower() in ["desktop_commander","desktop_commander.exe"]]
                    if guis:
                        time.sleep(1)
                        assert guis[0].is_running()
                        return guis[0]
                    time.sleep(0.1)
                raise AssertionError("Flutter process did not start")

            request("gui")
            first = find_gui()
            first.terminate()
            first.wait(timeout=15)
            assert request("status")["pid"] == child.pid
            assert not process.children()
            report["backendSurvivesGuiExit"] = True
            samples = []
            for index in range(5):
                if sys.platform == "darwin":
                    raw = WORK / f"footprint-{index}.json"
                    subprocess.run(["/usr/bin/footprint","--noCategories","-p",str(child.pid),"-j",str(raw)],check=True,stdout=subprocess.DEVNULL)
                    sample = json.loads(raw.read_text())["total footprint"]
                    metric = "physical footprint"
                else:
                    info = process.memory_full_info()
                    sample = info.uss if os.name == "nt" else info.pss
                    metric = "USS" if os.name == "nt" else "PSS"
                assert sample > 0
                samples.append(sample)
                time.sleep(0.2)
            report["idleTrayMemory"] = {"metric":metric,"samplesBytes":samples,"maxMiB":max(samples)/1024**2}
            request("gui")
            second = find_gui()
            assert second.pid != first.pid
            request("gui")
            time.sleep(0.5)
            assert len(process.children()) == 1
            report["guiReopensAsSingleProcess"] = True
            request("quit")
            child.wait(timeout=15)
            assert child.returncode == 0
            assert not psutil.pid_exists(second.pid)
            assert not descriptor.exists()
            report["ownedProcessesExitWithBackend"] = True
            report["passed"] = True
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            (EVIDENCE / "tray-e2e.json").write_text(json.dumps(report,indent=2),encoding="utf-8")


if __name__ == "__main__":
    main()
