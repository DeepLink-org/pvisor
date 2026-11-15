"""Bounded native ConPTY fixture. No GUI, third-party terminal, or VM is started."""

import ctypes
import msvcrt
import os
import subprocess
import sys
import threading
import time
from ctypes import wintypes as w
from pathlib import Path


class Coord(ctypes.Structure):
    _fields_ = [("X", ctypes.c_short), ("Y", ctypes.c_short)]


class StartupInfo(ctypes.Structure):
    _fields_ = [
        ("cb", w.DWORD),
        ("lpReserved", w.LPWSTR),
        ("lpDesktop", w.LPWSTR),
        ("lpTitle", w.LPWSTR),
        ("dwX", w.DWORD),
        ("dwY", w.DWORD),
        ("dwXSize", w.DWORD),
        ("dwYSize", w.DWORD),
        ("dwXCountChars", w.DWORD),
        ("dwYCountChars", w.DWORD),
        ("dwFillAttribute", w.DWORD),
        ("dwFlags", w.DWORD),
        ("wShowWindow", w.WORD),
        ("cbReserved2", w.WORD),
        ("lpReserved2", ctypes.c_void_p),
        ("hStdInput", w.HANDLE),
        ("hStdOutput", w.HANDLE),
        ("hStdError", w.HANDLE),
    ]


class StartupInfoEx(ctypes.Structure):
    _fields_ = [("StartupInfo", StartupInfo), ("lpAttributeList", ctypes.c_void_p)]


class ProcessInfo(ctypes.Structure):
    _fields_ = [
        ("hProcess", w.HANDLE),
        ("hThread", w.HANDLE),
        ("dwProcessId", w.DWORD),
        ("dwThreadId", w.DWORD),
    ]


