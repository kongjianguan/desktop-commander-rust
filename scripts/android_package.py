"""Package the Android/Bionic ELF and resource bundle for direct use in Termux."""
import hashlib
import json
from pathlib import Path
import shutil
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
BUNDLE = ROOT / ".work/bundle"
PACKAGES = ROOT / ".work/packages/android"
TARGET = "aarch64-linux-android"


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    prefix = f"desktop-commander-{version}-android-arm64"
    dist = ROOT / ".work/android-dist" / prefix
    shutil.rmtree(dist, ignore_errors=True)
    dist.mkdir(parents=True)
    shutil.copy2(BUNDLE / "desktop-commander", dist / "desktop-commander")
    shutil.copytree(BUNDLE / "resources", dist / "resources")
    shutil.copy2(ROOT / "LICENSE", dist / "LICENSE")
    shutil.copy2(ROOT / "docs/android-termux.md", dist / "README.md")
    shutil.copy2(ROOT / "scripts/install-termux.sh", dist / "install-termux.sh")
    shutil.copy2(ROOT / "scripts/run-termux.sh", dist / "run-termux.sh")
    for script in ("desktop-commander", "install-termux.sh", "run-termux.sh"):
        (dist / script).chmod(0o755)
    for binary in (dist / "resources/bin").iterdir():
        if binary.is_file():
            binary.chmod(0o755)
    files = {str(path.relative_to(dist)): digest(path)
             for path in sorted(dist.rglob("*")) if path.is_file()}
    manifest = {
        "version": version,
        "target": TARGET,
        "abi": "Android Bionic (NOT GNU/Linux glibc)",
        "headless": True,
        "features": {
            "mcpToolSchemaCount": 26,
            "searchBundled": (dist / "resources/bin/rg").is_file(),
            "typstBundled": (dist / "resources/bin/typst").is_file(),
            "pdfiumBundled": (dist / "resources/libpdfium.so").is_file(),
        },
        "filesSha256": files,
    }
    (dist / "manifest.json").write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n")
    PACKAGES.mkdir(parents=True, exist_ok=True)
    archive = PACKAGES / f"{prefix}.tar.gz"
    with tarfile.open(archive, "w:gz") as output:
        output.add(dist, arcname=dist.name)
    (PACKAGES / f"{prefix}.sha256").write_text(f"{digest(archive)}  {archive.name}\n")
    (PACKAGES / f"{prefix}.json").write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n")
    print(json.dumps({"archive": str(archive), "sha256": digest(archive),
                      "features": manifest["features"]}, indent=2))


if __name__ == "__main__":
    main()
