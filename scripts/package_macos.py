"""将 GitControl.app 制作为可拖拽安装的 macOS 镜像。"""

import argparse
import plistlib
import subprocess
import tempfile
from pathlib import Path


def package(app, output_dir):
    with (app / "Contents/Info.plist").open("rb") as source:
        info = plistlib.load(source)
    if info["CFBundleIdentifier"] != "com.ziggy.gitcontrol":
        raise ValueError("请选择 GitControl.app")
    binary = app / "Contents/MacOS" / info["CFBundleExecutable"]
    arch = subprocess.check_output(["lipo", "-archs", binary], text=True).strip()
    suffix = {"arm64": "aarch64", "x86_64": "x64"}[arch]
    version = info["CFBundleShortVersionString"]
    output_dir.mkdir(parents=True, exist_ok=True)
    output = output_dir / f"GitControl_{version}_{suffix}.dmg"
    guide = Path(__file__).resolve().parents[1] / "docs/install/macOS安装说明.txt"
    with tempfile.TemporaryDirectory(dir=output_dir) as scratch:
        stage = Path(scratch) / "contents"
        stage.mkdir()
        staged_app = stage / "GitControl.app"
        subprocess.run(["ditto", app, staged_app], check=True)
        subprocess.run(["codesign", "--verify", "--deep", "--strict", staged_app], check=True)
        (stage / "Applications").symlink_to("/Applications")
        (stage / "安装前请阅读 - Read Me.txt").write_bytes(guide.read_bytes())
        image = Path(scratch) / output.name
        subprocess.run(["hdiutil", "create", "-volname", "GitControl",
                        "-srcfolder", stage, "-format", "UDZO", image], check=True)
        subprocess.run(["hdiutil", "verify", image], check=True)
        image.replace(output)
    print(output)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    package(args.app, args.output_dir)