class Pty:
    def __init__(self, command, env, columns=91, rows=37):
        self.k = ctypes.WinDLL("kernel32", use_last_error=True)
        self.k.CreatePipe.argtypes = [
            ctypes.POINTER(w.HANDLE),
            ctypes.POINTER(w.HANDLE),
            ctypes.c_void_p,
            w.DWORD,
        ]
        self.k.CreatePseudoConsole.argtypes = [
            Coord,
            w.HANDLE,
            w.HANDLE,
            w.DWORD,
            ctypes.POINTER(w.HANDLE),
        ]
        self.k.CreatePseudoConsole.restype = ctypes.c_long
        self.k.ClosePseudoConsole.argtypes = [w.HANDLE]
        self.k.InitializeProcThreadAttributeList.argtypes = [
            ctypes.c_void_p,
            w.DWORD,
            w.DWORD,
            ctypes.POINTER(ctypes.c_size_t),
        ]
        self.k.UpdateProcThreadAttribute.argtypes = [
            ctypes.c_void_p,
            w.DWORD,
            ctypes.c_size_t,
            ctypes.c_void_p,
            ctypes.c_size_t,
            ctypes.c_void_p,
            ctypes.c_void_p,
        ]
        self.k.DeleteProcThreadAttributeList.argtypes = [ctypes.c_void_p]
        self.k.CreateProcessW.argtypes = [
            w.LPCWSTR,
            w.LPWSTR,
            ctypes.c_void_p,
            ctypes.c_void_p,
            w.BOOL,
            w.DWORD,
            ctypes.c_void_p,
            w.LPCWSTR,
            ctypes.POINTER(StartupInfoEx),
            ctypes.POINTER(ProcessInfo),
        ]
        self.k.CloseHandle.argtypes = [w.HANDLE]
        self.k.WaitForSingleObject.argtypes = [w.HANDLE, w.DWORD]
        self.k.GetExitCodeProcess.argtypes = [w.HANDLE, ctypes.POINTER(w.DWORD)]
        self.k.TerminateProcess.argtypes = [w.HANDLE, w.UINT]
        self.console = w.HANDLE()
        self.process = None
        self.pid = None
        self.reader = None
        self.input = self.output = None
        self.raw = []
        self.bytes = bytearray()
        self.lock = threading.Lock()
        self.changed = threading.Event()
        self.error = None
        self.closing = False
        attrs = None
        attrs_ready = False
        try:
            ir, iw, ore, ow = (w.HANDLE() for _ in range(4))
            self._check(self.k.CreatePipe(ctypes.byref(ir), ctypes.byref(iw), None, 0))
            self.raw.extend([ir.value, iw.value])
            self._check(self.k.CreatePipe(ctypes.byref(ore), ctypes.byref(ow), None, 0))
            self.raw.extend([ore.value, ow.value])
            hr = self.k.CreatePseudoConsole(
                Coord(columns, rows), ir, ow, 0, ctypes.byref(self.console)
            )
            if hr != 0:
                raise OSError(f"CreatePseudoConsole HRESULT=0x{hr & 0xFFFFFFFF:08x}")
            self._close_handle(ir.value)
            self._close_handle(ow.value)
            self.input = self._file(iw.value, "wb")
            self.output = self._file(ore.value, "rb")
            self.reader = threading.Thread(target=self._read, daemon=True)
            self.reader.start()
            size = ctypes.c_size_t()
            self.k.InitializeProcThreadAttributeList(None, 1, 0, ctypes.byref(size))
            attrs = ctypes.create_string_buffer(size.value)
            self._check(self.k.InitializeProcThreadAttributeList(attrs, 1, 0, ctypes.byref(size)))
            attrs_ready = True
            self._check(
                self.k.UpdateProcThreadAttribute(
                    attrs, 0, 0x00020016, self.console, ctypes.sizeof(w.HANDLE), None, None
                )
            )
            startup = StartupInfoEx()
            startup.StartupInfo.cb = ctypes.sizeof(startup)
            startup.lpAttributeList = ctypes.cast(attrs, ctypes.c_void_p)
            process = ProcessInfo()
            # A ConPTY attaches a console, but CreateProcess can still copy an
            # SSH parent's redirected standard-handle table. Bootstrap on that
            # console and explicitly pass CONIN$/CONOUT$ handles to the real
            # child. No shell parsing or argument rewriting is involved.
            bootstrap = [sys.executable, str(Path(__file__).resolve()), "--console-child", *command]
            line = ctypes.create_unicode_buffer(
                subprocess.list2cmdline([str(word) for word in bootstrap])
            )
            block = ctypes.create_unicode_buffer(
                "\0".join(
                    f"{key}={value}"
                    for key, value in sorted(env.items(), key=lambda item: item[0].casefold())
                )
                + "\0\0"
            )
            self._check(
                self.k.CreateProcessW(
                    sys.executable,
                    line,
                    None,
                    None,
                    False,
                    0x00080000 | 0x00000400,
                    block,
                    None,
                    ctypes.byref(startup),
                    ctypes.byref(process),
                )
            )
            self.process = process.hProcess
            self.pid = process.dwProcessId
            self.k.CloseHandle(process.hThread)
        except BaseException:
            self.close()
            raise
        finally:
            if attrs_ready:
                self.k.DeleteProcThreadAttributeList(attrs)

    @staticmethod
    def _check(ok):
        if not ok:
            raise ctypes.WinError(ctypes.get_last_error())

    def _close_handle(self, handle):
        self.k.CloseHandle(handle)
        self.raw.remove(handle)

    def _file(self, handle, mode):
        access = os.O_WRONLY if "w" in mode else os.O_RDONLY
        fd = msvcrt.open_osfhandle(handle, os.O_BINARY | access)
        self.raw.remove(handle)
        return os.fdopen(fd, mode, buffering=0)

    def _read(self):
        try:
            while data := self.output.read(4096):
                with self.lock:
                    self.bytes.extend(data)
                self.changed.set()
        except OSError as error:
            if not self.closing:
                self.error = error
        finally:
            self.changed.set()

    def data(self):
        with self.lock:
            return bytes(self.bytes)

    def send(self, data):
        while data:
            written = self.input.write(data)
            if not written:
                raise OSError("ConPTY input closed")
            data = data[written:]

    def expect(self, marker, timeout=30):
        deadline = time.monotonic() + timeout
        exited = None
        while marker not in self.data():
            if self.error:
                raise self.error
            if time.monotonic() >= deadline:
                raise AssertionError(f"ConPTY did not produce {marker!r}: {self.data()!r}")
            if self.k.WaitForSingleObject(self.process, 0) == 0:
                exited = exited or time.monotonic()
                if time.monotonic() - exited > 1:
                    raise AssertionError(
                        f"ConPTY process exited {self.wait()} before {marker!r}: {self.data()!r}"
                    )
            self.changed.wait(0.02)
            self.changed.clear()

    def wait(self, timeout=30):
        result = self.k.WaitForSingleObject(self.process, int(timeout * 1000))
        if result != 0:
            raise AssertionError(f"ConPTY process deadline exceeded ({result})")
        code = w.DWORD()
        self._check(self.k.GetExitCodeProcess(self.process, ctypes.byref(code)))
        return code.value

    def close(self):
        self.closing = True
        if self.process and self.k.WaitForSingleObject(self.process, 0) == 0x102:
            # Include the console bootstrap's child on a deadline failure.
            subprocess.run(
                ["taskkill", "/PID", str(self.pid), "/T", "/F"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=10,
                check=False,
            )
            self.k.TerminateProcess(self.process, 125)
            self.k.WaitForSingleObject(self.process, 5000)
        if self.console:
            self.k.ClosePseudoConsole(self.console)
            self.console = None
        if self.reader:
            self.reader.join(timeout=5)
            if self.reader.is_alive():
                raise AssertionError("ConPTY reader did not drain after close")
        for stream in (self.input, self.output):
            if stream:
                stream.close()
        if self.process:
            self.k.CloseHandle(self.process)
            self.process = None
        for handle in list(self.raw):
            self._close_handle(handle)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


if __name__ == "__main__":
    if sys.argv[1:2] != ["--console-child"] or len(sys.argv) < 3:
        raise SystemExit("private ConPTY console bootstrap requires a command")
    with open("CONIN$", "rb", buffering=0) as input, open("CONOUT$", "wb", buffering=0) as output:
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.GetConsoleMode.argtypes = [w.HANDLE, ctypes.POINTER(w.DWORD)]

        def console_state():
            modes = []
            for stream in (input, output):
                mode = w.DWORD()
                Pty._check(
                    kernel.GetConsoleMode(msvcrt.get_osfhandle(stream.fileno()), ctypes.byref(mode))
                )
                modes.append(mode.value)
            return modes, kernel.GetConsoleCP(), kernel.GetConsoleOutputCP()

        before = console_state()
        code = subprocess.call(sys.argv[2:], stdin=input, stdout=output, stderr=output)
        after = console_state()
        if after != before:
            raise AssertionError(
                f"child did not restore console modes/code pages: {before!r} -> {after!r}"
            )
        raise SystemExit(code)
