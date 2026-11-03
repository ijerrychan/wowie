# Windows 控制面板与设置索引

## 上游与准确获取途径

| 数据 | 当前来源与维护方式 | 准确性边界 |
| --- | --- | --- |
| 设置页 URI、版本范围、别名与英文 | 微软 [PowerToys Command Palette WindowsSettings](https://github.com/microsoft/PowerToys/tree/main/src/modules/cmdpal/ext/Microsoft.CmdPal.Ext.WindowsSettings) 的 JSON、schema 和英文 RESX；固定 commit 随仓库分发 | 是维护目录，不是本机可用性证明，也不保证立即包含每个 Windows 新页面 |
| 简体中文 | 官方 PowerToys 发布程序内嵌资源，通过既有导入器提取；记录原发布版本、二进制 SHA-256 和已核对的语言流偏移 | 上游源码只有中性英文 RESX，不能假定有可直接下载的中文 RESX，不能猜测新版本偏移 |
| 本机控制面板主项与任务 | 运行时枚举 ControlPanelFolder 与 All Tasks，读取 Shell 标题、属性、关键字及 PIDL | Shell 枚举反映当前机器暴露的项，包含第三方 applet；不等同于所有 Windows 版本的全集 |
| MMC 工具和图标 | 本机 System32 文件与 `.msc` 的 VisualAttributes/Icon | 文件存在与资源可读较可靠，最终权限、组件和页面运行状态仍需按需检查 |
| 兼容性修正 | 微软设置 URI 文档、已移除功能说明，以及本项目针对已确认问题的修正 | 由项目维护者维护，不能完全交给目录更新替代 |

微软的 [Settings URI reference](https://learn.microsoft.com/en-us/windows/apps/develop/launch/launch-settings)
是核对 URI、版本及硬件前提的官方资料，但不是返回“本机所有可用页面”的 API，
也不提供与本项目完全对应的多语言搜索目录。协议能启动不代表 URI 对应页存在，
有的不可用页会跳回首页或父页。因此目前采用维护目录、版本/策略/设备过滤相结合的方案。

控制面板的 [IShellFolder::EnumObjects](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-ishellfolder-enumobjects)
是正式 Shell 接口。[Canonical names](https://learn.microsoft.com/en-us/windows/win32/shell/controlpanel-canonical-names)
文档可核对不随语言变化的名称，但只列 Windows 内置项，部分条目描述的是旧版系统，
不能替代本机枚举。All Tasks 是借鉴 PowerToys 的 Shell namespace 用法，不应被描述为
微软承诺完整、稳定的设置页枚举 API。内部 Settings XML/DLL 同样不具备这一保证。

2026-11-01 已核对最新稳定版 [v0.102.3001](https://github.com/microsoft/PowerToys/releases/tag/v0.102.3001)，
目录固定到 `a00b2bce7403f0a48efe5cbb6671074e01f27824`，仍为 306 项。
目录、英文源与此前快照一致，因此继续复用 `v0.101.2362.0` 中已验证的中文资源。
准确版本与源文件哈希以 [`SOURCE.json`](../src/providers/windows_settings_data/SOURCE.json) 为准：
`catalog` 记录本次目录来源，`localizations` 保留译文的真实来源，不把旧译文标成新版本提取结果。

## 构建前自动更新

普通 `cargo build` 和 `cargo test` 继续使用仓库快照，不在 `build.rs` 中联网或修改源码。
需要追踪稳定版时使用可选的构建入口（Python 3.10+、PowerShell、Cargo）：

```powershell
# 更新到最新稳定版，校验通过后构建 release
pwsh -NoProfile -File tools/build.ps1 -UpdateWindowsSettings

# 指定上游版本并构建开发版
pwsh -NoProfile -File tools/build.ps1 -UpdateWindowsSettings -SettingsRef v0.102.3001 -Profile dev
```

更新器只访问 `microsoft/PowerToys` 的公开 GitHub API 与源码，先把 tag 解析成不可变
commit，再从同一 commit 获取 JSON、schema、英文 RESX 和 MIT 许可证；不下载或执行
发布程序，不安装 PowerToys。`latest` 明确选择最新稳定版，不追踪开发分支。
脚本也支持显式 ref/commit；不传 `--ref` 时只重新获取当前固定 commit。

只检查差异或单独更新：

```powershell
python -B tools/update_windows_settings.py --ref latest --check
python -B tools/update_windows_settings.py --ref latest
```

`--check` 不修改项目数据，退出码 0 表示一致、1 表示有更新、2 表示失败。
需要离线更新时，可使用已经取得的 PowerToys 仓库根目录：

```powershell
python -B tools/update_windows_settings.py --source <PowerToys仓库根目录> --ref <版本标签> --revision <40位commit>
```

自动更新校验项目实际使用的资源键、版本数值、条目类型和支持的设置 URI 格式。
已有各语言译文必须齐全，且对应英文原文保持一致才可复用；新增键或英文语义变化会
停止更新，提示先用[原始资源导入器](../tools/import_powertoys_windows_settings.py)取得同版本的
官方译文，不自动造中文或静默改用英文。未使用的新资源键不影响目录的正常更新。

全部下载及验证完成后才准备替换，写入错误会尝试回滚已替换的文件；断电/进程强制终止
不具备跨文件事务保证，应检查 Git 差异后重跑。网络或校验失败会让带更新的构建停止，
原有仓库数据不变；普通 Cargo 构建仍可直接使用已有快照。更新会产生可审阅、可提交的
源文件差异，请将数据与 SOURCE.json 一同提交。源文件哈希用于追踪取得的字节，
不等同于独立签名验证。GitHub 限流或离线时可改用离线源码。

只有目录更新时运行 `cargo test --locked providers::windows_settings::` 验证资源与索引行为；
修改更新器时运行 `python -B tools/test_update_windows_settings.py`。无需为一次数据维护
重跑整套桌面检查或性能基准。构建入口不自动运行测试，也不会发布程序或创建远程 PR。

## 数据来源与合并

设置页沿用仓库内已导入、带版本与许可证记录的 PowerToys Command Palette
目录及中英文资源。参考 [PowerToys Run](https://github.com/microsoft/PowerToys/blob/main/src/modules/launcher/Plugins/Microsoft.PowerToys.Run.Plugin.WindowsSettings/Main.cs)
和 [Flow Launcher](https://github.com/Flow-Launcher/Flow.Launcher/blob/dev/Plugins/Flow.Launcher.Plugin.WindowsSettings/Main.cs)
的维护目录、按 OS build 过滤和资源翻译方案。没有引入运行时下载或外部进程扫描。

控制面板主项与 All Tasks 子任务由 Shell 枚举，保留本机安装的第三方项、
系统本地化标题、关键字和任务启动 PIDL。目录中的控制面板记录用于补充
多语言别名，不凭静态表虚构已安装的传统 applet。MMC 工具必须在 System32
中实际存在。

一次 discovery 只读取一次设置宿主安装状态、策略和应用图标路径。设置页
按目录的引入/废弃 build 过滤，继续保留 Fonts、Cortana、24H2 Mixed Reality
的现有兼容性修正。机器和用户的 SettingsPageVisibility 限制同时生效。

设备探测区分支持、不支持和未知。明确不支持才排除，API 调用失败或尚未
实现的探测返回未知，避免整组设置凭空消失。Surface Dial 使用 HID usage
探测，触控板探测遇到读取失败不再误判为没有设备。无法取得 OS build 时
保留目录候选，不把失败当作系统版本过低。

原方案把 AllSystemSettings XML 中的 PolicyIds/DeepLink 或 DLL 中的 URI
字面量当作完整白名单。这些内部数据可能缺少入口页、使用不同的结构，
也可能包含已废弃内容。现已移除这种白名单及二进制字符串扫描。

合并键为不区分大小写的启动目标和保留原值的启动参数，不能用翻译标题
去重。Shell 结果在前，保持其已有身份和图标；同目标的目录标题、路径、
同义词并入别名。不同目标即便同名也独立保留。现有身份生成规则继续使用。
分层搜索支持中英文路径、末尾分隔符；中间空路径段不再意外扩大匹配范围。

## 图标

- 所有控制面板项都捕获绝对 PIDL 作为独立图标来源。普通 applet 的启动
  地址继续使用原解析名；All Tasks 继续通过 PIDL 启动。
- 设置优先使用本机应用 PNG，失败后尝试 Settings AppsFolder 图标。
- MMC 从 `.msc` 的 `VisualAttributes/Icon` 读取 File 和 Index，通过
  SystemIndexed 提取工具自己的 DLL 资源；限制文件大小，不执行控制台。
- 环境变量不区分大小写；命令先拆分可执行文件和参数，再展开路径变量，
  支持带引号及空格的路径。
- 结果与启动入口同步图标配置。共享应用合并在入口缺少图标元数据时保留
  已有结果图标。
- 原生提取失败时使用随程序打包的设置/控制面板 SVG，避免空图标。

## 验证

按[必要测试流程](开发文档.md#必要测试流程)选择范围。下面的本机集成测试只在修改设置发现、Shell/PIDL 或图标实现时按需执行，不是每次修改的必跑项。

```powershell
cargo fmt --check
cargo test --locked providers::windows_settings::
cargo test --locked platform::windows::settings::
cargo test providers::windows_settings::tests::installed_pages_and_icons -- --ignored --nocapture
```

单元测试覆盖 build 边界、已退休 URI、未知设备能力、宿主和策略限制、
重复目标合并、中英文路径、命令解析、MMC 资源及内置图标。
跨模块修改或发布前可用一次 `cargo test --locked --quiet` 替代上述局部测试；图标实现有变动时另选 `services::icon_service::`，不要重复执行局部与全量测试。
本机集成测试只读枚举，验证身份唯一、设置目标唯一、入口图标一致、任务
PIDL 和每条结果的非透明图标，不会启动任何设置页或修改系统设置。

2026-11-01 在 Windows build 26300 实测：428 个结果，其中 173 个设置入口、
198 个 All Tasks 任务，全部能解析出非全透明图标（包含必要时的类别兜底）。
结果数量会随系统版本、策略、设备、已安装工具和语言变化。

## 边界

维护目录不能保证覆盖未来 Windows 新增的全部页面，更新方法见
[数据来源说明](../src/providers/windows_settings_data/NOTICE.md)。未知探测
保留候选意味着少量受动态功能开关限制的页面可能跳转到父页；这与维护
目录方案的能力边界一致。build 单元测试覆盖 Windows 10/11 的数值边界，
本次没有在 Windows 10 实机运行。集成测试检查索引与图标，不验证每个入口
启动后的页面内容。切换 Wowie 语言不会翻译来自 Shell 的系统本地化任务。
