# 复用 D3D12 Worker 的黑洞原型

从仓库根目录，在 **x64 Native Tools Command Prompt** 中构建。脚本从仓库现有的 Worker 源码构建 `Action75WallpaperWorker.exe`，并将它放在探针旁边。探针加载 `wallpapers-native/black-hole` 中已附带的原生壁纸包。

```bat
native\windows-player\tests\display-core-probe\build-hmd-probe.cmd
build-display-core\hmd-worker\Action75HmdBlackHoleProbe.exe --offscreen "%CD%"
build-display-core\hmd-worker\Action75HmdBlackHoleProbe.exe --hmd "%CD%"
build-display-core\hmd-worker\Action75HmdBlackHoleProbe.exe --hmd-bright-keys "%CD%"
```

`--offscreen` 在 Action75 所接的 GPU 上验证 Worker 交接。测试暂时隐藏背景和黑洞主体，单独回读一次按键前后的 64×64 区域，确认 Worker 画出了星光。`--hmd` 需要副屏已被 Windows 识别为 HMD 或专用显示器，并可能需要管理员权限。它用默认黑洞外观运行约 55 秒，复用仓库的 Action75 键盘筛选和坐标校准。`--hmd-bright-keys` 使用更暗的背景、更亮的星光和无透镜偏移的画面，便于确认按键位置。实屏测试在启动 5 秒后发送一次合成测试事件。日志分别记录合成和物理按键数量。

`HmdOutput` 用 D3D12 打开 Display.Core 的 RGBA8 扫描缓冲区。Worker 的共享渲染纹理由 GPU `CopyResource` 复制一次，fence 协调 Worker 写入和输出读取。路径没有 CPU 像素回读。原型优先使用每次翻转的信号确认缓冲区可复用。驱动接受提交但不发出该信号时，原型改用垂直空白 fence 限制提交速率，轮换三个缓冲区。此模式能报告提交成功和垂直空白，逐帧上屏和缓冲区退役仍缺少硬件确认。
