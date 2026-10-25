import os
from pathlib import Path
import shutil
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]
WORK = ROOT / ".work/linux-desktop"
WORK.mkdir(parents=True,exist_ok=True)
config = WORK / "config/lxqt"
config.mkdir(parents=True,exist_ok=True)
shutil.copy2(ROOT / "resources/ci-panel.conf",config / "panel.conf")
env = {**os.environ,"XDG_CONFIG_HOME":str(config.parent),"XDG_CURRENT_DESKTOP":"LXQt","QT_QPA_PLATFORM":"xcb"}
with (WORK / "desktop.log").open("w") as log:
    manager = subprocess.Popen(["openbox","--sm-disable"],env=env,stdout=log,stderr=log)
    panel = subprocess.Popen(["lxqt-panel"],env=env,stdout=log,stderr=log)
    try:
        for _ in range(100):
            if panel.poll() is not None:
                raise RuntimeError("LXQt status notifier host exited")
            owner = subprocess.run(["gdbus","call","--session","--dest","org.freedesktop.DBus","--object-path","/org/freedesktop/DBus",
                "--method","org.freedesktop.DBus.NameHasOwner","org.kde.StatusNotifierWatcher"],capture_output=True,text=True,check=True,env=env)
            if "true" in owner.stdout:
                break
            time.sleep(0.1)
        else:
            raise RuntimeError("LXQt status notifier host did not register")
        subprocess.run(sys.argv[1:],check=True,env=env)
    finally:
        panel.terminate()
        manager.terminate()
        panel.wait(timeout=10)
        manager.wait(timeout=10)
