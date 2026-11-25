# Action75 专用显示器探针

这个独立程序用来验证 `Windows.Devices.Display.Core` 能否枚举、接管并向 Action75 的 1920×720 副屏提交一帧。它接受 Windows 已识别的 `HeadMounted` 或 `SpecialPurpose` 目标。

在仓库根目录打开 Visual Studio 的 **x64 Native Tools Command Prompt**。构建需要带 C++/WinRT 头文件的 Windows SDK。提交测试帧需要 Windows 11 的 `TryExecuteTask`。

```bat
mkdir build-display-core
cl /nologo /EHsc /std:c++17 /W4 /Fo:build-display-core\DisplayCoreProbe.obj /Fe:build-display-core\action75_display_core_probe.exe native\windows-player\tests\display-core-probe\DisplayCoreProbe.cpp d3d11.lib dxgi.lib windowsapp.lib windowscodecs.lib ole32.lib
build-display-core\action75_display_core_probe.exe --list
```

`--list` 只读取显示器类型、名称、原生分辨率和设备 ID。普通扩展屏显示为 `Standard`。探针按 Windows 报告的用途分类选择目标。系统提供「从桌面删除显示器」选项时，可以在 **设置 → 系统 → 显示 → 高级显示** 中选中 Action75，开启该选项，再用 `--list` 检查分类是否为 `SpecialPurpose`。

复制目标的完整设备 ID，包括其中的 `&` 字符，并保留引号。探针会尝试选择最接近 60 Hz 的 1920×720 模式，提交一帧纯绿色画面，等待两秒后释放应用持有的显示目标。以控制台的 `Display driver accepted test frame` 和实屏看到绿色作为两项验收。测试结束后可回到高级显示，关闭「从桌面删除显示器」以恢复扩展桌面。

```bat
build-display-core\action75_display_core_probe.exe --present "<monitor-id>"
build-display-core\action75_display_core_probe.exe --present "<monitor-id>" "<image-path>"
build-display-core\action75_display_core_probe.exe --present "<monitor-id>" "<image-path>" --fill-hold
```

传入图片路径时，探针使用 Windows Imaging Component 解码图片，保持比例居中显示 15 秒。在 Windows 11 Pro 上实测，普通进程申请目标被拒，提升为管理员后驱动接受了测试帧。

加上 `--fill-hold` 后，图片居中裁切并铺满输出。窗口保持占用显示目标，直到在窗口里按 Enter。

用 `--modes "<monitor-id>"` 读取当前输出模式并查询当前路径可见的候选模式。正在输出的 Action75 路径可读出实际分辨率、刷新率和链路格式，但驱动在该只读路径上返回了零个候选。图片结束后可用 `--acquire-modes "<monitor-id>"` 临时取得目标，在尚未设置参数的路径上枚举候选模式和链路格式。此命令不提交模式或图像。

- [微软对 HMD 与专用显示器 EDID 的定义](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/specialized-monitors-edid-extension)
- [微软自定义合成器指南](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/specialized-monitors-compositor)
