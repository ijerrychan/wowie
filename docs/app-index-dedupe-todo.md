# 应用索引与去重 TODO

本文记录 AppProvider 后续可优化的索引、身份识别、跨来源关联和重复项合并工作。

现有流程大致为：

```text
AppsFolder / Package / StartMenu / Desktop / 其他来源
    ↓
发现源项目
    ↓
关联文件入口与已发现应用
    ↓
按 identity 合并
    ↓
按实际启动行为去重 LaunchEntry
    ↓
兜底分类、选择默认入口、加载展示元数据
```

相关实现主要位于：

- `src/providers/app.rs`：来源编排、来源刷新和最终合并
- `src/providers/app/discovery.rs`：Start Menu、Desktop、AppsFolder 等来源转换
- `src/providers/app/association.rs`：快捷方式与应用关联、启动入口合并
- `src/providers/app/identity.rs`：identity 生成与 identity 合并
- `src/platform/windows.rs`：Windows AppsFolder、Package Catalog 和应用元数据
- `src/core/result.rs`：`SearchResult`、`LaunchEntry` 和结果元数据模型

## 优化目标

- 不因为显示标题相同而提前丢失不同应用。
- 只有证据足够强时才合并不同来源，避免误合并。
- 合并后完整保留来源、入口、图标、描述、Action 和其他展示元数据。
- 将应用身份和启动行为继续保持分离。
- 让包应用的身份和描述来源更加可靠。
- 在保持结果一致性的前提下，逐步减少不必要的全量重建。

## P0：修复可能丢数据或误合并的问题

### [x] 移除 AppsFolder 阶段基于标题的提前丢弃

当前 AppsFolder 枚举中存在按本地化标题判断是否已经存在商店应用的逻辑。相同标题不等于相同应用，两个不同包可能拥有相同的显示名称。

调整方向：

- 优先使用 AUMID、AppUserModelId、PackageFamilyName 或包身份建立候选记录。
- 只有身份相同或已确认属于同一应用时才合并。
- 标题只能用于排序、展示和受限的低置信度关联，不能用于直接丢弃源项目。
- 保留同名不同包，允许它们在结果层分别显示。

验收标准：

- [x] 两个不同 PackageFamilyName 但标题相同的应用都能被索引。
- [x] 同一应用从 AppsFolder 和 Package Catalog 返回时仍只显示一个结果。
- [x] 不同语言环境下，标题变化不会改变去重结果。

### [x] 完善重复记录的元数据合并策略

`merge_by_identity()` 当前主要合并 aliases、sources、launch entries 和部分图标信息。当高优先级来源替换当前记录时，其他来源的描述、Action、detail 或 ranking 信息可能没有统一的保留规则。

当前进度：

- [x] Action 按稳定值合并并去重。
- [x] detail、图标和类别采用非空优先及来源优先级策略。
- [x] ranking bias 在 canonical record 替换时保留有效值。
- [x] 将应用描述加入索引结果模型并纳入合并策略。

调整方向：

- 为每个字段定义明确策略：优先级覆盖、非空优先、合并、最大值或重新计算。
- 描述采用“最高可信度的非空描述”，而不是简单跟随最终 canonical record。
- 合并 Actions 时按稳定 Action identity 去重。
- detail、ranking_bias、图标和类别信息不得因来源替换无故丢失。
- 对最终 canonical record 保留所有来源和启动入口。

验收标准：

- 高优先级来源替换记录后，别名、来源、入口、图标、描述和 Action 均有测试覆盖。
- 相同字段重复合并不会产生重复值。
- 合并顺序改变时，最终结果仍保持确定性。

### [ ] 增加 identity 证据等级和歧义保护

当前 identity 规则已经区分 AUMID、文件路径、Shell path 和 URL，但后续规则仍容易随着特殊快捷方式不断增加条件分支。

建议定义类似以下的证据等级：

```text
强证据：AUMID、AppUserModelId、PackageFamilyName、规范化后的精确目标
中证据：快捷方式解析出的目标、同一文件系统目标、明确的 URL
弱证据：标题、产品名、可执行文件名、技术别名
```

合并规则：

- 强证据相同，可以直接合并。
- 只有中证据时，需要来源和上下文约束。
- 只有弱证据时，不自动合并；最多生成待确认候选。
- 多个强候选或强弱证据冲突时，必须拒绝合并。

当前进度：

- [x] 入口关联会拒绝冲突的 AppUserModelId / PackageFamilyName。
- [x] 文件目标强匹配也会经过包身份冲突检查，不能绕过身份保护。
- [ ] 将匹配证据正式类型化，并输出可解释的去重决策诊断。

