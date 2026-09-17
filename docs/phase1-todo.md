# Phase 1 TODO：Launcher

目标：完成一个可搜索并启动本机应用的最小 Launcher。

## 已开始

- [x] 建立 Rust + Slint 工程入口和 UI 编译流程
- [x] 定义统一 `SearchResult` 与 `Action::Launch`
- [x] 实现 Search Core：空查询、精确、前缀、包含和顺序模糊匹配
- [x] 实现 Windows Start Menu 的 `.lnk` / `.exe` 应用发现
- [x] 实现跨平台启动动作适配层
- [x] 接入 Windows 全局 Hotkey：默认 `Alt+Space`
- [x] 增加 JSON 持久化配置：快捷键、窗口宽度、结果数、主题、语言、启动显示和失焦隐藏
- [x] 应用配置中的窗口宽度和最大结果数，并对配置值做边界校验
- [x] 为应用记录增加稳定 Identity、别名、来源和独立启动目标字段
- [x] 搜索同时匹配应用标题与别名，并按 Identity 合并重复记录
- [x] 解析 `.lnk` 的真实 target、arguments 和 working directory
- [x] 按来源优先级合并记录，并对重复标题进行去重
- [ ] 接入 Windows Execution Alias（WindowsApps）数据源（当前明确不启用）
- [x] 扫描用户桌面和公共桌面的 `.lnk` / `.url` / `.exe`
- [x] 接入 Windows AppsFolder 数据源
- [x] 接入 Windows Package Catalog 数据源
- [x] 使用 AppsFolder 本地化名称作为 Package 主标题，并保留包名 Alias

## 下一步

- [x] 将窗口改为启动器面板：圆角深色样式、快捷键隐藏/唤起、启动时按实际尺寸居中偏上
- [x] 隐藏面板后保持进程后台常驻
- [x] 将 Windows 热键、窗口激活和屏幕定位隔离到 PlatformIntegration
- [x] 默认使用 Slint Skia OpenGL，支持配置为软件渲染，并在 Skia 初始化失败时自动 fallback
- [x] Windows 平台关闭启动器窗口的 DWM 过渡动画
- [x] 启用透明无边框窗口，避免圆角面板出现方角背景
- [x] 增加失焦自动隐藏
- [x] 完善键盘导航：上下选择、Enter 启动、Esc 隐藏
- [x] 将搜索放入后台线程，并加入 query_id 防止旧结果覆盖新查询
- [x] 解析 `.lnk` 的 AppUserModelId，并用它辅助确定逻辑 Identity
- [x] 改用原生 Windows API 替代 Package / AppsFolder 的 PowerShell 枚举
- [x] 增加应用图标
  - [x] 已接入 Windows `SHGetFileInfoW`，对 `shell:AppsFolder\...` 使用 PIDL 解析；请求 32×32 并转换为 RGBA 后在 Slint 中显示，不放大低分辨率图标。
  - [x] 将图标提取迁移到后台线程；当前结果查询线程负责提取，后续可继续按 application identity 缓存。
- [x] 为 Result / Action 增加启动错误状态与用户可见反馈
- [x] 补充 App Provider、Search Core、Launcher 的测试覆盖
- [x] 评估全局热键依赖与 Windows 原生窗口生命周期方案（见开发文档）

## Phase 1 验收标准

1. 启动程序后能看到搜索界面。
2. 输入应用名能返回排序后的应用结果。
3. 点击结果能启动对应应用。
4. 核心搜索逻辑有自动化测试。
5. Provider、Search Core、Result、Action、UI 之间边界清晰，后续可接入文件 Provider。
