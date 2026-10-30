"""Exercise the management CLI against a real local headless Rust control server."""
import json
import os
from pathlib import Path
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
CONFIG = ROOT / ".work/cli-e2e/config"
EVIDENCE = ROOT / ".work/evidence/cli-e2e.json"
EXE = Path(os.environ.get("DC_TEST_EXECUTABLE", ROOT / ".work/bundle" / ("desktop-commander.exe" if os.name == "nt" else "desktop-commander")))
steps = []


def record(name, **details):
    entry = {"step": name, **details}
    steps.append(entry)
    EVIDENCE.parent.mkdir(parents=True, exist_ok=True)
    EVIDENCE.write_text(json.dumps({"passed": False, "steps": steps}, indent=2) + "\n")
    print(f"STEP: {name}", flush=True)


def complete(name, **details):
    steps[-1].update(details)
    EVIDENCE.write_text(json.dumps({"passed": False, "steps": steps}, indent=2) + "\n")


def call(*args, success=True):
    result = subprocess.run([str(EXE), "--config-dir", str(CONFIG), *args],
                            capture_output=True, text=True, timeout=20)
    if success and result.returncode:
        raise AssertionError(f"{args}: {result.stdout}\n{result.stderr}")
    if not success and result.returncode == 0:
        raise AssertionError(f"{args}: expected failure")
    return result.stdout.strip()


def status_running():
    return json.loads(call("status", "--json")).get("running") is True


def main():
    CONFIG.mkdir(parents=True, exist_ok=True)
    for path in CONFIG.iterdir():
        if path.is_file():
            path.unlink()

    record("initial stopped status")
    assert json.loads(call("status", "--json"))["running"] is False
    complete("initial stopped status", passed=True)

    record("offline settings read and write")
    call("settings", "set", "autoConnect", "off")
    call("settings", "set", "deviceName", "Termux CLI E2E")
    call("settings", "set", "tools.allowedDirectories", "[]")
    assert call("settings", "get", "deviceName") == "Termux CLI E2E"
    assert json.loads(call("settings", "show", "--json"))["autoConnect"] is False
    call("settings", "set", "autoConnect", "nonsense", success=False)
    call("settings", "set", "unknownSetting", "x", success=False)
    complete("offline settings read and write", passed=True)

    record("direct headless agent launch")
    with (CONFIG / "agent-stderr.log").open("w") as log:
        child = subprocess.Popen([str(EXE), "--config-dir", str(CONFIG), "remote", "--headless"],
                                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=log)
        try:
            snap = None
            for _ in range(100):
                if child.poll() is not None:
                    raise AssertionError(f"agent exited prematurely: {child.returncode}")
                try:
                    candidate = json.loads(call("status", "--json"))
                except subprocess.TimeoutExpired:
                    raise
                except AssertionError:
                    time.sleep(.1)
                    continue
                if candidate.get("pid") == child.pid:
                    snap = candidate
                    break
                time.sleep(.1)
            if snap is None:
                raise AssertionError("agent control endpoint not ready")
            assert snap["settings"]["deviceName"] == "Termux CLI E2E"
            complete("direct headless agent launch", passed=True, pid=child.pid)

            record("already-running detection and events")
            assert "already running" in call("start")
            assert json.loads(call("events", "--json")) == []
            complete("already-running detection and events", passed=True, pid=child.pid)

            record("online settings update")
            call("settings", "set", "deviceName", "Live CLI updated")
            assert call("settings", "get", "deviceName") == "Live CLI updated"
            assert json.loads(call("settings", "show", "--json"))["deviceName"] == "Live CLI updated"
            complete("online settings update", passed=True, pid=child.pid)

            record("diagnostics export")
            assert "Diagnostics exported" in call("diagnostics")
            assert (CONFIG / "diagnostics.json").exists()
            complete("diagnostics export", passed=True, pid=child.pid)

            record("online logout")
            assert "Local device session removed" in call("logout")
            complete("online logout", passed=True, pid=child.pid)

            record("stale descriptor after real agent termination")
            child.kill()
            child.wait(timeout=10)
            assert child.returncode is not None
            stale_descriptor = json.loads((CONFIG / "control.json").read_text())
            assert stale_descriptor["pid"] == child.pid
            assert json.loads(call("status", "--json"))["running"] is False
            assert call("settings", "get", "deviceName") == "Live CLI updated"
            call("settings", "set", "deviceName", "Stale descriptor cleared")
            assert call("settings", "get", "deviceName") == "Stale descriptor cleared"
            assert "Local device session removed" in call("logout")
            assert not (CONFIG / "session.json").exists()
            complete("stale descriptor after real agent termination", passed=True,
                     terminated_pid=child.pid, stale_descriptor_pid=stale_descriptor["pid"])
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=10)

    record("detached agent start")
    assert "Agent started" in call("start")
    complete("detached agent start", passed=True)

    record("detached agent status")
    assert status_running()
    complete("detached agent status", passed=True)

    record("detached agent quit")
    call("quit")
    for _ in range(100):
        if not status_running():
            break
        time.sleep(.1)
    else:
        raise AssertionError("started agent did not terminate")
    complete("detached agent quit", passed=True)

    EVIDENCE.write_text(json.dumps({"passed": True, "steps": steps}, indent=2) + "\n")
    print(f"PASS: CLI E2E evidence saved to {EVIDENCE}", flush=True)


if __name__ == "__main__":
    main()
