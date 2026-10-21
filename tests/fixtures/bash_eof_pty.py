"""Exercise the production wrapper against a real Linux terminal line discipline."""
import os
import pty
import select
import signal
import sys
import time

pid, fd = pty.fork()
if pid == 0:
    os.environ["PS1"] = "UU_TEST_PROMPT> "
    os.execvp("bash", ["bash", "--noprofile", "--norc", "-i"])

pending = b""


def until(needle):
    global pending
    deadline = time.monotonic() + 5
    while needle not in pending:
        assert time.monotonic() < deadline, repr(pending)
        if select.select([fd], [], [], 0.1)[0]:
            pending += os.read(fd, 65536)
    end = pending.index(needle) + len(needle)
    result, pending = pending[:end], pending[end:]
    return result


try:
    until(b"UU_TEST_PROMPT> ")
    # One delayed EOF per completed job must not accumulate and exit Bash.
    for _ in range(25):
        os.write(fd, sys.argv[1].encode())
        until(b"UUCfastjobE0Z")
        until(b"UU_TEST_PROMPT> ")
        os.write(fd, b"\x04")
        until(b"UU_TEST_PROMPT> ")

    # The same VEOF must still terminate a foreground stdin consumer.
    os.write(fd, sys.argv[2].encode())
    until(b"UUCstdinjobB")
    os.write(fd, b"stdin-payload\n")
    until(b"stdin-payload\r\n")
    os.write(fd, b"\x04")
    result = until(b"UUCstdinjobE0Z")
    assert b"stdin-complete" in result, repr(result)
    until(b"UU_TEST_PROMPT> ")

    os.write(fd, sys.argv[3].encode())
    result = until(b"UUCafterjobE0Z")
    assert b"worker-alive" in result, repr(result)
    print("real PTY: idle EOF safe, foreground EOF delivered, worker reusable")
finally:
    os.killpg(pid, signal.SIGKILL)
    os.waitpid(pid, 0)
    os.close(fd)
