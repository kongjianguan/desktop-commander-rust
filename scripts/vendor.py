import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import tarfile
import urllib.request
import zipfile


ROOT = Path(__file__).resolve().parents[1]


def fetch(url, target):
    target.parent.mkdir(parents=True, exist_ok=True)
    if not target.exists():
        request = urllib.request.Request(url, headers={"User-Agent": "desktop-commander-build"})
        with urllib.request.urlopen(request, timeout=120) as source, target.open("wb") as output:
            shutil.copyfileobj(source, output)
    return target


def unpack(archive, folder):
    folder.mkdir(parents=True, exist_ok=True)
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as source:
            for member in source.infolist():
                if not (folder / member.filename).resolve().is_relative_to(folder.resolve()):
                    raise ValueError("Archive path escapes destination")
            source.extractall(folder)
    else:
        with tarfile.open(archive) as source:
            source.extractall(folder, filter="data")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("target")
    parser.add_argument("--bundle", default=".work/bundle")
    args = parser.parse_args()
    target = args.target
    windows = "windows" in target
    mac = "apple" in target
    arm = target.startswith("aarch64")
    bundle = ROOT / args.bundle
    resources = bundle / "resources"
    binary = resources / "bin"
    binary.mkdir(parents=True, exist_ok=True)
    cache = ROOT / ".work/vendor" / target
    cache.mkdir(parents=True, exist_ok=True)
    suffix = ".exe" if windows else ""
    typst_target = target if mac or windows else "x86_64-unknown-linux-musl"
    typst_asset = f"typst-{typst_target}.{'zip' if windows else 'tar.xz'}"
    typst = fetch(f"https://github.com/typst/typst/releases/download/v0.13.1/{typst_asset}", cache / typst_asset)
    unpack(typst, cache / "typst")
    shutil.copy2(next((cache / "typst").rglob(f"typst{suffix}")), binary / f"typst{suffix}")
    rg_target = target if mac or windows else "x86_64-unknown-linux-musl"
    rg_asset = f"ripgrep-14.1.1-{rg_target}.{'zip' if windows else 'tar.gz'}"
    rg = fetch(f"https://github.com/BurntSushi/ripgrep/releases/download/14.1.1/{rg_asset}", cache / rg_asset)
    unpack(rg, cache / "ripgrep")
    shutil.copy2(next((cache / "ripgrep").rglob(f"rg{suffix}")), binary / f"rg{suffix}")
    pdf_platform = "win" if windows else "mac" if mac else "linux"
    pdf_asset = f"pdfium-{pdf_platform}-{'arm64' if arm else 'x64'}.tgz"
    pdfium = fetch(f"https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F7350/{pdf_asset}", cache / pdf_asset)
    unpack(pdfium, cache / "pdfium")
    library = "pdfium.dll" if windows else "libpdfium.dylib" if mac else "libpdfium.so"
    shutil.copy2(next((cache / "pdfium").rglob(library)), resources / library)
    cmarker = resources / "cmarker"
    cmarker.mkdir(exist_ok=True)
    for name in ["lib.typ", "plugin.wasm", "LICENSE", "typst.toml"]:
        item = fetch(f"https://raw.githubusercontent.com/typst/packages/02555ae3e8d9219403fe0b45cc710809e22835e1/packages/preview/cmarker/0.1.6/{name}", cache / "cmarker" / name)
        shutil.copy2(item, cmarker / name)
    fonts = resources / "fonts"
    fonts.mkdir(exist_ok=True)
    for name, remote in [("NotoSansCJKsc-Regular.otf", "OTF/SimplifiedChinese/NotoSansCJKsc-Regular.otf"), ("LICENSE", "LICENSE")]:
        item = fetch(f"https://raw.githubusercontent.com/notofonts/noto-cjk/Sans2.004/Sans/{remote}", cache / "fonts" / name)
        shutil.copy2(item, fonts / name)
    shutil.copy2(ROOT / "resources/pdf.typ", resources / "pdf.typ")
    licenses = resources / "licenses"
    licenses.mkdir(exist_ok=True)
    for component in ["typst", "ripgrep", "pdfium"]:
        folder = licenses / component
        folder.mkdir(exist_ok=True)
        for item in (cache / component).rglob("*"):
            if item.is_file() and ("license" in item.name.lower() or "copyright" in item.name.lower()):
                shutil.copy2(item, folder / item.name)
    if not windows:
        for item in binary.iterdir():
            item.chmod(0o755)
    manifest = {str(item.relative_to(bundle)): hashlib.sha256(item.read_bytes()).hexdigest()
                for item in resources.rglob("*") if item.is_file()}
    (bundle / "dependencies.sha256.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    print(json.dumps({"target": target, "resources": len(manifest)}))


if __name__ == "__main__":
    main()
