# AndroidTool

AndroidTool 是一个使用 Rust + egui 编写的便携式 Android 工具台。

项目仓库：[TouMingQAQ/AndroidTool](https://github.com/TouMingQAQ/AndroidTool)。

> **第三方项目声明：** 本项目的高性能投屏基于 [Genymobile/scrcpy](https://github.com/Genymobile/scrcpy)，
> Windows 便携包随附官方 scrcpy 5.0.1 可执行文件、服务端及所需依赖。
> scrcpy 采用 Apache License 2.0；AndroidTool 并非 scrcpy 官方客户端，也不代表其维护团队。
> 详细来源、版本与许可见 [第三方说明](THIRD_PARTY.md)。

应用图标采用安卓机器人与终端提示符组合，统一用于窗口、任务栏、EXE 和应用侧栏。
图标源文件位于 `assets/androidtool.svg`，PNG 与多尺寸 ICO 同目录提供。
安装 Pillow 后运行 `python scripts/generate-icon.py` 可重新生成；正常 Rust 构建无需 Python。

## 功能

- 多设备管理、USB / 无线 ADB 设备自动发现
- 基于官方 scrcpy 的 H.264 低延迟投屏与鼠标键盘控制，保留内嵌 ADB 截图兼容模式
- Logcat 使用统一应用选择器，自动扫描前台和运行中的包名；默认自动选择前台应用，也可从列表固定选择、搜索或输入包名（含子进程），支持全部日志、级别/关键字过滤和导入导出
- APK / AAB 统一安装器，AAB 自动通过 bundletool 转换后安装；支持多组 keystore 签名配置，每个 AAB 执行前选择密钥
- APKS 安装、AAB 仅转换、顺序安装队列与转换产物保留
- 文件拖拽、设备文件浏览、上传和下载
- Xbox / PlayStation / Nintendo / 通用手柄识别，通过 UHID 向 Android 发送原生手柄输入
- 虚拟手柄、双摇杆、方向键与扳机，支持按住和组合键
- 设置页支持深色、浅色和跟随系统主题，主题选择自动保存
- 配置与转换产物保存在程序安装目录，便于便携使用

## 快速开始

1. 获取 Windows ZIP 便携包并解压到可写目录，或运行 Setup 安装包；不要只复制主程序 EXE。
2. 运行 `androidtool.exe`，在 Android 设备上启用开发者选项和 USB 调试，并确认授权。
3. 从顶部设备选择器选择在线设备，再进入投屏、Logcat、安装器或文件工作区。
4. 如需无线连接，Android 11+ 可在“无线调试”中通过二维码或配对码配对。
5. 安装 AAB 前，在设置页下载或指定 bundletool 和 Java 17+；仅安装 APK 不需要这两项。

Windows ZIP 便携包和 Setup 安装包均包含 ADB 与 scrcpy，但不预装 Java 和 bundletool。Setup 默认安装到当前用户的 `%LOCALAPPDATA%\Programs\AndroidTool`，无需管理员权限，提供开始菜单入口、可选桌面快捷方式和卸载入口。卸载不主动删除个人配置、下载工具和缓存。未发布二进制包时，可按下文自行构建。

推送版本标签（例如 `v0.0.1`，需与 Cargo 版本一致）后，GitHub Actions 会在 Windows 上测试并构建，随后发布 ZIP、Setup EXE 及各自的 SHA-256 文件到 GitHub Releases；也可以手动运行工作流获取构建产物。

## 开发与构建

源码构建需要支持 Rust 2024 edition 的稳定版 Rust；Windows 需要 MSVC 工具链与 Visual C++ Build Tools。
仓库的 `.cargo/config.toml` 默认使用 rsproxy.cn 镜像，无法访问时可按本机网络调整 Cargo 源配置。

```bash
git clone https://github.com/TouMingQAQ/AndroidTool.git
cd AndroidTool
cargo test --locked
```

运行开发版：

```bash
cargo run
```

ADB 路径留空时，优先使用可执行文件旁的 `platform-tools/adb`，其次查找同目录的 ADB 和系统
PATH。设置页可手动指定其他 ADB，也可点击“使用内置 ADB”清除旧路径并切回随包版本。
相对 ADB 路径以可执行文件所在目录为基准，不依赖启动时的工作目录。

Windows：

```powershell
# 下载官方 ADB 和 scrcpy，并构建便携 ZIP
powershell -ExecutionPolicy Bypass -File .\scripts\package-windows.ps1

# 同时生成 ZIP 和 Setup 安装包（额外需要 Inno Setup 6.3+）
powershell -ExecutionPolicy Bypass -File .\scripts\package-windows.ps1 -BuildInstaller
# 自定义编译器位置时增加：-IsccPath 'C:\path\to\ISCC.exe'
```

产物位于 `dist/`，命名为 `AndroidTool-v<版本>-windows-x64.zip` 和 `AndroidTool-v<版本>-windows-x64-setup.exe`，附带 `.sha256` 校验文件。ZIP 解压后运行其中的 `androidtool.exe`。请分发整个文件夹或 ZIP，不能只复制
EXE；包内保留 ADB 和 scrcpy 的依赖 DLL、许可文件与版本信息。构建需要 Rust MSVC 工具链及 Visual C++
Build Tools（请在开发者 PowerShell 中运行）。脚本不会打包个人设置、缓存或转换产物。

只准备项目内 ADB 和 scrcpy（之后 `cargo run` / `cargo build` 自动复制到可执行文件旁）：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\package-windows.ps1 -PrepareOnly
cargo run
```

下载缓存在 `vendor/windows/platform-tools/`，默认复用现有副本。需要固定版本时可使用
`-PlatformToolsUrl` 和 `-Sha256` 指定官方归档和预期哈希；下载归档的 SHA256 记录在
`vendor/windows/platform-tools.sha256`。这份记录本身不是独立的真实性校验。
Windows 构建脚本还下载固定版本 scrcpy 5.0.1，并校验其官方发布的 SHA-256；已缓存的副本会复用。

注意：内置的是 ADB 客户端；后台 ADB 服务默认仍与其他 Android 工具共享，不会自动终止或
重启其他工具的服务。若遇到 mDNS 不可用或版本冲突，请按无线配对章节排查。

macOS：

```bash
cargo build --release
./target/release/androidtool
```

macOS 打包为 `.app` 后，默认配置路径位于 `AndroidTool.app/Contents/MacOS/`。如果应用安装在
只读目录，请将应用移动到具有写权限的位置。
macOS / Linux 使用高性能投屏时，需要另行安装官方 scrcpy 并确保它在 PATH 中；未安装时可切换截图兼容模式。

## 使用

### AAB 签名密钥

在设置页的“AAB 签名密钥”中添加多组配置，填写名称、keystore 文件（JKS / PKCS12）和 Key alias。
文件位于程序目录内时保存相对路径；不会复制或打包你的密钥文件。
Keystore 密码和密钥密码仅在当前运行会话内保留，不写入配置文件；密钥密码留空时使用 Keystore 密码。
重启或重新加载配置后需重新输入密码。

拖入或选择 AAB 后，队列会在转换前弹出密钥选择确认；“仅转换”和“转换并安装”均使用所选密钥签名。
可进入“管理密钥”添加、修改配置，再点击“返回待签名任务”继续；取消仅移除当前任务。
APK / APKS 已签名，不会重新签名，也不需要选择密钥。

bundletool 使用短期密码文件读取密码，不将密码放入命令行，调用结束后清理文件；Windows 文件句柄关闭时自动删除。
日志对密码做脱敏处理。密码仍存在于应用和子进程内存中，请只在可信电脑上操作。
要覆盖设备上已有应用，必须使用与其签名一致的应用签名密钥；Google Play 的上传密钥不一定是实际分发的应用签名密钥。
签名不一致时不能覆盖安装，工具不会自动卸载应用或删除应用数据。

### 常用操作

1. 完整 Windows 便携包默认使用内置 ADB，无需另外安装 SDK。旧配置可在设置页点击“使用内置 ADB”；缺少 ADB 时可点击“下载 ADB”。
2. 连接设备并在手机上确认 USB 调试授权，然后在顶部设备选择器切换目标设备。
3. 将 APK、AAB 或 APKS 拖入窗口，或从统一安装器选择文件。
4. 在 Logcat 页筛选、暂停、导入或导出日志；在文件工作区上传和下载设备文件。
5. 设置修改后会自动保存，配置会在编辑约 900 ms 后写入安装目录，也可点击“立即保存配置”。

## 投屏

Windows 的“实时投屏”页默认将官方 scrcpy 的原生视频窗口内嵌到页面，使用 AndroidTool 当前设备和 ADB，无需单独安装或手动打开 scrcpy。画面仍由 scrcpy / SDL 解码与渲染，不经截图、PNG 或 egui 纹理中转；点击画面后可使用原生鼠标键盘控制。内嵌区域随窗口和 DPI 缩放调整；设备选择弹层或配对窗口打开时暂时隐藏视频，避免覆盖弹层。可点击“切换独立窗口”回退；macOS / Linux 暂时使用独立窗口。

离开投屏页或切换设备会结束当前进程，可在页面停止、重新启动或应用设置。视频使用 H.264，默认帧率上限 60 FPS、最长边 1920、码率 8 Mbps，不转发音频。较慢的无线网络可尝试 1280 像素和 4 Mbps，或改用 USB。静止画面可能不产生新帧，实际帧率还受设备编码和网络限制。内嵌是窗口级整合，scrcpy 仍是包内单独的子进程，并非静态链接进 EXE。

横竖屏切换使用等比留边显示：竖屏两侧留边，横屏按区域宽高自动缩放并居中，不拉伸、不裁剪。
首帧和旋转导致 scrcpy 自行调整窗口时，宿主会校正实际窗口位置和尺寸，确保画面保持在页面可视区域内。

scrcpy 与截图兼容模式共用“左侧画面、右侧设备控制”的布局。右侧“显示手柄 / 隐藏手柄”按钮用于开关画面内的半透明悬浮手柄，拖动手柄左上角图标可调整位置。悬浮手柄不缩小视频，使用独立透明宿主窗口覆盖原生视频表面；离开投屏页或打开弹层时自动隐藏。

虚拟手柄和实体手柄通过 scrcpy 的 UHID 控制通道注册为 Android 原生手柄，不模拟触屏。需要在手柄页或悬浮面板点击连接，目标设备须支持并允许访问 /dev/uhid，游戏也须支持手柄输入。

投屏页可切换回原先的内嵌截图兼容模式。该模式仍通过 ADB 操作，受截图速度限制，无法达到 scrcpy 的低延迟。手柄连接不依赖投屏窗口。

快捷键 `Ctrl+1` 至 `Ctrl+7`（macOS 为 `Command+1` 至 `Command+7`）可切换页面。

## 无线配对

手机和电脑连接同一局域网，在 Android 11 及以上设备的开发者选项中启用“无线调试”。

- 在设备总览点击“扫码配对”后，电脑会立即等待扫描。使用手机“无线调试”中的“使用二维码配对设备”扫描电脑二维码，保持弹窗打开直到连接成功。
- 使用配对码时，填写手机配对弹窗显示的 IP、配对端口和 6 位配对码，再点击“配对并连接”。IP 输入框也可直接粘贴 `IP:端口`。
- 连接端口留空时，配对成功后自动发现目标手机的连接服务。局域网屏蔽 mDNS 时，可手动填写手机“无线调试”主页显示的连接端口。
- 配对端口与连接端口是两个不同端口；无线调试连接端口不能固定填 `5555`。已经配对的设备只需填写主页的 IP 和连接端口，点击“仅连接”。
- 配对、连接或服务发现失败时，弹窗显示 ADB 返回的错误；停止操作或关闭弹窗会取消正在执行的无线 ADB 命令。
- 如果提示 mDNS 不可用，请先关闭可能使用 ADB 的 Android Studio、Unity 等工具，再用应用所选的 ADB 执行 `adb kill-server` 和 `adb start-server`，并检查 `adb mdns check`。重启服务会暂时断开其他 ADB 连接。仍无法发现时，请检查防火墙和路由器的 mDNS（UDP 5353）设置，或手动填写连接端口。
- 配对窗口也提供手动重启入口：停止等待，勾选允许重启后点击“使用当前 ADB 重启后台服务”，检查通过后重新等待扫描。服务发现连续失败 3 次会停止等待并提示排查，不会无限重试或自动中断其他工具。

## 原生手柄

AndroidTool 使用 gilrs 识别 Xbox、DualSense / DualShock、Switch Pro / Joy-Con 与通用手柄。
在“手柄控制”页或投屏悬浮面板连接手柄后，实体和虚拟输入均作为标准 HID 报告发送。USB 和无线 ADB 均可使用；切换设备时需要重新连接。若设备不支持 UHID，界面会提示错误，不回退为触屏。

LT / RT 同时上报模拟扳机轴和原生 L2 / R2 按键（半程阈值），兼容读取轴或按键的游戏。虚拟扳机为按住全按、松开归零；实体扳机保留模拟行程，并在界面高亮显示。

虚拟手柄参考 [TouMingQAQ/dsh-android-tool](https://github.com/TouMingQAQ/dsh-android-tool) 的手柄面板布局，以 egui 矢量控件重新绘制。可自动识别或手动
切换 Xbox、PlayStation、Nintendo Switch 外观；外观切换不改变标准按键位置。实体手柄按下的
按钮与双摇杆状态会同步高亮，鼠标可操作按钮、扳机和独立的 L3 / R3 按钮，拖动摇杆后松开自动回中。


## AAB 安装

AAB 安装需要 Java 17+。AndroidTool 会调用 bundletool 构建 universal APK，再通过 ADB
安装到当前目标设备。转换后的 APK 也会保留在安装目录的 `converted/` 中。

设置页可以分别下载 ADB、bundletool 和 Eclipse Temurin Java 运行时。bundletool 可从官方发布版本中选择；Java 支持选择 17、21、25 LTS 及对应的具体发行版本。先点击“获取版本”，选择版本后点击“下载”。三个工具各自使用独立进程，可分别取消，成功后自动更新工具路径。Java 下载功能目前支持官方 ZIP 发行包；macOS / Linux 的 tar.gz 发行包请手动安装并指定路径。

下载代理支持 `http://主机:端口` 的 HTTP CONNECT 或混合代理端口，留空为直连；该设置只影响工具下载和版本查询，不影响 ADB 连接。代理地址明文保存在配置文件中，请勿填写敏感凭据。Java 下载校验官方 SHA-256；bundletool 在上游提供 SHA-256 时校验，ADB 下载使用 HTTPS 并验证可执行文件。

bundletool 官方 API 不可用时，会尝试 GitHub 的官方发布订阅源；两者均不可用时提供内置的已知官方版本列表（不是在线最新列表），并显示失败原因。订阅源不提供文件校验值，仅内置的已知版本有固定 SHA-256。所有下载地址均为 GitHub 官方发布地址；如果 GitHub 下载不可达，仍需设置可用代理。

## 便携文件

- `androidtool.settings.json`：工具路径、帧率、日志和手柄配置
- `platform-tools/`：便携包内置的 ADB
- `scrcpy/`：Windows 包内的官方 scrcpy 可执行文件、服务端、依赖及许可文件
- `tools/bundletool/<版本>/`：下载的 bundletool 版本
- `tools/jdk/<版本>/`：下载的 Java 运行时版本
- `tools/adb/`：应用内下载的 ADB
- `cache/`：AAB 转换临时文件
- `converted/`：转换后的 universal APK

下载完成后，安装目录内的工具路径以相对路径保存；移动便携目录时请一并保留配置文件与 `tools/`。重复下载会先询问是否覆盖，并在下载和校验成功后替换旧目录。旧版 `tools/downloads/` 目录及配置仍可继续使用。

升级时将旧版 `droiddeck.settings.json` 放在新程序旁即可继续读取；新设置保存为
`androidtool.settings.json`，两个文件同时存在时以新文件为准。

## 已知限制

- 高性能视频内嵌目前仅支持 Windows；其他系统使用独立 scrcpy 窗口。默认不转发音频，原生子窗口不支持 egui 浮层直接叠加。截图兼容模式的实际帧率通常远低于设置值。
- Logcat 使用前台 Activity 与周期性进程列表映射包名；极短生命周期进程、无法查询的进程及不含包名的导入日志可能无法归属。可切换“全部日志”查看。
- 受 DRM 保护的画面、部分系统弹窗和安全输入界面可能无法截图或注入触摸。
- “发送文本”使用 Android `input text`，默认仅保证 ASCII 文本输入。

## 上游项目与致谢

| 项目 | 用途 | 上游仓库 / 官方站点 |
| --- | --- | --- |
| scrcpy | H.264 投屏、原生控制通道及派生的 HID 报告描述符 | [Genymobile/scrcpy](https://github.com/Genymobile/scrcpy) |
| Android platform-tools | ADB 设备连接、文件与调试命令 | [Android Developers](https://developer.android.com/tools/releases/platform-tools) |
| bundletool | AAB / APKS 处理 | [google/bundletool](https://github.com/google/bundletool) |
| Eclipse Temurin | 可选 Java 运行时下载 | [Adoptium](https://adoptium.net/) |
| egui / eframe | Rust 桌面界面 | [emilk/egui](https://github.com/emilk/egui) |
| gilrs | 实体手柄识别 | [gilrs-project/gilrs](https://github.com/gilrs-project/gilrs) |
| dsh-android-tool | 手柄面板布局参考 | [TouMingQAQ/dsh-android-tool](https://github.com/TouMingQAQ/dsh-android-tool) |

AndroidTool 通过独立进程调用官方 scrcpy，Windows 内嵌是原生窗口级整合，不是把 scrcpy 静态链接进 EXE。
手柄 HID 报告描述符基于 scrcpy 调整，相关 Apache-2.0 许可保存在 `assets/scrcpy-APACHE-2.0.txt`。
分发便携包时须保留 scrcpy、ADB 和依赖库的许可及声明；第三方组件仍受各自许可约束，详见 [THIRD_PARTY.md](THIRD_PARTY.md)。

## 源码目录

```text
src/          Rust 应用源码
assets/       应用图标、HID 描述符和第三方许可
scripts/      Windows 打包、图标生成脚本
build.rs      Windows 图标资源与便携工具复制
vendor/       本地下载的官方工具（不提交）
dist/         Windows 便携包产物（不提交）
```

提交问题时请说明系统、设备型号、连接方式与复现步骤。日志和设置可能包含设备序列号、包名、文件路径及代理凭据，公开前请先脱敏。