## P1：完善索引质量和包应用处理

### [x] 让快捷方式关联检查全部有效启动入口

关联逻辑不应只依赖某个项目的第一个 `LaunchEntry`。一个结果可能同时包含多个快捷方式、多个目标或不同启动参数。

调整方向：

- 对所有有效入口建立目标索引。
- 为每个候选入口计算匹配强度。
- 只有唯一强匹配时自动关联。
- 多个入口分别匹配不同应用时保留歧义，不强行合并。

已实现：遍历所有启动入口，合并同一应用的重复候选，并让强匹配优先于受限标题回退；多个不同候选仍会拒绝自动关联。

### [ ] 将包应用身份提升为显式索引字段

当前部分包信息通过 identity 字符串表达。后续可在应用索引模型中显式保存：

- AppUserModelId / AUMID
- PackageFamilyName
- Package full name
- Package install location
- Application manifest identity
- 应用级 DisplayName 和 Description 的来源

当前进度：

- [x] `SearchResult` 和 `StoreApplication` 显式保存 `AppUserModelId`。
- [x] 从 AUMID 保存 `PackageFamilyName`，并在重复项合并时保留。
- [x] 保存 Package full name 和 package identity name。
- [ ] 补充可靠的 Package 安装位置。

这样可以避免从 `aumid:` 字符串中反向拆解业务信息，并方便精确查询、诊断和后续增量更新。

### [ ] 改进包应用描述获取和本地化资源解析

应用描述应优先从应用级 manifest `Description` 获取，并支持 `ms-resource:` 资源解析；Package 级 Description、AppsFolder 展示名称和 EXE `FileDescription` 作为有明确优先级的回退来源。

建议描述优先级：

```text
应用 manifest Description
    ↓
已解析的 ms-resource Description
    ↓
Windows AppInfo.DisplayInfo.Description
    ↓
Package.Description
    ↓
EXE FileDescription / 快捷方式 Description
    ↓
本地化类别名称（Application / 应用）
```

描述最好在索引阶段保存到结果模型中，而不是首次 UI 展示时才查询。这样搜索、排序、详情面板和调试信息可以共享同一份数据。

### [ ] 扩展 Windows 目标规范化

在不强制解析真实文件、避免启动阶段明显变慢的前提下，补充以下等价形式的处理：

- UNC 路径和本地路径的统一规则。
- 环境变量路径的受控展开。
- Windows 8.3 短路径与长路径的关联。
- junction / symbolic link 的可选规范化。
- `file:` URL 与本地路径的边界。
- Shell namespace path 与 filesystem path 的继续隔离。

每种规范化都必须保留原始路径，不能影响实际启动行为和展示位置。

## P1：完善测试和诊断能力

### [ ] 增加完整索引流水线快照测试

测试不应只验证单个 helper，而应覆盖：

```text
发现
  → 入口关联
  → identity 合并
  → LaunchEntry 行为去重
  → 分类
  → 默认入口选择
  → 展示元数据合并
```

至少覆盖以下场景：

- AppsFolder、Package、Start Menu、Desktop 同时出现的同一应用。
- 两个不同包拥有相同本地化标题。
- 同一 EXE 拥有多个参数不同的快捷方式。
- 同一目标使用不同工作目录。
- 同名但目标不同的应用。
- AUMID 不同但目标文件相同。
- Shell path 与 filesystem path 不应误匹配。
- 本地 HTML、远程 URL 和应用快捷方式混合出现。
- 缺少描述、描述为资源引用、描述为多语言资源。
- 来源刷新后删除一个来源项目，其他来源仍能保留应用。

当前进度：

- [x] 增加跨来源完整流水线回归测试，覆盖发现、入口关联、identity 合并、分类和默认入口选择。
- [x] 覆盖同名不同包、同一目标不同参数，以及包身份冲突保护。
- [x] 覆盖 AppsFolder 优先级、Package 元数据补全和关联来源保留。
- [ ] 补齐 URL、Shell path、本地 HTML、资源描述和来源刷新删除等场景。

### [x] 添加去重决策诊断信息

在调试模式下记录每次关联和合并的原因：

- 被比较的两个项目。
- 使用的 identity 或目标键。
- 匹配证据等级。
- 是否存在其他候选。
- 最终选择合并、保留独立项还是拒绝关联。
- canonical record 选择了哪个来源。

诊断信息应默认关闭，并支持现有的 Provider 调试筛选，避免普通用户看到内部路径和技术字段。

已完成：

