# Windows 系统集成回归检查

## 自动化覆盖

本页是按影响范围选择的检查目录，不是每次修改都要执行的清单。日常流程见[必要测试流程](开发文档.md#必要测试流程)。默认测试已覆盖的项目无需再逐条重跑；只有修改相应系统行为、调查故障或验证发布兼容性时，才执行对应桌面检查。

| 场景 | 自动化覆盖 | 运行方式 |
| --- | --- | --- |
| 快捷键冲突后保留旧注册 | 临时占用 F24，更新失败后检查旧快捷键仍注册 | `cargo test platform::windows::tests::hotkey_update_conflict_restores_the_previous_registration -- --ignored --nocapture --test-threads=1` |
| 平台监听器退出与重复启动 | 两轮启动快捷键、托盘、目录、Shell 和包监听后统一停止 | `cargo test platform::windows::tests::platform_listeners_release_resources_across_restarts -- --ignored --nocapture --test-threads=1` |
| 深层目录变化通知 | 临时空目录先确认未触发，再新建深层文件确认收到通知；不附带性能采样 | `cargo test platform::windows::tests::custom_directory_notification_covers_deep_changes` |
| 损坏 `.lnk` | 固定截断 ShellLink 样本及偏移边界、ANSI、Unicode 和超大文件用例 | `cargo test platform::windows::shortcuts::tests -- --nocapture --test-threads=1` |
| 不可解析配置 | 固定的截断 JSON 样本确保读取失败时原文件保持不变 | `cargo test config::tests::invalid_existing_config_is_preserved_and_valid_writes_replace_it -- --nocapture` |
| renderer 配置规范化 | 确认支持的 renderer 值在配置加载后保留 | `cargo test config::tests::supported_renderers_are_preserved_by_normalization` |
| 紧凑窗口反复呼出 | 实际 Slint UI 软件渲染；基线语言/缩放完整扫描选中边界，其他语言与分数 DPI 稀疏采样，覆盖分组、行高、结果数量、选中项、小窗口和尺寸变化，断言无 callback/instantiation chain 日志 | `cargo test ui::layout_tests::launcher_layout_settles_when_reopened` |
| Windows 过渡背景 | 隐藏原生窗口验证独立主题、主题切换、尺寸增大及绘制期间擦除不会覆盖已渲染内容 | `cargo test platform::windows::window_background::tests::` |

测试样本位于 [`tests/fixtures/windows`](../tests/fixtures/windows/README.md)。快捷键与平台监听器的桌面集成测试默认忽略，它们会短暂注册系统级热键、托盘图标和 Shell 监听器，单独串行运行。只读系统项枚举也默认忽略；目录通知、无头布局与隐藏窗口像素测试属于默认测试集，不需要运行 `--ignored`。

## 手工桌面检查清单
- [ ] **系统项存在性和图标**（修改设置发现、Shell/PIDL 或原生图标时）：运行 `cargo test installed_pages_and_icons -- --ignored --nocapture --test-threads=1`；验证本机索引 ID 与设置目标唯一、入口图标一致、设置与控制面板图标可解码（允许类别兜底）。发行兼容性验证再覆盖 Windows 10/11、Home/Pro 和不同硬件；确认不存在的 MMC 文件和明确不支持的硬件页面被过滤，未知探测保留候选，hide/showonly 策略生效。设置页不再要求 XML/DLL 字符串证据。详见[设置索引](Windows设置索引.md)。

- [ ] **呼出与展开底色**：使用 FemtoVG，分别固定暗色／亮色，反复用快捷键呼出和隐藏窗口；紧凑模式下输入查询加载结果、清空查询并再次呼出。确认暗色不闪白、亮色展开不闪黑；切换主题后立即重复。另用 software 与可用的其他 GPU renderer 检查。

仅选择与改动有关的项目在真实 Windows 桌面完成；跨版本、显示器和 renderer 矩阵留给对应兼容性问题或发布验证。记录 Windows 版本、显示器数量与缩放比例、所选 renderer、结果和日期。

- [ ] **外观页字体加载**：冷启动后立即打开“外观”页，确认字体扫描期间窗口仍可响应；加载完成后字体列表补全，当前选择不变。扫描期间切换其他分类、修改字体或关闭设置窗口，确认完成回调不切换页面、不覆盖编辑、不重新打开窗口；再次进入“外观”页使用已缓存的完整字体列表。

- [ ] **DPI 与多显示器定位**：分别在单显示器、不同缩放率的双显示器和负坐标副屏上打开启动器、设置窗与上下文菜单；确认窗口完整落在目标屏幕工作区内，缩放变化后尺寸合理。
- [ ] **原生窗口轮廓与阴影**：在 Windows 10/11 上打开启动器、设置窗与上下文菜单，确认不透明背景贴合窗口边界，UI 与阴影之间没有一圈透明留边；Windows 11 使用系统圆角，Windows 10 使用直角，最大化/贴靠时轮廓由系统决定。确认系统允许窗口阴影时出现 DWM 阴影；检查浅色/深色主题、不同 DPI、紧凑态展开/收起、调整尺寸及反复显示/隐藏后阴影正常，验证 Windows 11 关闭 DWM 边框绘制后顶部额外 1px 细线是否消失，阴影和圆角是否保留；Windows 10 可能仍有细线，确认不遮挡内容。分别用可用的 GPU renderer 和 software 检查，确认窗口定位、内部控件圆角、列表与详情间距及边缘拖动缩放正常。
- [ ] **系统主题变化**：主题设为“跟随系统”，在 Windows 设置中切换浅色/深色；确认启动器、设置窗和托盘菜单颜色同步变化。再固定为浅色/深色，确认系统变化不会覆盖用户选择。
- [ ] **登录时启动**：在 Windows“常规”设置中开启登录时启动，确认当前用户 Run 项写入带引号的 Wowie 路径；注销并重新登录后确认按预期显示启动器。关闭该选项，确认 Wowie Run 值立即删除，配置保存失败时确认注册表状态回滚。
- [ ] **Windows 设置搜索与语言**：分别将 Wowie 设为英文和简体中文；用两种语言搜索蓝牙、显示、Wi-Fi、隐私、Windows 更新及 Command Palette 目录中的控制面板/MMC 条目，确认标题、区域和替代名称与固定版本的上游资源一致（例如 Wi-Fi 的中文标题为 WLAN），两种语言别名都能命中。用 `设置应用程序 > 帐户` 一类的 `>` 路径查询确认逐级筛选。搜索本机 All Tasks 中的控制面板任务，确认使用系统显示语言的标题、区域和关键词，能通过保留的 PIDL 打开并显示原生图标。打开常用结果确认设置页跳转，确认设置页使用 Windows 设置图标；在 Windows 10 与 Windows 11 上检查 build 过滤，在无电池、无 Wi-Fi 适配器、无 Precision Touchpad 的设备上确认对应页面不显示，并检查 Windows 11 24H2 的混合现实页面已隐藏。
- [ ] **文件搜索后端与加载页**：确认发布版即使不带外部 `plugins` 目录也显示独立的“文件搜索”入口。在“插件 > 文件搜索”设置中选择“自动”、“Everything”和“Windows Search”：自动模式在 Everything IPC 可用时使用 Everything，否则回退到系统索引；手动选项固定对应后端，且设置重启后保留。进入文件搜索 session 后输入较慢查询，确认 session 结果区域显示本地化加载页、root search 不显示加载提示；结果完成后加载页消失，连续输入新查询时旧查询取消且界面仍可响应。测试文件名包含空格、引号、中文字符的查询、结果打开和连续翻页；在 Everything 不可用或 Windows Search 服务不可用时确认其他启动器搜索仍正常。
- [ ] **托盘退出**：从托盘菜单选择“退出”，确认所有窗口关闭、进程结束；重复启动和退出两次，确认没有残留托盘图标或快捷键占用。
- [ ] **junction 与符号链接**：在自定义索引目录中加入指向自身或父目录的 junction、目录符号链接；确认扫描不会递归循环或重复列出目标，并在移除链接后正常刷新。
- [ ] **不可读配置**：备份当前配置，将读取 ACL 设为拒绝当前用户读取；确认设置编辑和退出尺寸保存不会覆盖原文件，设置窗能显示读取错误。恢复 ACL 与备份。
- [ ] **renderer fallback**：分别选用可用 renderer，并在受限/不可用图形环境下启动；确认首选 renderer 初始化失败时回退到 software，日志包含首选 renderer 和失败原因。
