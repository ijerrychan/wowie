# Plugin Support TODO

本文档记录 Wowie 插件支持的当前实现状态和后续计划。

## 当前架构

- 插件使用内嵌 Lua VM，在 Wowie 进程内运行。
- 插件不要求用户安装 Lua、Python、Node 等外部运行时。
- 插件不会创建独立子进程。
- 插件入口由 manifest 的 `entry_point` 指定，当前默认实现为 `main.lua`。
- 插件通过 `wowie.on("query", ...)` 和 `wowie.on("execute", ...)` 注册能力。
- Wowie 负责搜索、排序、配置、图标解析、详情渲染和 Action 调度；Lua 只返回声明式数据。
- 不支持全局快捷键。插件 shortcut 仅指搜索框中的 `keywords`、`aliases` 和 `action_words`。

## 已完成

### Phase 1：协议和基础模型

- [x] `api_version`、`request_id`、结构化 `error`
- [x] Manifest、command、query、execute 数据模型
- [x] command 的 `keywords`、`action_words` 和 command icon
- [x] action word 前缀解析并传入 `command_id`
- [x] 内嵌 Lua VM 和插件生命周期初始化
- [x] `wowie.on`、`wowie.result`、`wowie.results`、`wowie.action`
- [x] query / execute 的 Lua API 测试

### Phase 2：Provider、配置和 Action

- [x] 插件动态加入统一搜索流程
- [x] 插件 `enabled`、`weight`、`preferences`
- [x] 配置热更新后刷新启用状态、权重和 preferences
- [x] query / execute 读取 preferences
- [x] 插件状态、最近一次错误和失败计数
- [x] Plugin Action 接入结果详情和上下文菜单

### Phase 3：图标和 i18n 基础能力

- [x] 统一 `IconRef`：`resource`、`system`、`named`、受控 `remote`
- [x] 结果、command、Action、DetailAction、DetailBlock 使用统一图标模型
- [x] 插件资源路径禁止绝对路径和 `..` 穿越
- [x] PNG、ICO、SVG 文件大小和缩略图尺寸限制
- [x] 无法解析资源时保留 glyph fallback
- [x] query 上下文传入当前 locale 和 fallback locale
- [ ] light/dark 主题图标变体
- [ ] 完整的翻译资源加载与 key fallback API

## 暂停实施

### Phase 4：详情面板

当前已实现：`text`、`metric`、`icon`、`weather`、`separator`、`markdown`、`image`、`code`、`table`、`link`、`loading`、`error`、DetailAction、preferred width、scrollable，以及宿主按窗口宽度计算的面板比例。

结构化预览已经由仓库内的 Lua 插件示例和集成测试覆盖。

暂不实施：

- [x] 可滚动详情容器
- [x] markdown、image、code、table、link、loading、error 等结构化 primitive
- [ ] 插件动态详情请求（当前使用静态结构化预览；动态详情继续由宿主 DetailProvider 处理）

## 下一步建议

### 1. 完善内嵌 Lua API

- [ ] 提供正式的 `wowie.locale`、翻译查找和 locale 变更通知
- [x] 提供受控的 HTTP、剪贴板和下载 API（共享客户端、权限门控、大小限制）
- [ ] 提供更细粒度的文件 API 和系统 Action API
- [ ] 为 API 增加超时、取消和结果数量限制
- [ ] 明确 Lua 错误映射为插件状态和用户可见错误

### 2. 插件安全和稳定性

- [ ] Manifest permissions 校验和宿主 API 权限门控
- [ ] 限制单次 query 的执行时间、内存和返回数据量
- [ ] 插件 VM 错误隔离与重新初始化
- [ ] 插件目录变更检测和开发模式 reload
- [ ] 禁止插件直接访问宿主 UI、窗口句柄和任意子进程

### 3. 测试和开发体验

- [x] 内嵌 Lua query / execute 最小闭环测试
- [x] 增加真实 `manifest.json + main.lua` 示例插件
- [x] 增加图标、详情、i18n、Action 的端到端测试
- [ ] 增加 API 版本和错误场景测试
- [ ] 提供插件开发文档和模板

## Manifest 示例

```json
{
  "id": "github",
  "name": "GitHub",
  "version": "1.0.0",
  "api_version": 1,
  "entry_point": "main.lua",
  "keywords": ["github", "gh"],
  "commands": [
    {
      "id": "search",
      "title": "Search GitHub",
      "action_words": ["gh"],
      "icon": {"type": "named", "id": "github"}
    }
  ]
}
```

## 产品边界

- 插件不能注册全局快捷键。
- 插件不能直接绘制 Slint UI；详情面板使用宿主声明式 primitive。
- 插件不应依赖用户环境中的语言运行时或命令行工具。
- 插件 API 优先保证稳定、可测试和受宿主控制，再扩展更多 UI 能力。
