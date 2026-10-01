# Windows 系统集成回归检查

## 自动化覆盖

| 场景 | 自动化覆盖 | 运行方式 |
| --- | --- | --- |
| 快捷键冲突后保留旧注册 | 临时占用 F24，更新失败后检查旧快捷键仍注册 | `cargo test platform::windows::tests::hotkey_update_conflict_restores_the_previous_registration -- --ignored --nocapture --test-threads=1` |
| 平台监听器退出与重复启动 | 两轮启动快捷键、托盘、目录、Shell 和包监听后统一停止 | `cargo test platform::windows::tests::platform_listeners_release_resources_across_restarts -- --ignored --nocapture --test-threads=1` |
| 深层目录变化通知与空闲 CPU | 临时目录、1000 个文件、深层新建文件；同时输出扫描耗时和空闲 CPU | `cargo test platform::windows::tests::custom_directory_notification_covers_deep_changes -- --nocapture --test-threads=1` |
| 损坏 `.lnk` | 固定截断 ShellLink 样本及偏移边界、ANSI、Unicode 和超大文件用例 | `cargo test platform::windows::shortcuts::tests -- --nocapture --test-threads=1` |
| 不可解析配置 | 固定的截断 JSON 样本确保读取失败时原文件保持不变 | `cargo test config::tests::invalid_existing_config_is_preserved_and_valid_writes_replace_it -- --nocapture` |
| renderer 配置规范化 | 确认支持的 renderer 值在配置加载后保留 | `cargo test config::tests::supported_renderers_are_preserved_by_normalization` |

测试样本位于 [`tests/fixtures/windows`](../tests/fixtures/windows/README.md)。系统集成测试默认忽略，运行前关闭其他可能占用测试热键的应用；它们会短暂注册系统级热键、托盘图标和 Shell 监听器。

## 手工桌面检查清单

在真实 Windows 桌面完成。每次记录 Windows 版本、显示器数量与缩放比例、所选 renderer、结果和日期。

- [ ] **DPI 与多显示器定位**：分别在单显示器、不同缩放率的双显示器和负坐标副屏上打开启动器、设置窗与上下文菜单；确认窗口完整落在目标屏幕工作区内，缩放变化后尺寸合理。
- [ ] **原生窗口轮廓与阴影**：在 Windows 10/11 上打开启动器、设置窗与上下文菜单，确认不透明背景贴合窗口边界，UI 与阴影之间没有一圈透明留边；Windows 11 使用系统圆角，Windows 10 使用直角，最大化/贴靠时轮廓由系统决定。确认系统允许窗口阴影时出现 DWM 阴影；检查浅色/深色主题、不同 DPI、紧凑态展开/收起、调整尺寸及反复显示/隐藏后阴影正常，顶部原生 1px 细线不遮挡内容。分别用可用的 GPU renderer 和 software 检查，确认窗口定位、内部控件圆角、列表与详情间距及边缘拖动缩放正常。
- [ ] **系统主题变化**：主题设为“跟随系统”，在 Windows 设置中切换浅色/深色；确认启动器、设置窗和托盘菜单颜色同步变化。再固定为浅色/深色，确认系统变化不会覆盖用户选择。
- [ ] **Windows 设置搜索与语言**：分别将 Wowie 设为英文和简体中文；用两种语言搜索蓝牙、显示、Wi-Fi、隐私、Windows 更新及 PowerToys 目录中的控制面板/MMC 条目，确认 Microsoft Learn 中文标签优先、回退名称仍可搜索，两种语言别名都能命中。用 `系统设置 > 帐户` 一类的 `>` 路径查询确认逐级筛选。打开常用结果确认设置页跳转，确认设置页使用 Windows 设置图标；在 Windows 10 与 Windows 11 上检查 build 过滤，在无电池、无 Wi-Fi 适配器、无 Precision Touchpad 的设备上确认对应页面不显示，并检查 Windows 11 24H2 的混合现实页面已隐藏。
- [ ] **托盘退出**：从托盘菜单选择“退出”，确认所有窗口关闭、进程结束；重复启动和退出两次，确认没有残留托盘图标或快捷键占用。
- [ ] **junction 与符号链接**：在自定义索引目录中加入指向自身或父目录的 junction、目录符号链接；确认扫描不会递归循环或重复列出目标，并在移除链接后正常刷新。
- [ ] **不可读配置**：备份当前配置，将读取 ACL 设为拒绝当前用户读取；确认设置编辑和退出尺寸保存不会覆盖原文件，设置窗能显示读取错误。恢复 ACL 与备份。
- [ ] **renderer fallback**：分别选用可用 renderer，并在受限/不可用图形环境下启动；确认首选 renderer 初始化失败时回退到 software，日志包含首选 renderer 和失败原因。
