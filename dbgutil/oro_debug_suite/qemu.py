import subprocess
import tempfile
import shutil
import gdb  # type: ignore
import threading
from qemu.qmp import QMPClient, Runstate  # type:ignore
import asyncio
from os import path
from queue import SimpleQueue
import time
import signal


class QmpThread(threading.Thread):
    def __init__(self, qmp_fifo_path):
        super().__init__()
        self.__qmp_path = qmp_fifo_path
        self.__qmp = None
        self.__loop = asyncio.new_event_loop()

    def __on_request(self, request, response_queue):
        async def _on_request_async(request, response_queue):
            response = await self.__qmp.request(request)
            response_queue.put_nowait(response)

        self.__loop.create_task(_on_request_async(request, response_queue))

    def request(self, request):
        if self.__loop.is_closed():
            raise RuntimeError("QMP client has been closed")
        response_queue = SimpleQueue()
        self.__loop.call_soon_threadsafe(self.__on_request, request, response_queue)
        return response_queue.get()

    def run(self):
        self.__loop.run_until_complete(self.__run())
        self.__loop.close()

    async def __run(self):
        self.__qmp = QMPClient("oro-kernel")
        await self.__qmp.connect(self.__qmp_path)
        while True:
            # We've shut down and the connection is now IDLE.
            rs = await self.__qmp.runstate_changed()
            if rs == Runstate.IDLE:
                break

    async def __disconnect(self):
        if self.__qmp is not None and self.__qmp.runstate == Runstate.RUNNING:
            try:
                await self.__qmp.disconnect()
            except Exception as e:
                pass

    def __shutdown(self):
        self.__loop.create_task(self.__disconnect())

    def shutdown(self):
        try:
            self.__loop.call_soon_threadsafe(self.__shutdown)
        except:
            # Loop's already been closed.
            pass
        self.join()


def wait_for_file(file_path, timeout=5):
    """
    Waits for a file to exist, with a timeout.
    """

    for _ in range(timeout * 10):
        if path.exists(file_path):
            return
        time.sleep(0.1)

    raise TimeoutError(f"file not found (timed out waiting for it): {file_path}")


class QemuProcess(object):
    """
    Spawns QEMU with the given arguments and provides a way to connect GDB to it.

    Note that `-qmp` and `-gdb` arguments are automatically added to the arguments
    and should not be specified by the caller.
    """

    def __init__(self, args, **kwargs):
        self.__tmpdir = tempfile.mkdtemp()
        self.__qmp_path = path.join(self.__tmpdir, "qmp.sock")
        self.__qmp = QmpThread(self.__qmp_path)
        self.__gdbsrv_path = path.join(self.__tmpdir, "gdbsrv.sock")

        args = [
            *args,
            "-qmp",
            f"unix:{self.__qmp_path},server",
            "-gdb",
            f"unix:{self.__gdbsrv_path},server",
            "-S",
        ]

        self.__process = subprocess.Popen(
            args,
            **kwargs,
            stdin=subprocess.DEVNULL,
            close_fds=True,
            preexec_fn=lambda: signal.pthread_sigmask(
                signal.SIG_BLOCK, [signal.SIGINT]
            ),
        )

        wait_for_file(self.__qmp_path)
        self.__qmp.start()

    def poll(self):
        """
        Polls the underlying child process to check if it has terminated.
        """

        return self.__process.poll()

    def shutdown(self):
        """
        Safely shuts down the QEMU process and the QMP thread.
        """

        conn = gdb.selected_inferior().connection
        if isinstance(conn, gdb.RemoteTargetConnection) and conn.is_valid():
            details = conn.details
            if details == self.__gdbsrv_path:
                gdb.execute("disconnect", to_string=False, from_tty=False)

        if self.__qmp is not None:
            self.__qmp.shutdown()
            self.__qmp = None
        if self.__process is not None:
            if self.__process.poll() is None:
                self.__process.kill()
                self.__process.wait()
            self.__process = None
        shutil.rmtree(self.__tmpdir, ignore_errors=True)

    def __del__(self):
        self.shutdown()

    @property
    def pid(self):
        """
        The PID of the QEMU process, or None if the process has not been spawned /
        has already been terminated.
        """

        if self.__process is None:
            return None
        return self.__process.pid

    def connect_gdb(self):
        """
        Connects GDB to the QEMU process that was spawned.
        """

        wait_for_file(self.__gdbsrv_path)
        gdb.execute(
            f"target remote {self.__gdbsrv_path}", to_string=False, from_tty=False
        )
