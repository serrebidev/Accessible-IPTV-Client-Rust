"""Real-app check that the built-in player's ticks run: a cut stream must reopen and a
steady one must not restart. Unit tests cannot see this; the player timers once died at
creation and every test still passed (validation 0071).

Windows only. Runs the app on a private desktop, so it takes no focus and NVDA stays
quiet. The stream is silent and settings are isolated from the real ones.

    python tools/smoke-player-recovery.py --app <folder with iptv-app.exe and libvlc.dll> --ffmpeg <ffmpeg.exe>
"""

import argparse
import ctypes as C
import http.server
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from ctypes import wintypes as W

k = C.WinDLL("kernel32", use_last_error=True)
u = C.WinDLL("user32", use_last_error=True)
u.CreateDesktopW.restype = W.HANDLE
u.CreateDesktopW.argtypes = [W.LPCWSTR, W.LPCWSTR, C.c_void_p, W.DWORD, W.DWORD, C.c_void_p]
u.SendMessageW.restype = W.LPARAM
u.SendMessageW.argtypes = [W.HWND, W.UINT, W.WPARAM, W.LPARAM]
u.PostMessageW.argtypes = [W.HWND, W.UINT, W.WPARAM, W.LPARAM]
ENUM = C.WINFUNCTYPE(W.BOOL, W.HWND, W.LPARAM)
STILL_ACTIVE = 259
TS_SECONDS = 120


class StartupInfo(C.Structure):
    _fields_ = [("cb", W.DWORD), ("lpReserved", W.LPWSTR), ("lpDesktop", W.LPWSTR), ("lpTitle", W.LPWSTR)] + [
        (name, W.DWORD)
        for name in "dwX dwY dwXSize dwYSize dwXCountChars dwYCountChars dwFillAttribute dwFlags".split()
    ] + [("wShowWindow", W.WORD), ("cbReserved2", W.WORD), ("lpReserved2", C.c_void_p),
         ("hStdInput", W.HANDLE), ("hStdOutput", W.HANDLE), ("hStdError", W.HANDLE)]


class ProcessInfo(C.Structure):
    _fields_ = [("hProcess", W.HANDLE), ("hThread", W.HANDLE), ("dwProcessId", W.DWORD), ("dwThreadId", W.DWORD)]


k.CreateProcessW.argtypes = [W.LPCWSTR, W.LPWSTR, C.c_void_p, C.c_void_p, W.BOOL, W.DWORD, C.c_void_p, W.LPCWSTR,
                             C.POINTER(StartupInfo), C.POINTER(ProcessInfo)]


def serve(data, cut_after):
    """Serve the stream in real time; cut each connection after cut_after seconds."""
    opens = []

    class Stream(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_GET(self):
            opens.append(time.perf_counter())
            self.send_response(200)
            self.send_header("Content-Type", "video/mp2t")
            self.end_headers()
            start, sent, rate = time.perf_counter(), 0, len(data) / TS_SECONDS
            try:
                while sent < len(data) and (cut_after is None or time.perf_counter() - start < cut_after):
                    self.wfile.write(data[sent:sent + 65536])
                    sent += 65536
                    wait = start + sent / rate - time.perf_counter()
                    if wait > 0:
                        time.sleep(wait)
            except OSError:
                pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Stream)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return f"http://127.0.0.1:{server.server_address[1]}/live/test.ts", opens


def find_channel_list(desktop):
    tops, found = [], []

    def child(hwnd, _):
        name = C.create_unicode_buffer(64)
        u.GetClassNameW(hwnd, name, 64)
        if name.value == "SysListView32":
            found.append(hwnd)
        return True

    child_cb = ENUM(child)
    top_cb = ENUM(lambda hwnd, _: (tops.append(hwnd), True)[1])
    u.EnumDesktopWindows(desktop, top_cb, 0)
    for top in tops:
        u.EnumChildWindows(top, child_cb, 0)
    return found[0] if found else None