- identity 合并会记录参与合并的来源、canonical 来源、是否发生 canonical 替换，以及两侧入口数量。
- LaunchEntry 行为去重会记录去重前后数量和移除数量。
- 入口关联会记录匹配证据、候选数量，以及歧义拒绝信息。
- 日志继续受调试开关和 `WOWIE_PROVIDER_DEBUG_FILTER` 控制。

### [x] 增加索引结果统计

在调试输出或开发诊断页面中统计：

- 各来源发现数量。
- identity 合并数量。
- LaunchEntry 去重数量。
- 因歧义而拒绝合并的数量。
- 缺失描述的包应用数量。
- 同标题但不同身份的应用数量。
- 每个来源的索引耗时。

当前进度：

- [x] 调试输出各来源输入量、入口关联量、歧义拒绝量、identity 合并量和 LaunchEntry 去重量。
- [x] 输出最终结果数、缺失包描述数、同标题不同身份数和本次合并耗时。
- [x] 输出各来源发现阶段的独立耗时统计。

## P2：缓存和架构优化

### [ ] 将索引缓存按来源拆分

参考 PowerToys 的 Win32 / Package 分离方式，将来源快照和统一结果合并分开：

```text
Source Repository
    ├── AppsFolder snapshot
    ├── Package snapshot
    ├── StartMenu snapshot
    └── Desktop snapshot
             ↓
      Association / Merge Layer
             ↓
        Unified SearchResult
```

目标：

- 单个来源变化时只重新发现该来源。
- 保留其他来源的稳定快照。
- 明确区分来源缓存和最终合并结果。
- 后续支持按 PackageFamilyName、AUMID 或文件目标快速查询。

### [ ] 减少来源刷新后的全量合并成本

在保证结果确定性的前提下，评估：

- 只重新计算受影响 identity 的关联。
- 为 filesystem target、AUMID 和 PackageFamilyName 建立反向索引。
- 将完整合并保留为启动时重建和诊断模式下的校验路径。
- 增加索引耗时基准，避免为了微优化牺牲去重正确性。

### [ ] 统一应用索引的内部中间模型

当前 `SearchResult` 同时承担索引结果、展示结果和启动结果。后续可评估引入轻量中间模型：

```text
DiscoveredApp
    ├── identity evidence
    ├── package metadata
    ├── display metadata
    ├── source records
    └── launch entries
             ↓
       SearchResult
```

该模型不应改变现有 Provider 对外行为，优先用于隔离发现、合并和展示字段的生命周期。

## 推荐实施顺序

```text
1. 移除 AppsFolder 标题提前丢弃
        ↓
2. 补充同名不同包和跨来源快照测试
        ↓
3. 完善 merge_by_identity() 的字段保留策略
        ↓
4. 增加 identity 证据等级和去重诊断
        ↓
5. 完善包应用身份与描述索引
        ↓
6. 检查全部 LaunchEntry 的关联逻辑
        ↓
7. 再评估来源缓存和增量合并
```

## 参考实现

- [PowerToys AppCache.cs](https://github.com/microsoft/PowerToys/blob/main/src/modules/cmdpal/ext/Microsoft.CmdPal.Ext.Apps/AppCache.cs)：Win32 和 Package Repository、文件监视和 reload 状态。
- [PowerToys UWPApplication.cs](https://github.com/microsoft/PowerToys/blob/main/src/modules/cmdpal/ext/Microsoft.CmdPal.Ext.Apps/Programs/UWPApplication.cs)：包应用 manifest 描述、AUMID 和资源字符串处理。
- [PowerToys Win32Program.cs](https://github.com/microsoft/PowerToys/blob/main/src/modules/cmdpal/ext/Microsoft.CmdPal.Ext.Apps/Programs/Win32Program.cs)：Win32 描述获取和复合字段去重比较器。
- [PowerToys AllAppsCommandProvider.cs](https://github.com/microsoft/PowerToys/blob/main/src/modules/cmdpal/ext/Microsoft.CmdPal.Ext.Apps/AllAppsCommandProvider.cs)：包名、产品码和显示名称查询中的唯一匹配与歧义处理。

## 完成标准

- 同名不同应用不会因为显示标题相同而被提前丢弃或错误合并。
- 同一应用的多个来源只产生一个结果，并保留全部有效启动行为。
- 合并操作不会丢失描述、图标、Action、detail、别名或来源信息。
- 包应用可以稳定获得应用级描述，缺失时按定义好的顺序回退。
- 所有身份关联和去重决策都有自动化测试覆盖。
- Provider 刷新后的结果与启动时全量重建结果一致。
- 诊断模式可以解释任意一项合并或拒绝合并的原因。
