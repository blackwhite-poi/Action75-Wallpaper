"""Verify the player window, real composition worker, and both Explorer desktop layouts."""
import ctypes as C
from ctypes import wintypes as W
from pathlib import Path
import json
import shutil
import sys
import tempfile
import time
import uuid
from contextlib import contextmanager, ExitStack

binary, fixture, repo = (Path(p).resolve() for p in sys.argv[1:4])
sys.path.insert(0,str(repo / "tools"))
import release_qa_gui as win

parent = win.api(win.USER,"GetParent",W.HWND,W.HWND)
sibling = win.api(win.USER,"GetWindow",W.HWND,W.HWND,W.UINT)
get_prop = win.api(win.USER,"GetPropW",W.HANDLE,W.HWND,W.LPCWSTR)
bounds = win.api(win.USER,"GetWindowRect",W.BOOL,W.HWND,C.POINTER(W.RECT))
show_window = win.api(win.USER,"ShowWindowAsync",W.BOOL,W.HWND,C.c_int)

@contextmanager
def process(args, root, desktop):
    proc=win.spawn_on_desktop([str(x) for x in args], root, desktop)
    try:
        yield proc
    finally:
        if win.wait(proc.handle,0):
            win.terminate(proc.handle,1)
            win.wait(proc.handle,5000)
        win.close(proc.thread)
        win.close(proc.handle)

