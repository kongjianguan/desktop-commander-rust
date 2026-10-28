"""Exercise the management CLI against a real local headless Rust control server."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
CONFIG = ROOT / ".work/cli-e2e/config"
EXE = Path(os.environ.get("DC_TEST_EXECUTABLE", ROOT / ".work/bundle" / ("desktop-commander.exe" if os.name == "nt" else "desktop-commander")))


def call(*args, success=True):
    result = subprocess.run([str(EXE), "--config-dir", str(CONFIG), *args],
                            capture_output=True, text=True, timeout=20)
    if success and result.returncode:
        raise AssertionError(f"{args}: {result.stdout}\n{result.stderr}")
    if not success and result.returncode == 0:
        raise AssertionError(f"{args}: expected failure")
    return result.stdout.strip()


def main():
    CONFIG.mkdir(parents=True, exist_ok=True)
    for path in CONFIG.iterdir():
        if path.is_file():
            path.unlink()
    assert json.loads(call("status", "--json"))["running"] is False
    call("settings", "set", "autoConnect", "off")
    call("settings", "set", "deviceName", "Termux CLI E2E")
    call("settings", "set", "tools.allowedDirectories", "[]")
    assert call("settings", "get", "deviceName") == "Termux CLI E2E"
    assert json.loads(call("settings", "show", "--json"))["autoConnect"] is False
    call("settings", "set", "autoConnect", "nonsense", success=False)
    call("settings", "set", "unknownSetting", "x", success=False)

    with (CONFIG / "agent-stderr.log").open("w") as log:
        child = subprocess.Popen([str(EXE), "--config-dir", str(CONFIG), "remote", "--headless"], stderr=log)
        try:
            for _ in range(100):
                if child.poll() is not None:
                    raise AssertionError(f"agent exited prematurely: {child.returncode}")
                try:
                    snap = json.loads(call("status", "--json"))
                    if snap.get("pid") == child.pid:
                        break
                except Exception:
                    pass
                time.sleep(.1)
            else:
                raise AssertionError("agent control endpoint not ready")

            assert snap["settings"]["deviceName"] == "Termux CLI E2E"
            assert "already running" in call("start")
            assert json.loads(call("events", "--json")) == []
            call("settings", "set", "deviceName", "Live CLI updated")
            assert call("settings", "get", "deviceName") == "Live CLI updated"
            assert json.loads(call("settings", "show", "--json"))["deviceName"] == "Live CLI updated"
            assert "Diagnostics exported" in call("diagnostics")
            assert (CONFIG / "diagnostics.json").exists()
            call("logout")
            assert not (CONFIG / "session.json").exists()
            call("quit")
            child.wait(timeout=15)
            assert child.returncode == 0
            assert not (CONFIG / "control.json").exists()
            assert json.loads(call("status", "--json"))["running"] is False
            print("PASS: offline/online settings, type validation, status, events, diagnostics, logout, shutdown")
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=10)

    assert "Agent started" in call("start")
    assert json.loads(call("status", "--json"))["running"] is True
    call("quit")
    for _ in range(100):
        if json.loads(call("status", "--json")).get("running") is False:
            break
        time.sleep(.1)
    else:
        raise AssertionError("started agent did not terminate")
    print("PASS: detached CLI start and quit")


if __name__ == "__main__":
    main()
