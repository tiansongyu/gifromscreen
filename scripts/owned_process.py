"""Linux-only bounded ownership of a child session and its process group.

The leader is observed with waitid(WNOWAIT), never reaped until group cleanup.
Callers must not install a child-reaping SIGCHLD handler or wait on this PID.
Children which deliberately create another session are outside this contract.
"""

import os
import math
import signal
import stat
import subprocess
import sys
import time


class OwnedProcess:
    """One freshly created session; use as a context manager for guaranteed cleanup."""

    def __init__(self, command, *, terminate_timeout=1.0, kill_timeout=2.0, **kwargs):
        if sys.platform != "linux":
            raise ValueError("OwnedProcess requires Linux waitid(WNOWAIT) and /proc")
        if "start_new_session" in kwargs or "process_group" in kwargs or "preexec_fn" in kwargs:
            raise ValueError("OwnedProcess owns start_new_session/process_group/preexec_fn")
        if signal.getsignal(signal.SIGCHLD) != signal.SIG_DFL:
            raise ValueError("OwnedProcess requires the default, non-reaping SIGCHLD disposition")
        for name in ("stdout", "stderr"):
            self._check_output(name, kwargs.get(name))
        if kwargs.get("stdin") == subprocess.PIPE:
            raise ValueError("OwnedProcess does not support stdin=PIPE")
        if any(not isinstance(value, (int, float)) or not math.isfinite(value) or value <= 0
               for value in (terminate_timeout, kill_timeout)):
            raise ValueError("Cleanup timeouts must be finite and positive")
        self._terminate_timeout = terminate_timeout
        self._kill_timeout = kill_timeout
        self._closed = False
        self._result = None
        self._process = subprocess.Popen(command, start_new_session=True, **kwargs)
        self.pid = self._process.pid
        # Popen returns after successful exec; setsid has already completed.
        if os.getpgid(self.pid) != self.pid:
            raise RuntimeError("Child did not create its owned process group")

    @staticmethod
    def _check_output(name, value):
        if value in (None, subprocess.DEVNULL):
            return
        if name == "stderr" and value == subprocess.STDOUT:
            return
        if value == subprocess.PIPE:
            raise ValueError("OwnedProcess requires regular output logs, not PIPE")
        descriptor = value if isinstance(value, int) else value.fileno()
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            raise ValueError("OwnedProcess requires a regular " + name + " log")

    def poll(self):
        """Return exit status without reaping the leader or releasing its PID."""
        if self._closed:
            return self._result
        try:
            status = os.waitid(os.P_PID, self.pid, os.WEXITED | os.WNOWAIT | os.WNOHANG)
        except ChildProcessError as error:
            raise RuntimeError("Owned leader was reaped externally; refusing group signals") from error
        if status is None:
            return None
        return status.si_status if status.si_code == os.CLD_EXITED else -status.si_status

    def wait(self, timeout):
        """Wait up to timeout seconds, leaving an exited leader unreaped."""
        if not isinstance(timeout, (int, float)) or not math.isfinite(timeout) or timeout < 0:
            raise ValueError("Wait timeout must be finite and nonnegative")
        deadline = time.monotonic() + timeout
        while True:
            result = self.poll()
            if result is not None:
                return result
            if time.monotonic() >= deadline:
                raise subprocess.TimeoutExpired(self._process.args, timeout)
            time.sleep(min(0.01, max(0.0, deadline - time.monotonic())))

    def _members(self, deadline):
        """Read only /proc stat fields; exclude zombies, which cannot execute."""
        members = []
        with os.scandir("/proc") as entries:
            for entry in entries:
                if time.monotonic() >= deadline:
                    raise TimeoutError("Timed out checking the owned process group")
                if not entry.name.isdecimal():
                    continue
                try:
                    with open(entry.path + "/stat", "rb") as source:
                        record = source.read(4097)
                except (FileNotFoundError, ProcessLookupError):
                    continue
                except PermissionError:
                    # Foreign users may hide stat. Our descendants have the same
                    # UID and retain observable group membership in this scope.
                    continue
                if len(record) > 4096:
                    raise ValueError("Oversized /proc stat record")
                _, separator, fields = record.rpartition(b") ")
                parts = fields.split()
                if not separator or len(parts) < 3:
                    raise ValueError("Malformed /proc stat record")
                if int(parts[2]) == self.pid and parts[0] not in (b"Z", b"X"):
                    members.append(int(entry.name))
        return members

    def _signal(self, value):
        # WNOWAIT verifies child ownership immediately before every group signal.
        # The unreaped leader keeps this numeric PID/PGID unavailable for reuse.
        self.poll()
        if os.getpgid(self.pid) != self.pid:
            raise RuntimeError("Owned leader group changed; refusing group signals")
        try:
            os.killpg(self.pid, value)
        except ProcessLookupError:
            pass

    def _wait_group_empty(self, timeout):
        deadline = time.monotonic() + timeout
        while True:
            try:
                members = self._members(deadline)
            except TimeoutError:
                return False
            if not members:
                return True
            if time.monotonic() >= deadline:
                return False
            time.sleep(min(0.01, max(0.0, deadline - time.monotonic())))

    def close(self):
        """Terminate remaining group members, escalate if needed, then reap leader."""
        if self._closed:
            return
        self.poll()  # Refuse unsafe cleanup if some other code reaped our child.
        self._signal(signal.SIGTERM)
        empty = self._wait_group_empty(self._terminate_timeout)
        if not empty:
            self._signal(signal.SIGKILL)
            empty = self._wait_group_empty(self._kill_timeout)
        # Group signals always precede reaping; never signal this PGID afterward.
        self._result = self._process.wait(timeout=self._kill_timeout)
        self._closed = True
        if not empty:
            raise TimeoutError("Owned process group still has live members after SIGKILL")

    def __enter__(self):
        return self

    def __exit__(self, kind, value, traceback):
        self.close()


def run(command, *, timeout, check=False, **kwargs):
    """Run with owned group cleanup; return an integer exit status, never capture pipes."""
    with OwnedProcess(command, **kwargs) as process:
        result = process.wait(timeout)
    if check and result:
        raise subprocess.CalledProcessError(result, command)
    return result
