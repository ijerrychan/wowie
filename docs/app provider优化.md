### TODO

* [x] **隔离 filesystem path 与 Shell Namespace path**

  * 增加 `is_shell_namespace()`
  * 增加 `same_filesystem_target()`
  * 所有 `normalize_path(a) == normalize_path(b)` 的 target 比较统一改掉
  * 确保 `shell:AppsFolder\...` 不参与普通文件路径比较

* [x] **完善虚拟 AppsFolder 应用关联**

  * 从解析后的 Shortcut/ShellLink target 或 arguments 中提取 `shell:AppsFolder\...`，优先生成 AUMID 或 `shell:` application identity
  * `title + identity.contains(title)` 保留为候选唯一时的 fallback，避免同名应用误合并

* [x] **明确 Application identity 与 LaunchEntry 的职责**

  * `SearchResult.identity`：只表示“哪个应用”
  * `LaunchEntry`：表示“通过什么入口、什么参数启动”
  * 避免将 arguments / working directory 等启动配置长期编码进 application identity

* [x] **补充重复应用测试**

  * 同一应用多个 `.lnk`
  * 同一 `.exe` 不同 arguments
  * AppsFolder + StartMenu + Desktop
  * Shell path 与 filesystem path 不应误匹配
  * 不同应用同名时不能误合并

* [x] **检查 `merge_by_identity()` 的语义**

  * 确认合并后 `source / aliases / launch_entries` 都完整保留
  * 确认最终 `title` 的选择符合预期

* [x] **最后再优化代码结构**

  * 把 identity 判断、target 判断、source 判断拆成几个小 helper
  * 减少 `shortcut_matches_application()` 中的条件分支

### 推荐执行顺序

```text
1. same_filesystem_target()
        ↓
2. 补 Shell path 隔离测试
        ↓
3. 完善 shell: 虚拟应用关联
        ↓
4. identity / LaunchEntry 概念重构
        ↓
5. 补全各种重复/误合并测试
        ↓
6. 最后做代码整理
```

**目前不建议动 `merge_by_identity()` 的整体结构**。现阶段它的行为已经比较稳定，先把路径 namespace 和虚拟应用关联这两个边界处理干净更合适。
