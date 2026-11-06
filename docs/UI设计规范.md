# UI 设计规范

参考 [Magpie](https://github.com/yetone/magpie) 的轻量桌面界面：使用安静的中性色、连续内容面和细分隔线，以内容与操作建立层级，不添加装饰性大色块或自绘重阴影。

## 颜色

共享色值定义在 `ui/design-system.slint`，按窗口的 `dark-mode` 选取。

| 用途 | 浅色 | 深色 |
| --- | --- | --- |
| 画布 | `#f5f5f7` | `#202024` |
| 内容面 | `#ffffff` | `#28282d` |
| 控件、悬浮面 | `#eeeef2` | `#323238` |
| 边界 | `#e1e1e8` | `#3c3c44` |
| 主文字 | `#202025` | `#f2f2f5` |
| 次文字 | `#686873` | `#b1b1bc` |
| 辅助文字 | `#777780` | `#92929e` |

强调色由新增的 `accent_color` 配置集中解析，三个窗口共用同一结果：

| 配置值 | 行为 |
| --- | --- |
| 空字符串（默认） | 应用默认蓝紫：浅色 `#6260a8`，深色使用更亮的 `#9492dc`，保证强调色文字在深色内容面上的对比度 |
| `system` | 显式跟随系统强调色 |
| `#RRGGBB` | 使用用户指定的颜色，不做主题改写 |

非法值在加载配置时会归一化为空字符串；在设置界面提交非法值会直接报错并保留原值。选中结果使用由强调色推导的半透明 `accent-background`（浅色 alpha `0x28`，深色 alpha `0x42`），它不是用户直接设置的独立配置。错误提示与内容图标保留语义色。

## 布局、圆角与动效

圆角统一为三档 token：`radius-sm` 6px、`radius-md` 8px、`radius-lg` 12px（`ui/design-system.slint`）。原控件中零散的 7px/13px 等近似值已并入对应档位。等比圆形（开关滑块、状态点）保持自身半径。

动效 token 为 `motion-fast` 100ms、`motion-base` 140ms、`motion-slow` 160ms。所有应用内过渡最长 160ms，已移除 `ease-out-back` 的回弹，统一使用 `ease-out`/`ease-in` 等克制缓动。新增的 `animations_enabled` 配置默认开启，并在启动、设置回调和配置文件热重载三条路径同步到启动器、设置和上下文菜单；关闭时所有应用内 `animate` 立即到达终值。详情关闭后的清理定时器会根据该开关在 200ms 与下一事件循环之间切换，避免生命周期状态滞留。

设置窗口仅暴露 Slint Fluent 的标准 `ComboBox` 与滚动条。`Palette` 公开且可写的只有 `color-scheme`，因此这里只同步明暗配色；Fluent 下拉框内部的 3px 圆角、200ms 边框过渡和 150ms 图标过渡没有公开样式 API，不通过自绘下拉框强行覆盖，以保留完整键盘与可访问性行为。滚动条同样由 Fluent 样式绘制，仅随 `Palette.color-scheme` 对齐主题。

## 字体与文字层级

字体层级、字重同样定义在 `ui/design-system.slint`：`font-micro` 到 `font-page-title` 使用 `rem` 相对用户基准字号缩放，字重分为 regular/medium/semibold/bold。搜索输入、结果标题、分组标题、详情块和设置行均引用同一组 token，避免同一层级出现近似但不一致的字号或字重。

`font_family` 留空时，窗口先使用 Slint 的系统默认字体，启动过程不在 UI 线程扫描字体。后台通过 `fontdb` 枚举并缓存字体列表后，仅在缓存确认平台界面字体可用时，将留空选择解析为 Windows `Segoe UI`、Linux `Noto Sans`，macOS 保留系统默认族名；用户显式选择的字体始终优先。项目不捆绑额外字体包。该策略表示“优先可靠平台字体”，并不保证各平台字形或度量完全一致。

## 平台边界

Windows 继续使用 `native-window-frame`（`cfg!(windows)`）、DWM 原生圆角/阴影和 GDI 首帧背景填充，避免黑边与首帧闪色；这些原生外壳不属于 Slint 应用内容的统一范围。其他平台保留透明无框外壳和 Slint 自绘圆角。`native-background-color` 始终与各窗口实际画布一致。

本次改动没有在真实 macOS/Linux 设备或不同 Windows DPI/显卡组合上做逐项实机验证；已验证范围以 `cargo check`、格式检查和仓库内的软件渲染布局测试为准。此前文档中“用户已有强调色设置”的说法不准确：`accent_color`、界面动画开关和对应的中英文文案均为本次新增；原有实现只是直接读取系统强调色。