def exercise(root, layout, presentation):
    name="Action75MemQA-"+uuid.uuid4().hex
    desktop=win.checked(win.create_desktop(name,None,None,0,0x1ff,None))
    try:
        with ExitStack() as fixtures:
            shell=fixtures.enter_context(process([fixture,layout],root,name))
            def top(cls):
                return win.matching_windows(win.enum_desktop,desktop,lambda h:win.text_of(win.window_class,h)==cls)
            def child(h,cls):
                return win.matching_windows(win.enum_children,h,lambda w:win.text_of(win.window_class,w)==cls)
            progman=win.wait_for(lambda:top("Progman"),"fixture desktop")[0]
            parent_window=progman if layout=="modern" else win.wait_for(
                lambda:[h for h in top("WorkerW") if win.text_of(win.window_title,h)=="background"],"fixture worker")[0]
            token=uuid.uuid4().hex
            state=root/token
            state.mkdir()
            (state/"native-player.json").write_text(json.dumps(dict(monitorDevice="A",monitorTargetPath="A",monitorTargetSerial="test-keyboard")))
            endpoint=Path(r"\\.\pipe")/token
            with process([binary,"--headless-test","--headless-hardware","--headless-window",presentation,
                          "--root",root,"--state-dir",state,"--pipe",token],root,name) as player:
                def call(action, fail=False, **fields):
                    until=time.monotonic()+6
                    while True:
                        try:
                            stream=endpoint.open("r+b",buffering=0)
                            break
                        except OSError:
                            if time.monotonic()>=until or not win.wait(player.handle,0):
                                raise
                            time.sleep(.02)
                    with stream:
                        stream.write(json.dumps(dict(action=action,expectedRuntimeRoot=str(root),
                                                    expectedPlayerPid=player.pid,**fields)).encode()+b"\n")
                        reply=json.loads(stream.readline())
                    assert reply["ok"]!=fail,reply
                    return reply.get("result",reply)
                call("testMonitor",device="A",width=640,height=360)
                active=call("selectPack",id="tidal-signal")
                worker=active["nativeWorkerPid"]
                assert worker and not active["desktopLayerActive"],active
                host=top("Action75NativeWindow")[0]
                assert win.visible(host)
                def keyboard_bar():
                    return next(h for h in top("Shell_SecondaryTrayWnd")
                                if win.text_of(win.window_title,h)=="keyboard bar")
                def check_taskbars(hidden):
                    win.wait_for(lambda:bool(win.visible(keyboard_bar()))!=hidden,
                                 "keyboard taskbar visibility")
                    assert all(win.visible(h) for h in top("Shell_TrayWnd"))
                    assert all(win.visible(h) for h in top("Shell_SecondaryTrayWnd") if h!=keyboard_bar())
                def hide_taskbar(value,fail=False):
                    return call("setSettings",fail=fail,settings={"hideKeyboardTaskbar":value})
                assert active["hideKeyboardTaskbar"] is False
                check_taskbars(False)
                hide_taskbar("invalid",True)
                assert hide_taskbar(True)["hideKeyboardTaskbar"] is True
                check_taskbars(True)
                assert json.loads((state/"native-player.json").read_text())["hideKeyboardTaskbar"] is True
                win.checked(show_window(keyboard_bar(),4))  # SW_SHOWNOACTIVATE
                win.wait_for(lambda:win.visible(keyboard_bar()),"Explorer re-showing the taskbar")
                check_taskbars(True)
                def opacity(value,fail=False):
                    return call("setPackSettings",fail=fail,id="tidal-signal",values={"desktop":value})
                def check_session_lock(surface):
                    win.post(host,0x02B1,7,0)  # WM_WTSSESSION_CHANGE / WTS_SESSION_LOCK
                    win.wait_for(lambda:not call("status")["outputReady"],"locked output")
                    assert not win.visible(host) and not win.visible(surface)
                    check_taskbars(False)
                    # Display changes while locked must not re-show either window.
                    locked=call("testMonitor",device="A",width=640,height=360,firstKey=True)
                    assert not locked["outputReady"] and locked["keyHandle"]<0
                    assert not win.visible(host) and not win.visible(surface)
                    win.post(host,0x02B1,8,0)  # WTS_SESSION_UNLOCK
                    ready=win.wait_for(lambda:win.visible(surface) and call("status"),"unlocked output")
                    assert ready["outputReady"] and ready["nativeWorkerPid"]==worker,ready
                    check_taskbars(True)
                check_session_lock(host)
                if presentation=="shared":
                    opacity(50,True)
                    assert call("getPackSettings",id="tidal-signal")["values"]["desktop"]==100
                    assert win.visible(host) and call("status")["nativeWorkerPid"]==worker
                else:
                    for value in (50,0,100,70):
                        opacity(value)
                        status=call("status")
                        assert status["nativeWorkerPid"]==worker and status["desktopLayerActive"]==(value<100),status
                        check_taskbars(True)
                        layers=child(parent_window,"Action75DesktopSurface")
                        assert bool(win.visible(host))==(value==100)
                        if value==100:
                            assert not layers
                        else:
                            assert len(layers)==1 and win.visible(layers[0]) and parent(layers[0])==parent_window,layers
                            if layout=="modern":
                                assert sibling(layers[0],3)==child(progman,"SHELLDLL_DefView")[0]
                            a,b=W.RECT(),W.RECT()
                            assert bounds(host,C.byref(a)) and bounds(layers[0],C.byref(b)) and bytes(a)==bytes(b)
                    old_layer=child(parent_window,"Action75DesktopSurface")[0]
                    check_session_lock(old_layer)
                    with process([fixture,"foreign"],root,name):
                        win.wait_for(lambda:any(get_prop(w,"Action75ReorderComplete") for w in child(parent_window,"Static")),
                                     "separate process completing its z-order change")
                        wanted=child(progman,"SHELLDLL_DefView")[0] if layout=="modern" else None
                        win.wait_for(lambda:sibling(old_layer,3)==wanted,"overlay above changed wallpaper")
                    assert call("status")["nativeWorkerPid"]==worker
                    assert call("testMonitor",device="",firstKey=True)["keyHandle"]<0
                    assert not win.visible(host) and not win.visible(child(parent_window,"Action75DesktopSurface")[0])
                    check_taskbars(False)
                    assert call("testMonitor",device="A",width=640,height=360,firstKey=True)["keyHandle"]>0
                    assert win.visible(child(parent_window,"Action75DesktopSurface")[0])
                    check_taskbars(True)
                    call("testMonitor",device="")
                    win.wait_for(lambda:call("status")["graphicsSuspended"],"detached graphics release")
                    restored=call("testMonitor",device="A",width=640,height=360,firstKey=True)
                    assert restored["nativeWorkerPid"]!=worker and restored["keyHandle"]>0 and restored["desktopLayerActive"],restored
                    assert call("getPackSettings",id="tidal-signal")["values"]["desktop"]==70
                    if layout=="modern":
                        # Restart only our mock shell. The IPC window must outlive its Explorer child.
                        assert win.terminate(shell.handle,0) and not win.wait(shell.handle,5000)
                        win.post(host,0x001A,0,0)
                        assert not win.visible(host) and call("status")["nativeWorkerPid"]==restored["nativeWorkerPid"]
                        fixtures.enter_context(process([fixture,layout],root,name))
                        progman=win.wait_for(lambda:top("Progman"),"replacement shell")[0]
                        win.post(host,0x001A,0,0)
                        parent_window=progman
                        win.wait_for(lambda:any(win.visible(h) for h in child(progman,"Action75DesktopSurface")),"restored shell layer")
                        assert call("status")["nativeWorkerPid"]==restored["nativeWorkerPid"]
                        check_taskbars(True)
                assert hide_taskbar(False)["hideKeyboardTaskbar"] is False
                check_taskbars(False)
                hide_taskbar(True)
                check_taskbars(True)
                if layout=="classic" and presentation=="native":
                    assert win.terminate(player.handle,0)
                else:
                    call("stop")
                assert win.wait(player.handle,5000)==0
                check_taskbars(False)
                assert not child(parent_window,"Action75DesktopSurface")
    finally:
        win.close_desktop(desktop)

with tempfile.TemporaryDirectory(prefix="a75-desktop-qa-") as temp:
    root=Path(temp)
    for src,dst in [("assets/defaults/fm-calibration.json","data/fm-calibration.json"),
                    ("existing-resources/finalmouse/action75-ue-keymap.json","existing-resources/finalmouse/action75-ue-keymap.json")]:
        (root/dst).parent.mkdir(parents=True,exist_ok=True)
        shutil.copyfile(repo/src,root/dst)
    pack=root/"wallpapers-native/tidal-signal"
    shutil.copytree(repo/"examples/native-wallpapers/tidal-signal/pack",pack)
    for file in ("manifest","settings"):
        path=pack/(file+".json")
        obj=json.loads(path.read_text(encoding="utf8"))
        if file=="manifest":
            obj["desktopOpacityControl"]="desktop"
        else:
            obj["groups"][0]["controls"].append(dict(id="desktop",label="alpha",type="slider",min=0,max=100,default=100,step=1))
        path.write_text(json.dumps(obj),encoding="utf8")
    for layout,presentation in (("modern","native"),("classic","native"),("modern","shared")):
        exercise(root,layout,presentation)
        print(layout,presentation,"passed",flush=True)
