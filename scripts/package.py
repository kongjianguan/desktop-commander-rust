import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import plistlib
import secrets
import shutil
import subprocess
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[1]


def run(*args):
    subprocess.run([str(arg) for arg in args], check=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("target")
    args = parser.parse_args()
    target = args.target
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    bundle = ROOT / ".work/bundle"
    packages = ROOT / ".work/packages"
    packages.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / ".work/flutter-e2e/result.json",ROOT / ".work/evidence/flutter-e2e.json")
    desktop = bundle / "desktop"
    desktop.mkdir(exist_ok=True)
    signed = False
    if "apple" in target:
        built = next((ROOT / "desktop/build/macos/Build/Products/Release").glob("*.app"))
        shutil.copytree(built, desktop / "Desktop Commander.app", dirs_exist_ok=True, symlinks=True)
        application = ROOT / ".work/macos/Desktop Commander.app"
        executable = application / "Contents/MacOS"
        executable.mkdir(parents=True, exist_ok=True)
        resources = application / "Contents/Resources"
        helpers = application / "Contents/Helpers"
        frameworks = application / "Contents/Frameworks"
        helpers.mkdir(exist_ok=True)
        frameworks.mkdir(exist_ok=True)
        shutil.copy2(bundle / "desktop-commander",executable / "desktop-commander")
        shutil.copytree(bundle / "resources",resources,dirs_exist_ok=True,symlinks=True)
        for item in (resources / "bin").iterdir():
            shutil.move(item,helpers / item.name)
        (resources / "bin").rmdir()
        shutil.move(resources / "libpdfium.dylib",frameworks / "libpdfium.dylib")
        shutil.copytree(desktop / "Desktop Commander.app",helpers / "Desktop Commander Control.app",dirs_exist_ok=True,symlinks=True)
        shutil.copy2(ROOT / "LICENSE",resources / "LICENSE")
        with (application / "Contents/Info.plist").open("wb") as output:
            plistlib.dump({"CFBundleExecutable":"desktop-commander", "CFBundleIdentifier":"app.desktopcommander.rust",
                "CFBundleName":"Desktop Commander", "CFBundleDisplayName":"Desktop Commander",
                "CFBundlePackageType":"APPL", "CFBundleShortVersionString":version,
                "CFBundleVersion":version, "LSUIElement":True, "LSMinimumSystemVersion":"12.0"},output)
        certificate = os.environ.get("MACOS_CERTIFICATE_P12")
        if certificate:
            signing = ROOT / ".work/signing"
            signing.mkdir(exist_ok=True)
            p12 = signing / "identity.p12"
            p12.write_bytes(base64.b64decode(certificate))
            keychain = signing / "build.keychain-db"
            password = secrets.token_urlsafe(24)
            run("security","create-keychain","-p",password,keychain)
            run("security","unlock-keychain","-p",password,keychain)
            run("security","import",p12,"-k",keychain,"-P",os.environ["MACOS_CERTIFICATE_PASSWORD"],"-T","/usr/bin/codesign")
            run("security","set-key-partition-list","-S","apple-tool:,apple:","-s","-k",password,keychain)
            run("codesign","--force","--deep","--options","runtime","--timestamp","--keychain",keychain,"--sign",os.environ["MACOS_SIGNING_IDENTITY"],application)
            run("codesign","--verify","--deep","--strict",application)
            signed = True
        else:
            run("codesign","--force","--deep","--sign","-",application)
        run("codesign","--verify","--deep","--strict",application)
        subprocess.run([sys.executable,str(ROOT / "scripts/e2e.py")],check=True,
            env={**os.environ,"DC_TEST_EXECUTABLE":str(executable / "desktop-commander"),"DC_TEST_VARIANT":"packaged"})
        image = packages / f"desktop-commander-{version}-{target}.dmg"
        run("hdiutil","create","-volname","Desktop Commander","-srcfolder",application.parent,"-ov","-format","UDZO",image)
        if signed and os.environ.get("APPLE_ID"):
            run("xcrun","notarytool","submit",image,"--apple-id",os.environ["APPLE_ID"],"--password",os.environ["APPLE_APP_PASSWORD"],"--team-id",os.environ["APPLE_TEAM_ID"],"--wait")
            run("xcrun","stapler","staple",image)
    elif "windows" in target:
        built = ROOT / "desktop/build/windows/x64/runner/Release"
        shutil.copytree(built,desktop,dirs_exist_ok=True,symlinks=True)
        nsis = Path(os.environ["ProgramFiles(x86)"]) / "NSIS/makensis.exe"
        if not nsis.exists():
            run("choco","install","nsis","-y","--no-progress")
        run(nsis,f"/DVERSION={version}",f"/DBUNDLE={bundle}",f"/DOUTPUT={packages / f'desktop-commander-{version}-windows-x64-setup.exe'}",ROOT / "resources/installer.nsi")
    else:
        built = ROOT / "desktop/build/linux/x64/release/bundle"
        shutil.copytree(built,desktop,dirs_exist_ok=True,symlinks=True)
        deb = ROOT / ".work/deb"
        install = deb / "opt/desktop-commander"
        shutil.copytree(bundle,install,dirs_exist_ok=True,symlinks=True)
        (deb / "DEBIAN").mkdir(parents=True,exist_ok=True)
        (deb / "DEBIAN/control").write_text(f"Package: desktop-commander-rust\nVersion: {version}\nArchitecture: amd64\nMaintainer: Desktop Commander\nDepends: libgtk-3-0\nDescription: Remote desktop tools and Flutter control panel\n")
        launchers = deb / "usr/share/applications"
        launchers.mkdir(parents=True,exist_ok=True)
        (launchers / "desktop-commander.desktop").write_text("[Desktop Entry]\nType=Application\nName=Desktop Commander\nExec=/opt/desktop-commander/desktop-commander remote\nTerminal=false\nCategories=Utility;\n")
        bin_dir = deb / "usr/bin"
        bin_dir.mkdir(parents=True,exist_ok=True)
        (bin_dir / "desktop-commander").symlink_to("/opt/desktop-commander/desktop-commander")
        run("dpkg-deb","--build","--root-owner-group",deb,packages / f"desktop-commander-{version}-linux-x64.deb")
    shutil.copy2(ROOT / "LICENSE",bundle / "LICENSE")
    shutil.copy2(ROOT / "README.md",bundle / "README.md")
    archive_format = "zip" if "windows" in target else "gztar"
    shutil.make_archive(str(packages / f"desktop-commander-{version}-{target}"),archive_format,bundle)
    agent = ROOT / ".work/headless" / target
    agent.mkdir(parents=True,exist_ok=True)
    suffix = ".exe" if "windows" in target else ""
    shutil.copy2(bundle / f"desktop-commander-headless{suffix}",agent / f"desktop-commander{suffix}")
    shutil.copytree(bundle / "resources",agent / "resources",dirs_exist_ok=True,symlinks=True)
    shutil.copy2(ROOT / "LICENSE",agent / "LICENSE")
    shutil.copy2(ROOT / "README.md",agent / "README.md")
    shutil.make_archive(str(packages / f"desktop-commander-headless-{version}-{target}"),archive_format,agent)
    checksums = {file.name:hashlib.sha256(file.read_bytes()).hexdigest() for file in packages.iterdir() if file.is_file()}
    (packages / f"checksums-{target}.json").write_text(json.dumps(checksums,indent=2),encoding="utf-8")
    (packages / f"build-{target}.json").write_text(json.dumps({"version":version,"target":target,"signed":signed,
        "commit":os.environ.get("GITHUB_SHA")},indent=2),encoding="utf-8")


if __name__ == "__main__":
    main()
