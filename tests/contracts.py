"""Offline consumer contracts: reports, terminal runtime, monitoring and wheels.

Run after cargo build/test with: python tests/contracts.py
Requires jsonschema==4.25.1. Provider credentials are excluded from subprocesses.
"""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import signal
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import unittest
import venv
import zipfile

from jsonschema import Draft202012Validator

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target" / "debug" / ("llmrace.exe" if os.name == "nt" else "llmrace")


class ConsumerContracts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="llmrace-contract-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = {key: value for key, value in os.environ.items()
                    if key.upper() in {"PATH", "SYSTEMROOT", "COMSPEC", "PATHEXT", "TEMP", "TMP"}}
        self.env.update(HOME=str(self.root), USERPROFILE=str(self.root),
                        XDG_DATA_HOME=str(self.root / "data"), NO_PROXY="127.0.0.1,localhost")

    def cli(self, *args, expected=0):
        result = subprocess.run([str(BINARY), *map(str, args)], cwd=self.root, env=self.env,
                                text=True, capture_output=True, timeout=15)
        self.assertEqual(result.returncode, expected, result.stderr + result.stdout)
        return result

    def test_generated_reports_validate_and_schema_has_no_drift(self):
        generated = self.cli("--schema").stdout
        self.assertEqual(generated.rstrip(), (ROOT / "docs" / "schema.json").read_text().rstrip())
        schema = json.loads(generated)
        Draft202012Validator.check_schema(schema)
        validator = Draft202012Validator(schema)
        for name, providers, expected in [("one", ["mock"], 0), ("outage", ["openai"], 1),
                                           ("mixed", ["openai", "mock"], 0)]:
            output = self.root / f"{name}.json"
            self.cli(*providers, "--no-tui", "-n", "1", "--warmup", "0", "--out", output, expected=expected)
            document = json.loads(output.read_text())
            validator.validate(document)
            canonical = json.dumps(document["experiment"], sort_keys=True, separators=(",", ":"), ensure_ascii=False)
            self.assertEqual(document["experiment_sha256"], hashlib.sha256(canonical.encode()).hexdigest())

    def test_strict_gate_agrees_with_json_junit_and_sarif(self):
        baseline = self.root / "baseline.json"
        # Deliberately impossible tail baseline, with other metrics permissive.
        baseline.write_text(json.dumps({"providers": [{"provider": "mock", "model": "mock-1",
            "mean_total_ms": 10000, "p95_total_ms": 1, "mean_ttfb_ms": 10000,
            "mean_tokens_per_sec": 0.001}]}))
        output = self.root / "strict.json"
        self.cli("mock", "--no-tui", "-n", "1", "--warmup", "0", "--baseline", baseline,
                 "--require-significance", "--fail-on-inconclusive", "--junit", "--sarif", "--out", output, expected=1)
        doc = json.loads(output.read_text())
        self.assertFalse(doc["regression"]["ok"])
        self.assertEqual(doc["regression"]["status"], "inconclusive")
        self.assertIn("<failure", output.with_suffix(".xml").read_text())
        sarif = json.loads(output.with_suffix(".sarif").read_text())
        self.assertTrue(any(r["ruleId"] == "llmrace/inconclusive" for r in sarif["runs"][0]["results"]))
        Draft202012Validator(json.loads(self.cli("--schema").stdout)).validate(doc)

    @unittest.skipIf(os.name == "nt", "POSIX pseudo-terminal regression")
    def test_single_worker_tui_can_finish_and_quit(self):
        import pty
        import termios
        master, slave = pty.openpty()
        termios.tcsetwinsize(master, (40, 120))
        child = subprocess.Popen([str(BINARY), "mock=a", "mock=b", "-n", "1", "--warmup", "0"],
                                 cwd=self.root, env={**self.env, "TERM": "xterm-256color", "TOKIO_WORKER_THREADS": "1"},
                                 stdin=slave, stdout=slave, stderr=slave)
        os.close(slave)
        captured = bytearray()

        def drain():
            try:
                while chunk := os.read(master, 4096):
                    captured.extend(chunk)
            except OSError:
                pass

        reader = threading.Thread(target=drain, daemon=True)
        reader.start()
        try:
            time.sleep(1)
            os.write(master, b"q")
            self.assertEqual(child.wait(timeout=5), 0, captured.decode(errors="replace"))
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            reader.join(timeout=1)
            os.close(master)

    @unittest.skipIf(os.name == "nt", "POSIX interrupt regression")
    def test_watch_persists_before_shutdown_and_interrupts_sleep(self):
        history = self.root / "data" / "llmrace" / "history.jsonl"
        output = self.root / "watch.json"
        child = subprocess.Popen([str(BINARY), "mock", "--watch", "--every", "900", "--record",
                                  "-n", "1", "--warmup", "0", "--out", str(output)],
                                 cwd=self.root, env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        try:
            deadline = time.monotonic() + 8
            while not history.exists() and child.poll() is None and time.monotonic() < deadline:
                time.sleep(0.05)
            self.assertTrue(history.exists(), "history must exist while watch is still running")
            self.assertIsNone(child.poll())
            child.send_signal(signal.SIGINT)
            _, error = child.communicate(timeout=3)
            self.assertEqual(child.returncode, 0, error)
            self.assertEqual(len(history.read_text().splitlines()), 1, "shutdown must not duplicate records")
            self.assertEqual(json.loads(output.read_text())["runs"], 1)
        finally:
            if child.poll() is None:
                child.kill()
                child.communicate()

    def test_wheel_installs_runs_and_verifies_integrity(self):
        spec = importlib.util.spec_from_file_location("make_wheel", ROOT / "python" / "make_wheel.py")
        wheel_module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(wheel_module)
        version = json.loads(self.cli("--version-json").stdout)["version"]
        arch = "aarch64" if platform.machine().lower() in {"arm64", "aarch64"} else "x86_64"
        target = ("x86_64-pc-windows-msvc" if os.name == "nt" else
                  f"{arch}-apple-darwin" if sys.platform == "darwin" else f"{arch}-unknown-linux-gnu")
        archive = self.root / f"llmrace-v{version}-{target}.{'zip' if os.name == 'nt' else 'tar.gz'}"
        member = f"llmrace-v{version}-{target}/bin/{BINARY.name}"
        if os.name == "nt":
            with zipfile.ZipFile(archive, "w") as zipped:
                zipped.write(BINARY, member)
        else:
            with tarfile.open(archive, "w:gz") as tar:
                tar.add(BINARY, arcname=member)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        Path(str(archive) + ".sha256").write_text(f"{digest}  {archive.name}\n")
        wheel = wheel_module.build(version, str(archive), str(self.root / "dist"))
        with zipfile.ZipFile(wheel) as zipped:
            for license_name in ["LICENSE-MIT", "LICENSE-APACHE"]:
                self.assertEqual(zipped.read(f"llmrace-{version}.dist-info/licenses/{license_name}"),
                                 (ROOT / license_name).read_bytes())
        environment = self.root / "installed"
        venv.EnvBuilder(with_pip=True).create(environment)
        scripts = environment / ("Scripts" if os.name == "nt" else "bin")
        python = scripts / ("python.exe" if os.name == "nt" else "python")
        installed = subprocess.run([str(python), "-m", "pip", "install", "--no-index", "--no-deps", wheel],
                                   env=self.env, capture_output=True, text=True, timeout=60)
        self.assertEqual(installed.returncode, 0, installed.stderr)
        for command in [[str(python), "-m", "llmrace"], [str(scripts / BINARY.name)]]:
            result = subprocess.run([*command, "--version"], env=self.env, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), f"llmrace {version}")
        with archive.open("ab") as fh:
            fh.write(b"corruption")
        with self.assertRaises(SystemExit):
            wheel_module.build(version, str(archive), str(self.root / "bad-dist"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