def run(app, work, data, label, cut_after, watch):
    """Play the first channel; return (stream opens, process alive) after watch seconds."""
    url, opens = serve(data, cut_after)
    box = pathlib.Path(work) / label
    (box / "appdata").mkdir(parents=True)
    exe_dir = box / "app"
    shutil.copytree(app, exe_dir)
    playlist = box / "one.m3u"
    playlist.write_text(f'#EXTM3U\n#EXTINF:-1 group-title="A",Test\n{url}\n#EXTINF:-1 group-title="A",Second\n'
                        "http://127.0.0.1:9/b.ts\n", encoding="utf-8", newline="\n")
    (exe_dir / "iptvclient.conf").write_text(json.dumps({
        "playlists": [str(playlist)], "epg_enabled": False, "auto_check_updates": False,
        "show_player_on_enter": False, "media_player": "Built-in Player", "announcement_level": 0,
        "use_playlist_epg": False, "live_buffer_enabled": False,
    }), encoding="utf-8")

    desk_name = f"iptvsmoke{os.getpid()}{label}"
    desktop = u.CreateDesktopW(desk_name, None, None, 0, 0x10000000, None)
    env = dict(os.environ, APPDATA=str(box / "appdata"), LOCALAPPDATA=str(box / "appdata"))
    block = C.create_unicode_buffer("\0".join(f"{a}={b}" for a, b in env.items()) + "\0\0")
    startup = StartupInfo(cb=C.sizeof(StartupInfo), lpDesktop=desk_name)
    process = ProcessInfo()
    assert k.CreateProcessW(str(exe_dir / "iptv-app.exe"), None, None, None, False, 0x400, block, str(exe_dir),
                            C.byref(startup), C.byref(process)), C.get_last_error()
    try:
        deadline = time.perf_counter() + 60
        channels = None
        while time.perf_counter() < deadline and not channels:
            hwnd = find_channel_list(desktop)
            if hwnd and u.SendMessageW(hwnd, 0x1004, 0, 0) >= 2:  # LVM_GETITEMCOUNT
                channels = hwnd
            time.sleep(0.25)
        assert channels, "the channel list never filled"
        for key in (0x24, 0x0D):  # Home, Enter
            u.PostMessageW(channels, 0x100, key, 1)
            u.PostMessageW(channels, 0x101, key, 0xC0000001)
            time.sleep(0.5)
        deadline = time.perf_counter() + 40  # a fresh copy's first libVLC start is slow
        while time.perf_counter() < deadline and not opens:
            time.sleep(0.05)
        assert opens, "Enter never opened the stream"
        time.sleep(watch)
        code = W.DWORD()
        k.GetExitCodeProcess(process.hProcess, C.byref(code))
        return len(opens), code.value == STILL_ACTIVE
    finally:
        k.TerminateProcess(process.hProcess, 0)
        k.WaitForSingleObject(process.hProcess, 5000)
        u.CloseDesktop(desktop)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--app", required=True)
    parser.add_argument("--ffmpeg", required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as work:
        stream = pathlib.Path(work) / "silent.ts"
        subprocess.run([args.ffmpeg, "-hide_banner", "-loglevel", "error", "-y",
                        "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=25",
                        "-f", "lavfi", "-i", "anullsrc=channel_layout=stereo:sample_rate=48000",
                        "-t", str(TS_SECONDS), "-c:v", "libx264", "-preset", "ultrafast", "-b:v", "4M",
                        "-c:a", "aac", "-f", "mpegts", str(stream)], check=True)
        data = stream.read_bytes()
        opens, alive = run(args.app, work, data, "cut", 6, 30)
        print(f"cut stream: {opens} opens in 30 s, process {'alive' if alive else 'DEAD'}")
        ok = alive and opens >= 2
        opens, alive = run(args.app, work, data, "steady", None, 45)
        print(f"steady stream: {opens} opens in 45 s, process {'alive' if alive else 'DEAD'}")
        ok = ok and alive and opens == 1
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
