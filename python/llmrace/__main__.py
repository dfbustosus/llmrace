"""Console entry point: exec the bundled llmrace binary."""

import os
import stat
import sys


def main() -> None:
    name = "llmrace.exe" if sys.platform == "win32" else "llmrace"
    binary = os.path.join(os.path.dirname(os.path.abspath(__file__)), "bin", name)
    if not os.path.exists(binary):
        sys.stderr.write(
            f"llmrace: bundled binary not found at {binary}\n"
            "This wheel may not match your platform; see "
            "https://github.com/dfbustosus/llmrace#install for alternatives.\n"
        )
        raise SystemExit(1)
    # pip normalizes package files to 0o644 on install; the shim restores
    # the exec bit the wheel carried
    mode = os.stat(binary).st_mode
    if not mode & stat.S_IXUSR:
        os.chmod(binary, mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    os.execv(binary, [binary, *sys.argv[1:]])


if __name__ == "__main__":
    main()
