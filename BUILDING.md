# Action75 Wallpaper

面向 Action75 键盘屏的 Windows 互动壁纸软件。管理界面使用 Rust 和 egui，C++17 播放器负责本地渲染和按键交互。当前源码支持 D3D11 和 D3D12，播放器按壁纸包声明选择后端。D3D12 作品的接口与示例见[壁纸制作指南](skills/action75-wallpaper-author/references/native.md#d3d12-与-abi-2)。

## 使用

运行下载的 Windows x64 安装包，选择安装位置。首次使用先按 Win + P 选择「扩展」，再选择键盘屏和键盘。拔插或换接口后，可在首次设置或「设备与输出」刷新设备。1920 × 720 的副屏会标为键盘屏候选；选择其他分辨率时可以确认继续。
壁纸库单击即可切换。按住卡片拖到目标卡片左侧或右侧，看到插入线后松手即可排序。顺序在重启后保留，拖动期间壁纸继续播放。右侧前景和背景参数独立调整，高级参数默认折叠。关闭控制面板后壁纸继续运行；托盘右键可暂停、设置开机运行或完全退出。需要立即停止壁纸并释放鼠标时，按 Ctrl + Alt + Shift + Q 即可退出本次运行的控制台和播放器。

校准页提供拟合键位：确认后预览，可撤销，保存后生效。默认校准使用当前实机的 83 键参数。
设置可修改壁纸和日志目录，自动复制、校验并迁移；冲突不覆盖。迁移只处理壁纸包与本软件日志，其它文件保留。
日志默认在安装目录的 `logs`，每份最多 10 MB，共保留 10 份，不记录真实逐键输入。

## 软件更新

使用安装版打开控制台时会在后台检查正式发行版，持续打开时每 24 小时复查，也可在设置中手动检查。发现新版后点击“立即更新”，下载和校验期间可继续播放。安装交接时短暂停播，完成后重新打开控制台并恢复原壁纸。便携目录与其他安装实例独立。

更新源为 `blackwhite-poi/Action75-Wallpaper` 的 GitHub Releases。发布时让标签 `vX.Y.Z`、控制台 Cargo 版本、安装器版本和附件名 `Action75-Wallpaper-Setup-X.Y.Z-x64.exe` 一致。先在草稿中传完整安装包，再发布为 latest 正式 Release。客户端通过 HTTPS 获取 GitHub 提供的 SHA-256 和长度，校验通过才执行安装。草稿和预发行版不参与检查。

## 构建

需要 Windows、Visual Studio 2022 C++ 工具与 Windows SDK、Rust MSVC 工具链、CMake 3.24+。测试与打包脚本使用 Python 3.11+。

```powershell
python tools/restore-wallpaper-assets.py
python tools/release_build.py --test --output-dir artifacts/distribution-build
python tools/package-source.py --output-dir artifacts/source
$archive = (Get-ChildItem artifacts/source -Filter 'ACTION75-source-*.zip' -File).FullName
./examples/native-wallpapers/verify-sdk.ps1 -Archive $archive -OutputDirectory artifacts/author-builds -WallpaperAssets wallpapers-native -EffekseerArchive vendor-source/windows-native/EffekseerForCpp1.80.8.zip
python tools/stage-native-wallpapers.py --sdk-output artifacts/author-builds --output artifacts/distribution-wallpapers
./tools/package.ps1 -BuildReceipt artifacts/distribution-build/build.json -Wallpapers artifacts/distribution-wallpapers -Compiler 'C:/path/to/InnoSetup/ISCC.exe'
```

本地打包会把正式安装器写入 `release/`。在 GitHub Actions 手动运行 Windows checks 后，也可从该次运行的 `action75-wallpaper-installer` 下载正式安装器。

同步到 Gitee 时，在 GitHub 正式发布后运行 `python tools/prepare-gitee-update.py <安装包路径>`。工具用 GitHub 发行资产的大小和 SHA-256 核对本地安装包，并在同目录生成 `Action75-Wallpaper-Update.json`。把同一个 EXE 和这份清单上传到 Gitee 对应的正式发行标签。控制台在 GitHub 无法提供发布信息时从 Gitee 读取该清单，下载后仍校验长度与 SHA-256。

首次构建需下载 Cargo 锁定依赖、nlohmann JSON 3.12.0 和 Effekseer 1.80.8；原生依赖下载有固定 SHA-256 校验。打包使用 Inno Setup 的命令行编译器 ISCC。可通过上面的 `-Compiler` 指定安装路径。
内置壁纸均为 Windows x64 原生壁纸包。将 `wallpapers-native/<壁纸 ID>` 整个文件夹交给其他用户，即可通过控制台导入；设置、配色和图像资源随包交付。旧版用户保留同一壁纸 ID 和参数配置。

原生壁纸通过 [独立作者工程](examples/native-wallpapers/README.md) 构建并暂存。用 `examples/native-wallpapers/verify-sdk.ps1 -Archive <源码包> -Project <工程名>` 从源码包复建，再核对输出包的 ABI、导出和依赖。播放器构建目录中的 ABI1 参考 DLL 留给兼容测试。第三方 ABI1 和历史格式的作者包继续走原加载路径。

水彩材质的可编辑 HLSL 位于 `native/windows-player/materials/paintball`。修改后用 `python tools/compile-paintball-materials.py` 更新材质源，再构建 `atlas-packs` 生成 DX12 缓存及成品。
`tools/release-assets.json` 定义安装包和源码归档共同分发的文件。新增内置壁纸或资源时更新这份清单。
`prebuiltNativeWallpapers` 列出随仓库分发的预构建 D3D11 原生包。发行暂存从对应的壁纸目录读取这些包，并按 ABI 1 验证。其他原生内置壁纸继续使用独立构建的 ABI 2 产物。

`python tools/verify-release.py --receipt artifacts/distribution-build/build.json --compiler '<ISCC.exe 路径>'` 构建带独立身份的测试安装器，检查安装、重复安装、同 ID 壁纸保留、播放中卸载和正式安装登记。完整验收还会在隔离桌面导入壁纸，验证参数、目录重载和离屏负载。CI 使用 `--install-only` 完成安装生命周期检查。

断屏重连回归包含在 `./tools/build-player.ps1 -Test` 中。本地升级验收使用独立安装身份与文件更新源。安装 Python 依赖 `psutil` 和 `pywin32` 后，运行下列命令构建两套测试程序，再验证会话恢复。Windows checks 的手动构建使用已发布的上一版安装器进行升级和回退验收，通过后才上传候选安装器。

```powershell
python tools/verify-installed-update.py build --work artifacts/update-qa --wallpapers artifacts/distribution-wallpapers
python tools/verify-installed-update.py run --work artifacts/update-qa --playing
python tools/verify-installed-update.py run --work artifacts/update-qa
```

## 开发资料

- [当前项目架构图](docs/项目架构.svg)
- [壁纸制作 Agent 指南](docs/wallpaper-authoring/README.md)
- [产品约定](docs/product-decisions.md)
- [架构与证据](docs/architecture-baseline.md)
- [架构决策](docs/adr/README.md)
- [0.3.1 历史验收记录](docs/release-verification.md)

自有代码按 [Apache-2.0](LICENSE) 分发。字体、第三方组件与壁纸素材的授权见[许可范围](LICENSING.md)。
