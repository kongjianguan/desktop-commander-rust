"""Static cross-architecture checks; runtime E2E requires an ARM64 Android device."""
import json
import shutil
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
BUNDLE = ROOT / ".work/bundle"
TARGET = "aarch64-linux-android"


def main():
    # The assembler job has no NDK: binutils readelf can inspect any ELF architecture.
    readelf = shutil.which("llvm-readelf") or shutil.which("readelf")
    assert readelf, "readelf (binutils) is required for the final package inspection"
    binary = BUNDLE / "desktop-commander"
    pdfium = BUNDLE / "resources/libpdfium.so"
    rg = BUNDLE / "resources/bin/rg"
    assert binary.is_file() and pdfium.is_file() and rg.is_file()
    evidence = {}
    for name, path in (("agent", binary), ("pdfium", pdfium), ("ripgrep", rg)):
        header = subprocess.check_output([readelf, "-h", path], text=True)
        dynamic = subprocess.check_output([readelf, "-d", path], text=True)
        assert "AArch64" in header, f"{name} is not an ARM64 ELF"
        assert "ELF64" in header, f"{name} is not a 64-bit ELF"
        assert "libc.so.6" not in dynamic, f"{name} unexpectedly links glibc"
        evidence[name] = {"path": str(path.relative_to(ROOT)), "header": header,
                          "dynamic": dynamic}
    assert (BUNDLE / "resources/pdf.typ").is_file()
    assert (BUNDLE / "resources/cmarker/plugin.wasm").is_file()
    assert (BUNDLE / "resources/fonts/NotoSansCJKsc-Regular.otf").is_file()
    result = {"target": TARGET, "passed": True,
              "validation": "ELF architecture and dynamic linkage only; not on-device runtime E2E",
              "binaries": evidence}
    location = ROOT / ".work/evidence/android"
    location.mkdir(parents=True, exist_ok=True)
    (location / "elf-validation.json").write_text(json.dumps(result, indent=2) + "\n")
    print("Android ARM64 ELF and bundled resources verified (static checks; no runtime claim)")


if __name__ == "__main__":
    main()
