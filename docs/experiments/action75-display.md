# Action75 显示输出实验记录

基线为 Action75 Wallpaper v0.4.2。实机使用 Intel Arc 130V、Windows 11 Pro 和 USB-C 连接的 Action75 1920 × 720 副屏。下表记录的是这套组合的观察结果。

| 项目 | 观察结果 |
| --- | --- |
| 专用显示输出 | Windows 将 Action75 枚举为 HeadMounted 后，管理员进程通过 Display.Core 接管目标。在 1920 × 720、60 Hz 下，实屏显示了纯色和按填充模式裁切的图片。 |
| 原始 EDID 与分类缓存 | 移除 EDID 覆写、保留 HMD 分类缓存后，重启显卡设备，Action75 仍被枚举为 HeadMounted。随后清除缓存并重启显卡设备，恢复为普通扩展，EDID 仍与原始数据一致。 |
| 局部恢复 | 只重启 Action75 的显示器节点，设备仍以 HMD 枚举，但未恢复首帧确认。重新插接曾恢复普通扩展。 |
| Intel IGCL | 在本机对 Action75 和作为对照的 HDMI 显示器调用 EDID 读取，均未成功。尝试写入原始 EDID 也未观察到 Windows 重新枚举。 |
| DDC/CI | 普通扩展下，亮度可在显示器报告的范围内调整并恢复。写入电源模式 0xD6=4 后副屏离线，软件唤醒未成功，最终由重新插接恢复。 |
| 黑洞输出 | D3D12 Worker 与输出共享纹理，GPU 拷贝一次后提交。实屏确认黑洞和按键星光。首帧可被 Windows 接受但迟迟没有逐帧翻转确认。将备用垂直刷新信号的初始化移到首帧提交后，并保留其 Windows 对象后，黑洞恢复持续播放。这两项调整一起验证。 |
| 系统快捷键 | 实机验证左 Alt、Win+E、Alt+Tab 和 Ctrl+Shift+Esc 可触发壁纸动效，同时保留系统动作。GameInput 参与了探针测试，正式输入路径使用 Raw Input 和低级键盘钩子。 |

复测枚举、模式查询和测试画面可使用[独立 Display.Core 探针](../../native/windows-player/tests/display-core-probe/README.md)。记录结果时分别注明 Windows 接受提交、刷新信号推进和实屏可见性。更改显示器分类和电源状态的测试应在可恢复连接的实机上进行。

微软文档说明了 [专用显示器 EDID 扩展](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/specialized-monitors-edid-extension)、[自定义合成器](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/specialized-monitors-compositor)及[垂直刷新同步](https://learn.microsoft.com/en-us/uwp/api/windows.devices.display.core.displaydevice.createperiodicfence)。
