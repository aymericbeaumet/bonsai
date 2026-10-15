"""Real PTY regression checks: python3 tests/terminal.py target/debug/bonsai."""

import fcntl
import os
import pathlib
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unittest

BINARY = str(pathlib.Path(sys.argv.pop(1)).resolve())


class TerminalTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = pathlib.Path(self.temp.name) / "repo"
        self.repo.mkdir()
        self.env = dict(os.environ, HOME=self.temp.name, BONSAI_ROOT=self.temp.name + "/trees",
                        GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
                        GIT_AUTHOR_NAME="Test", GIT_AUTHOR_EMAIL="test@example.com",
                        GIT_COMMITTER_NAME="Test", GIT_COMMITTER_EMAIL="test@example.com",
                        TERM="xterm-256color", BONSAI_ADD__INSTALL="false", BONSAI_ADD__FETCH="false")
        self.env.pop("_BONSAI_WRAPPED", None)
        for args in [["git", "init", "-b", "main"],
                     ["git", "commit", "--allow-empty", "-m", "seed"],
                     ["git", "remote", "add", "origin", "https://example.com/test/repo.git"],
                     [BINARY, "add", "ab/terminal"]]:
            result = subprocess.run(args, cwd=self.repo, env=self.env, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr.decode())

    def check_restoration(self, sig=None, escape=False, args=None):
        master, slave = pty.openpty()
        self.addCleanup(os.close, master)
        self.addCleanup(os.close, slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        before = termios.tcgetattr(slave)

        def controlling_terminal():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        process = subprocess.Popen([BINARY] + (args or ["cd"]), cwd=self.repo, env=self.env,
                                   stdin=slave, stdout=slave, stderr=slave,
                                   preexec_fn=controlling_terminal)
        def reap():
            if process.poll() is None:
                process.kill()
            process.wait()
        self.addCleanup(reap)
        deadline = time.monotonic() + 10
        captured = b""
        prompt = b"Worktree" if not args else b"Branch"
        while (termios.tcgetattr(slave) == before or prompt not in captured) and time.monotonic() < deadline:
            if select.select([master], [], [], 0.05)[0]:
                chunk = os.read(master, 65536)
                captured += chunk
                if b"\x1b[6n" in chunk:
                    os.write(master, b"\x1b[1;1R")
            if process.poll() is not None:
                self.fail(f"picker exited early: {captured!r}")
        self.assertNotEqual(termios.tcgetattr(slave), before, captured)
        # Resize while the picker is active, including a narrow viewport.
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 8, 12, 0, 0))
        process.send_signal(signal.SIGWINCH)
        if escape:
            os.write(master, b"\x1b")
        else:
            process.send_signal(sig)
        deadline = time.monotonic() + 10
        while process.poll() is None and time.monotonic() < deadline:
            if select.select([master], [], [], 0.05)[0]:
                try:
                    captured += os.read(master, 65536)
                except OSError:
                    break
        process.wait(timeout=1)
        self.assertEqual(termios.tcgetattr(master), before)
        if sig:
            self.assertEqual(process.returncode, -sig)

    def test_sigterm_restores_picker(self):
        self.check_restoration(signal.SIGTERM)

    def test_sighup_restores_picker(self):
        self.check_restoration(signal.SIGHUP)

    def test_sigint_restores_picker(self):
        self.check_restoration(signal.SIGINT)

    def test_escape_restores_picker(self):
        self.check_restoration(escape=True)

    def test_inquire_prompt_restores_on_sigterm(self):
        self.check_restoration(signal.SIGTERM, args=["add"])


if __name__ == "__main__":
    unittest.main()
