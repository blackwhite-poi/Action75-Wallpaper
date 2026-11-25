@echo off
setlocal
set "player=native\windows-player"
set "probe=%player%\tests\display-core-probe"
set "out=build-display-core\hmd-worker"
if not exist "%out%" mkdir "%out%"
if errorlevel 1 exit /b %errorlevel%
set "flags=/nologo /O2 /MT /EHsc /std:c++17 /utf-8 /DUNICODE /D_UNICODE /DNOMINMAX /DWIN32_LEAN_AND_MEAN /D_WIN32_WINNT=0x0A00"
set "includes=/I%player%\include /Ivendor-source\windows-native"
cl %flags% %includes% /Fo:%out%\ /Fe:%out%\Action75WallpaperWorker.exe %player%\src\NativePluginWorker.cpp %player%\src\NativePluginWorker12.cpp d3d11.lib d3d12.lib dxgi.lib advapi32.lib shell32.lib ole32.lib /link /SUBSYSTEM:WINDOWS /MANIFEST:EMBED /MANIFESTINPUT:%player%\cmake\ProcessHeap.manifest
if errorlevel 1 exit /b %errorlevel%
cl %flags% %includes% /Fo:%out%\ /Fe:%out%\Action75HmdBlackHoleProbe.exe %probe%\HmdBlackHoleProbe.cpp %player%\src\HmdOutput.cpp %player%\src\MonitorSelection.cpp %player%\src\NativePluginSession.cpp %player%\src\RawKeyboard.cpp %player%\src\PlayerConfig.cpp d3d11.lib d3d12.lib dxgi.lib advapi32.lib userenv.lib user32.lib setupapi.lib windowsapp.lib ole32.lib
exit /b %errorlevel%
