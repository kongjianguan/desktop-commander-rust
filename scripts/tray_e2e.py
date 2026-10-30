import json
import os
from pathlib import Path
import subprocess
import sys
import time
import urllib.request

import psutil

if os.name == "nt":
    import ctypes
    from ctypes import wintypes

    user32 = ctypes.WinDLL("user32", use_last_error=True)
    EnumWindowsProc = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, ctypes.c_ssize_t)
    user32.EnumWindows.argtypes = [EnumWindowsProc, ctypes.c_ssize_t]
    user32.EnumWindows.restype = wintypes.BOOL
    user32.IsWindowVisible.argtypes = [wintypes.HWND]
    user32.IsWindowVisible.restype = wintypes.BOOL
    user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
    user32.GetWindowThreadProcessId.restype = wintypes.DWORD
    user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, ctypes.c_ssize_t]
    user32.PostMessageW.restype = wintypes.BOOL


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
    executable = Path(os.environ.get("DC_TEST_EXECUTABLE", executable))
    report = {"commit":os.environ.get("GITHUB_SHA"),"passed":False,"connected":False,"desktopHost":os.environ.get("XDG_CURRENT_DESKTOP",sys.platform)}

    def visible_window(pid):
        windows = []

        @EnumWindowsProc
        def visit(hwnd, _lparam):
            if user32.IsWindowVisible(hwnd):
                window_pid = wintypes.DWORD()
                user32.GetWindowThreadProcessId(hwnd, ctypes.byref(window_pid))
                if window_pid.value == pid:
                    windows.append(hwnd)
            return True

        if not user32.EnumWindows(visit, 0):
            raise ctypes.WinError(ctypes.get_last_error())
        return windows

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
                        gui = guis[0]
                        if os.name == "nt":
                            windows = visible_window(gui.pid)
                            if windows:
                                time.sleep(1)
                                assert gui.is_running()
                                windows = visible_window(gui.pid)
                                assert windows
                                report.setdefault("visibleGuiWindows", []).append(
                                    {"pid": gui.pid, "hwnd": hex(int(windows[0]))}
                                )
                                return gui, windows[0]
                        else:
                            time.sleep(1)
                            assert gui.is_running()
                            return gui, None
                    time.sleep(0.1)
                raise AssertionError("Flutter process did not create a visible window")

            request("gui")
            first, first_window = find_gui()
            if os.name == "nt":
                if not user32.PostMessageW(first_window, 0x0010, 0, 0):
                    raise ctypes.WinError(ctypes.get_last_error())
            else:
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
            assert max(samples) <= 50*1024**2,report["idleTrayMemory"]
            request("gui")
            second, _ = find_gui()
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
