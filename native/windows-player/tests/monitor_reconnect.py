"""Replay output loss in the real player with a private pipe and an offscreen GPU target."""

import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import uuid


def check(condition, detail):
    if not condition:
        raise AssertionError(detail)


def exercise(binary, source, root, dx12, hardware):
    pack = "tidal-signal" if dx12 else "afterimage"
    control, value = ("radiance", 0.5) if dx12 else ("brightness", 73)
    state = root / "state"
    state.mkdir()
    (state / "native-player.json").write_text(json.dumps({
        "version": 1, "monitorDevice": "keyboard-port-A",
        "monitorTargetPath": "keyboard-port-A", "monitorTargetSerial": "test-keyboard",
    }), encoding="utf-8")
    keymap = Path("existing-resources/finalmouse/action75-ue-keymap.json")
    (root / keymap).parent.mkdir(parents=True)
    shutil.copyfile(source / keymap, root / keymap)
    (root / "data").mkdir()
    shutil.copyfile(source / "assets/defaults/fm-calibration.json", root / "data/fm-calibration.json")
    if hardware:
        (root / "data/app-settings.json").write_text('{"gpuPreference":"integrated"}', encoding="utf-8")
    shutil.copytree(source / "tools/fixtures/builtins-abi1/afterimage", root / "wallpapers-native/afterimage")
    if dx12:
        shutil.copytree(source / "examples/native-wallpapers/tidal-signal/pack", root / "wallpapers-native" / pack)
    name = "Action75ReconnectTest-" + uuid.uuid4().hex
    pipe = Path(r"\\.\pipe") / name
    args = [binary, "--headless-test", "--pipe", name, "--root", root, "--state-dir", state]
    if hardware:
        args.append("--headless-hardware")
    with subprocess.Popen(args, creationflags=subprocess.CREATE_NO_WINDOW) as player:
        def call(action="status", **fields):
            payload = dict(action=action, expectedPlayerPid=player.pid, expectedRuntimeRoot=str(root), **fields)
            deadline = time.monotonic() + 12
            while True:
                try:
                    channel = pipe.open("r+b", buffering=0)
                    break
                except OSError as error:
                    if player.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError(f"Could not connect to test player: {player.poll()}") from error
                    time.sleep(.02)
            with channel:
                channel.write(json.dumps(payload).encode() + b"\n")
                reply = json.loads(channel.readline())
            check(reply["ok"], reply)
            return reply["result"]

        def until(predicate):
            deadline = time.monotonic() + 8
            while True:
                status = call()
                if predicate(status):
                    return status
                check(time.monotonic() < deadline, status)
                time.sleep(.025)

        def topology(device, key=False, **fields):
            return call("testMonitor", device=device, firstKey=key, **fields)

        def image_digest():
            call("testFrame")
            return hashlib.sha256((state / "test-frame.png").read_bytes()).digest()

        try:
            armed = call()
            check(armed["headless"] and not armed["rawInputRegistered"] and not armed["exitShortcutRegistered"], armed)
            topology("keyboard-port-A")
            if dx12:
                legacy = call("selectPack", id="afterimage")
                check(legacy["renderer"] == "D3D11/NativePlugin", legacy)
            selected = call("selectPack", id=pack)
            check(selected["headlessHardware"] == hardware, selected)
            if hardware:
                check(selected["gpuAdapter"] and "Microsoft Basic Render Driver" not in selected["gpuAdapter"], selected)
            renderer = "D3D12/NativePlugin" if dx12 else "D3D11/NativePlugin"
            check(selected["renderer"] == renderer, selected)
            worker = selected["nativeWorkerPid"]
            check(selected["monitorConnected"] and worker, selected)
            unchanged = call("selectPack", id="afterimage" if dx12 else "black-hole", onlyIfEmpty=True)
            check(unchanged["packId"] == pack and unchanged["nativeWorkerPid"] == worker, unchanged)

            resize_started = time.monotonic()
            failed = topology("keyboard-port-A", key=True, width=1600, height=600, failResizeOnce=True)
            check(failed["monitorConnected"] and not failed["outputReady"], failed)
            check(failed["gpuRecovering"] and failed["keyHandle"] < 0 and failed["nativeWorkerPid"] == worker, failed)
            resized = until(lambda s: s["outputReady"] and s["framesRendered"] > failed["framesRendered"])
            retry_ms = round((time.monotonic() - resize_started) * 1000)
            check(resized["outputWidth"] == 1600 and resized["outputHeight"] == 600, resized)
            check(not resized["gpuRecovering"] and resized["nativeWorkerPid"] != worker, resized)
            accepted = topology("keyboard-port-A", key=True, width=1600, height=600)
            check(accepted["keyHandle"] > 0 and accepted["nativeWorkerPid"] == resized["nativeWorkerPid"], accepted)
            output = f"{selected['renderer']} ({selected['gpuAdapter']})"
            print(f"{output}: automatic 1600x600 output and first key recovered in {retry_ms} ms", flush=True)
            restored = topology("keyboard-port-A")
            check(restored["outputReady"], restored)
            worker = restored["nativeWorkerPid"]

            # A momentary unplug pauses output and input, then resumes the same worker.
            missing = topology("", key=True)
            check(not missing["monitorConnected"] and missing["keyHandle"] < 0, missing)
            time.sleep(.12)
            short = topology("keyboard-port-A")
            check(short["nativeWorkerPid"] == worker and not short["graphicsSuspended"], short)
            time.sleep(1.65)
            settled = call()
            check(settled["nativeWorkerPid"] == worker and settled["monitorConnected"], settled)
            idle = image_digest()

            # Let the production scheduler expire its grace period and release the device.
            missing = topology("")
            dormant = until(lambda s: s["graphicsSuspended"])
            check(not dormant["nativeWorkerPid"] and not dormant["gpuAdapterLuid"], dormant)
            check(dormant["framesRendered"] == missing["framesRendered"], dormant)
            profile = call("getPackSettings", id=pack)
            updated = call("setPackSettings", id=pack, expectedRevision=profile["revision"],
                           values={control: value})
            check(updated["revision"] > profile["revision"], updated)

            # The first physical-input handler runs synchronously with reattachment,
            # before an idle pass could rescue an incorrectly deferred device rebuild.
            connected = topology("keyboard-port-B", key=True)
            check(connected["monitorDevice"] == "keyboard-port-B" and connected["packId"] == pack, connected)
            check(connected["nativeWorkerPid"] and connected["nativeWorkerPid"] != worker, connected)
            check(connected["keyHandle"] > 0 and not connected["graphicsSuspended"], connected)
            check(connected["packSettingsRevision"] == updated["revision"], connected)
            until(lambda s: s["framesRendered"] > dormant["framesRendered"])
            check(image_digest() != idle, "First key did not change the rendered image")
            # Both idle captures must match with the saved settings applied.
            # This rules out a background or decoder change masquerading as a key effect.
            deadline = time.monotonic() + 5
            while image_digest() != idle:
                check(time.monotonic() < deadline, "Wallpaper did not return to its original idle image")
                time.sleep(.1)
            confirmed = call("getPackSettings", id=pack)
            check(confirmed["values"] == updated["values"] and confirmed["revision"] == updated["revision"], confirmed)
            persisted = json.loads((state / "wallpaper-settings" / f"{pack}.json").read_text(encoding="utf-8"))
            check(persisted["values"][control] == value and persisted["revision"] == updated["revision"], persisted)
            if dx12:
                legacy = call("selectPack", id="afterimage")
                check(legacy["renderer"] == "D3D11/NativePlugin" and legacy["nativeWorkerPid"] != connected["nativeWorkerPid"], legacy)
            print(f"{output}: brief reconnect retained worker; long reconnect restored settings and first-key pixels", flush=True)
            call("stop")
            player.wait(5)
        finally:
            if player.poll() is None:
                player.kill()
                player.wait()


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="action75-reconnect-") as temporary:
        options = sys.argv[3:]
        exercise(Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve(), Path(temporary),
                 "--dx12" in options, "--hardware" in options)
