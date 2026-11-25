#!/usr/bin/env python3
"""Build a platform wheel from an llmrace GitHub release archive.

Usage: make_wheel.py <version> <archive.tar.gz|archive.zip> <outdir>

The wheel contains the compiled binary under llmrace/bin/ plus a console
entry point that execs it. Wheels are zips; dist-info is generated here
so no setuptools platform-tag machinery is involved.
"""

import base64
import hashlib
import io
import os
import stat
import sys
import tarfile
import zipfile

# release archive target triple -> PEP 425 platform tag
PLATFORM_TAGS = {
    "aarch64-apple-darwin": "macosx_11_0_arm64",
    "x86_64-apple-darwin": "macosx_10_9_x86_64",
    "x86_64-unknown-linux-musl": "manylinux_2_17_x86_64.manylinux2014_x86_64",
    "aarch64-unknown-linux-musl": "manylinux_2_17_aarch64.manylinux2014_aarch64",
    "x86_64-unknown-linux-gnu": "manylinux_2_17_x86_64.manylinux2014_x86_64",
    "aarch64-unknown-linux-gnu": "manylinux_2_17_aarch64.manylinux2014_aarch64",
    "x86_64-pc-windows-msvc": "win_amd64",
}

SUMMARY = "Race LLM providers head-to-head: latency, TTFT, tokens/s, cost, and a CI gate that names the prompt."


def read_binary(archive: str) -> tuple[str, bytes]:
    """Return (binary filename, contents) from a release archive."""
    if archive.endswith(".zip"):
        with zipfile.ZipFile(archive) as zf:
            for name in zf.namelist():
                if name.endswith("bin/llmrace.exe"):
                    return "llmrace.exe", zf.read(name)
    else:
        with tarfile.open(archive, "r:gz") as tf:
            for member in tf.getmembers():
                if member.name.endswith("bin/llmrace"):
                    extracted = tf.extractfile(member)
                    if extracted is not None:
                        return "llmrace", extracted.read()
    raise SystemExit(f"no llmrace binary found in {archive}")


def target_of(archive: str) -> str:
    base = os.path.basename(archive)
    for target in PLATFORM_TAGS:
        if target in base:
            return target
    raise SystemExit(f"cannot map archive to a platform tag: {base}")


def record_line(path: str, data: bytes) -> str:
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
    return f"{path},sha256={digest},{len(data)}"


def build(version: str, archive: str, outdir: str) -> str:
    target = target_of(archive)
    tag = PLATFORM_TAGS[target]
    bin_name, binary = read_binary(archive)

    here = os.path.dirname(os.path.abspath(__file__))
    with open(os.path.join(here, "llmrace", "__init__.py"), "rb") as fh:
        init_py = fh.read().replace(b'__version__ = "0.0.0"', f'__version__ = "{version}"'.encode())
    with open(os.path.join(here, "llmrace", "__main__.py"), "rb") as fh:
        main_py = fh.read()

    dist_info = f"llmrace-{version}.dist-info"
    metadata = (
        "Metadata-Version: 2.1\n"
        "Name: llmrace\n"
        f"Version: {version}\n"
        f"Summary: {SUMMARY}\n"
        "License: MIT OR Apache-2.0\n"
        "Requires-Python: >=3.9\n"
        "Project-URL: Repository, https://github.com/dfbustosus/llmrace\n"
        "Project-URL: Changelog, https://github.com/dfbustosus/llmrace/blob/main/CHANGELOG.md\n"
        "Classifier: Environment :: Console\n"
        "Classifier: Operating System :: OS Independent\n"
        "Classifier: Programming Language :: Rust\n"
        "Classifier: Topic :: Software Development :: Testing\n"
        "Description-Content-Type: text/markdown\n"
        "\n"
        "Race the same LLM request across providers — latency, TTFT, tokens/s, cost — "
        "with a live terminal view and a CI gate that names the prompt that got worse. "
        "This wheel bundles the compiled binary; see the "
        "[README](https://github.com/dfbustosus/llmrace) for usage.\n"
    ).encode()
    wheel_meta = (
        "Wheel-Version: 1.0\n"
        "Generator: llmrace-make-wheel\n"
        "Root-Is-Purelib: false\n"
        f"Tag: py3-none-{tag}\n"
    ).encode()
    entry_points = b"[console_scripts]\nllmrace = llmrace.__main__:main\n"

    files = {
        "llmrace/__init__.py": init_py,
        "llmrace/__main__.py": main_py,
        f"llmrace/bin/{bin_name}": binary,
        f"{dist_info}/METADATA": metadata,
        f"{dist_info}/WHEEL": wheel_meta,
        f"{dist_info}/entry_points.txt": entry_points,
    }
    record = "".join(record_line(path, data) + "\n" for path, data in files.items())
    record += f"{dist_info}/RECORD,,\n"
    files[f"{dist_info}/RECORD"] = record.encode()

    os.makedirs(outdir, exist_ok=True)
    wheel_path = os.path.join(outdir, f"llmrace-{version}-py3-none-{tag}.whl")
    with zipfile.ZipFile(wheel_path, "w", zipfile.ZIP_DEFLATED) as zf:
        for path, data in files.items():
            info = zipfile.ZipInfo(path)
            if path.startswith("llmrace/bin/"):
                info.external_attr = (stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR
                                      | stat.S_IRGRP | stat.S_IXGRP
                                      | stat.S_IROTH | stat.S_IXOTH) << 16
            zf.writestr(info, data)
    return wheel_path


def main() -> None:
    if len(sys.argv) != 4:
        raise SystemExit("usage: make_wheel.py <version> <archive> <outdir>")
    wheel = build(sys.argv[1], sys.argv[2], sys.argv[3])
    print(wheel)


if __name__ == "__main__":
    main()
