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

LONG_DESCRIPTION = """\
# llmrace

**The same request, every provider, one verdict.**

Race LLM providers head-to-head from your terminal: latency, TTFT,
tokens/s, p95, cost and task success — live. Then a CI gate that names
the prompt that got worse.

This wheel carries the compiled `llmrace` binary for your platform. No
Rust toolchain needed.

## Quick start

```bash
pip install llmrace

# no API key needed — two mock lanes, a podium, and whether the gap is noise
llmrace mock mock --no-tui -n 3 --warmup 0 --winner

# the same race on the providers you pay for
llmrace openai anthropic groq gemini --winner

# one workload, every lane, the same prompt on the same round
llmrace openai groq --workload prompts.jsonl --winner
```

## What it does

- **The race.** One streaming request to every provider at the same
  time. TTFT, tokens/s, p95, inter-token gaps, cost per run (cache
  discounts priced), and goodput — a lane that drops half its rounds
  cannot win on the survivors.
- **The answer.** A workload checks each prompt (`exact`, `contains`,
  `regex` or a JSON Schema). A fast wrong answer is a miss. Workloads
  can be multi-turn conversations with per-prompt system prompts and
  custom tool schemas.
- **The gate.** `--baseline` exits 1 when one prompt's success falls or
  its latency rises past the threshold — with a Welch significance test
  so noise does not fail your build. `race.json` names the provider, the
  metric and the prompt id; JUnit and SARIF reports plug the same
  verdict into test reporters and code scanning.
- **The noise check.** Every comparison says whether the gap is real or
  could be noise (Welch's t-test, p < 0.05).
- **Agents.** `llmrace --mcp` is an MCP server: agents race providers,
  gate baselines and inspect the registry as native tool calls.
- **Embeddable.** The `llmrace` Rust crate runs a full multi-lane race
  from your own test suite with scoped per-race pricing.

Single static binary. No telemetry. 18 cloud providers plus local
Ollama, LM Studio, llama.cpp and any OpenAI-compatible endpoint. Also
on Homebrew (`brew install dfbustosus/tap/llmrace`), crates.io, Docker
and as a GitHub Action.

Links: [GitHub](https://github.com/dfbustosus/llmrace) ·
[Changelog](https://github.com/dfbustosus/llmrace/blob/main/CHANGELOG.md) ·
[Issue tracker](https://github.com/dfbustosus/llmrace/issues)
"""


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
    verify_checksum(archive)
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
        "Metadata-Version: 2.4\n"
        "Name: llmrace\n"
        f"Version: {version}\n"
        f"Summary: {SUMMARY}\n"
        "License-Expression: MIT OR Apache-2.0\n"
        "License-File: LICENSE-MIT\n"
        "License-File: LICENSE-APACHE\n"
        "Keywords: llm, benchmark, openai, anthropic, latency, ci, mcp\n"
        "Requires-Python: >=3.9\n"
        "Project-URL: Repository, https://github.com/dfbustosus/llmrace\n"
        "Project-URL: Changelog, https://github.com/dfbustosus/llmrace/blob/main/CHANGELOG.md\n"
        "Project-URL: Issues, https://github.com/dfbustosus/llmrace/issues\n"
        "Classifier: Development Status :: 5 - Production/Stable\n"
        "Classifier: Environment :: Console\n"
        "Classifier: Intended Audience :: Developers\n"
        "Classifier: Operating System :: MacOS\n"
        "Classifier: Operating System :: POSIX :: Linux\n"
        "Classifier: Operating System :: Microsoft :: Windows\n"
        "Classifier: Programming Language :: Rust\n"
        "Classifier: Topic :: Software Development :: Testing\n"
        "Classifier: Topic :: Software Development :: Quality Assurance\n"
        "Description-Content-Type: text/markdown\n"
        "\n"
        f"{LONG_DESCRIPTION}"
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
    for license_name in ("LICENSE-MIT", "LICENSE-APACHE"):
        with open(os.path.join(here, "..", license_name), "rb") as fh:
            files[f"{dist_info}/licenses/{license_name}"] = fh.read()
    record = "".join(record_line(path, data) + "\n" for path, data in files.items())
    record += f"{dist_info}/RECORD,,\n"
    files[f"{dist_info}/RECORD"] = record.encode()

    os.makedirs(outdir, exist_ok=True)
    wheel_path = os.path.join(outdir, f"llmrace-{version}-py3-none-{tag}.whl")
    with zipfile.ZipFile(wheel_path, "w", zipfile.ZIP_DEFLATED) as zf:
        for path, data in files.items():
            info = zipfile.ZipInfo(path)
            info.compress_type = zipfile.ZIP_DEFLATED
            if path.startswith("llmrace/bin/"):
                info.external_attr = (stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR
                                      | stat.S_IRGRP | stat.S_IXGRP
                                      | stat.S_IROTH | stat.S_IXOTH) << 16
            zf.writestr(info, data)
    return wheel_path


def verify_checksum(archive: str) -> None:
    """Verify the release's SHA-256 sidecar before packaging its executable."""
    with open(archive + ".sha256", encoding="ascii") as fh:
        expected = fh.read().split()[0].lower()
    digest = hashlib.sha256()
    with open(archive, "rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            digest.update(chunk)
    if digest.hexdigest() != expected:
        raise SystemExit(f"SHA-256 mismatch for {archive}")


def main() -> None:
    if len(sys.argv) != 4:
        raise SystemExit("usage: make_wheel.py <version> <archive> <outdir>")
    wheel = build(sys.argv[1], sys.argv[2], sys.argv[3])
    print(wheel)


if __name__ == "__main__":
    main()
