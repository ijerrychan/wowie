-- Wowie Developer Tools
-- A small practical plugin for exercising the host's P0 plugin surface.

local function is_chinese(locale)
    return locale == "zh" or locale == "zh-CN" or locale == "zh-Hans"
end

local function text(locale, english, chinese)
    if is_chinese(locale) then
        return chinese
    end
    return english
end

local function preference(context, key, fallback)
    local preferences = context.preferences or {}
    return preferences[key] or fallback
end

local function icon(kind, value)
    if kind == "system" then
        return {type = kind, target = value}
    end
    return {type = kind, id = value}
end

local function detail_actions(locale)
    return {
        {id = "open", title = text(locale, "Open guide", "打开指南"), icon = icon("named", "book")},
        {id = "open-preview-link", title = text(locale, "Open preview link", "打开预览链接"), icon = icon("named", "wowie")},
        {id = "retry-preview", title = text(locale, "Retry preview", "重试预览"), icon = icon("named", "checklist")}
    }
end

local function gallery_item(locale, command)
    return wowie.result({
        id = "preview-gallery",
        title = text(locale, "P0 Preview Gallery", "P0 预览能力展厅"),
        subtitle = text(locale, "Markdown, image, code, table, link and more", "Markdown、图片、代码、表格、链接等能力"),
        aliases = {"preview gallery", "detail blocks", "preview", "预览展厅", "详情块"},
        target = "docs/ui-improvement-todo.md",
        icon = {type = "resource", path = "icons/book-open.svg"},
        command_id = command,
        actions = {
            wowie.action("open", icon("named", "book")),
            wowie.action("open-preview-link", icon("named", "wowie")),
            wowie.action("retry-preview", icon("named", "checklist"))
        },
        detail = {
            title = text(locale, "Structured Preview Gallery", "结构化预览展厅"),
            subtitle = text(locale, "Every block is returned as data, not Slint", "所有详情块都以数据返回，不依赖 Slint 类型"),
            source = "wowie-devtools / preview-gallery",
            blocks = {
                {type = "text", text = text(locale, "This result exercises the renderer-neutral preview model.", "这个结果用于测试与渲染器无关的预览模型。"), color = "primary"},
                {type = "metric", label = text(locale, "Block count", "详情块数量"), value = "10", color = "accent"},
                {type = "markdown", markdown = text(locale, "**Markdown** stays declarative and can contain _emphasis_.", "**Markdown** 保持声明式，并支持 _强调_。"), color = "secondary"},
                {type = "image", source = "icons/book-open.svg", alt = text(locale, "Bundled resource image", "插件内置资源图片")},
                {type = "code", language = "lua", code = "local preview = wowie.result({ detail = blocks })\nreturn preview"},
                {type = "table", columns = {text(locale, "Block", "块"), text(locale, "Status", "状态")}, rows = {
                    {"markdown", text(locale, "ready", "就绪")},
                    {"image", text(locale, "ready", "就绪")},
                    {"table", text(locale, "ready", "就绪")}
                }},
                {type = "link", label = text(locale, "Documentation", "文档"), url = "https://example.com/wowie-preview"},
                {type = "icon", glyph = "★", color = "accent", icon = {type = "resource", path = "icons/book-open.svg"}},
                {type = "weather", value = "clear"},
                {type = "separator"}
            },
            actions = detail_actions(locale),
            width = {preferred = 560, min = 400, max = 720},
            scrollable = true
        }
    })
end

local function states_item(locale, command)
    return wowie.result({
        id = "preview-states",
        title = text(locale, "Preview Loading and Error States", "预览加载与错误状态"),
        subtitle = text(locale, "Loading, error and retry action", "加载中、错误和重试操作"),
        aliases = {"preview states", "loading preview", "preview error", "加载预览", "预览错误"},
        target = "docs/ui-improvement-todo.md",
        icon = icon("named", "checklist"),
        command_id = command,
        actions = {
            wowie.action("retry-preview", icon("named", "checklist"))
        },
        detail = {
            title = text(locale, "Preview State Gallery", "预览状态展厅"),
            subtitle = text(locale, "The host can render recoverable states", "宿主可以渲染可恢复的状态"),
            source = "wowie-devtools / preview-states",
            blocks = {
                {type = "loading", message = text(locale, "Loading a remote preview…", "正在加载远程预览…")},
                {type = "error", message = text(locale, "The preview service returned a test error.", "预览服务返回了测试错误。"), retry_action = "retry-preview"}
            },
            actions = {
                {id = "retry-preview", title = text(locale, "Retry preview", "重试预览"), icon = icon("named", "checklist")}
            },
            width = {preferred = 440, min = 320, max = 600}
        }
    })
end

local function safety_item(locale, command)
    return wowie.result({
        id = "preview-safety",
        title = text(locale, "Preview Safety Fallbacks", "预览安全降级"),
        subtitle = text(locale, "Rejected resource and link examples", "被拒绝的资源和链接示例"),
        aliases = {"preview safety", "unsafe preview", "安全降级"},
        target = "docs/ui-improvement-todo.md",
        icon = icon("named", "shield-account"),
        command_id = command,
        detail = {
            title = text(locale, "Safety Fallbacks", "安全降级"),
            subtitle = text(locale, "Invalid inputs become visible error blocks", "无效输入会变成可见的错误详情块"),
            source = "wowie-devtools / preview-safety",
            blocks = {
                {type = "image", source = "../wowie.png", alt = text(locale, "Rejected path", "被拒绝的路径")},
                {type = "link", label = text(locale, "Rejected link", "被拒绝的链接"), url = "javascript:alert(1)"}
            },
            width = {preferred = 440, min = 320, max = 600}
        }
    })
end

local function guide_item(locale, workspace, command)
    local suffix = text(locale, "Workspace: " .. workspace, "工作区：" .. workspace)
    return wowie.result({
        id = "developer-guide",
        title = text(locale, "Plugin Development Guide", "插件开发指南"),
        subtitle = suffix,
        aliases = {"plugin guide", "lua api", "插件文档"},
        target = "docs/插件开发.md",
        icon = {type = "resource", path = "icons/book-open.svg"},
        command_id = command,
        actions = {
            wowie.action("open", icon("named", "book")),
            wowie.action("inspect", icon("named", "calculator"))
        },
        detail = {
            title = text(locale, "Plugin Development", "插件开发"),
            subtitle = text(locale, "Embedded Lua API reference", "内嵌 Lua API 参考"),
            source = "wowie-devtools",
            blocks = {
                {type = "text", text = text(locale, "Runs inside Wowie without an external runtime.", "运行在 Wowie 内部，不依赖外部运行时。"), color = "primary"},
                {type = "metric", label = text(locale, "Runtime", "运行时"), value = "Embedded Lua", color = "accent"},
                {type = "icon", glyph = "★", color = "accent", icon = {type = "resource", path = "icons/book-open.svg"}},
                {type = "separator"}
            },
            actions = {
                {id = "open", title = text(locale, "Open guide", "打开指南"), icon = icon("named", "book")},
                {id = "inspect", title = text(locale, "Inspect API", "检查 API"), icon = icon("named", "calculator")}
            },
            width = {preferred = 500, min = 360, max = 680}
        }
    })
end

local function todo_item(locale, command)
    return wowie.result({
        id = "plugin-todo",
        title = text(locale, "Plugin Support TODO", "插件支持 TODO"),
        subtitle = text(locale, "Track implementation progress", "查看实现进度"),
        aliases = {"plugin status", "roadmap", "插件计划"},
        target = "docs/plugin-support-todo.md",
        icon = icon("named", "checklist"),
        command_id = command,
        actions = {
            wowie.action("open", icon("named", "checklist"))
        },
        detail = {
            title = text(locale, "Plugin Support Roadmap", "插件支持路线图"),
            blocks = {
                {type = "metric", label = text(locale, "Phase", "阶段"), value = "1-3 ready", color = "accent"},
                {type = "text", text = text(locale, "P0 preview and action capabilities are ready to test.", "P0 预览和操作能力已可测试。"), color = "secondary"}
            },
            actions = {
                {id = "open", title = text(locale, "Open TODO", "打开 TODO"), icon = icon("named", "checklist")}
            },
            width = {preferred = 440, min = 320, max = 600}
        }
    })
end

local function contains(value, query)
    return string.find(string.lower(value or ""), string.lower(query), 1, true) ~= nil
end

local function matches_query(item, query)
    if query == "" then
        return true
    end
    if contains(item.title, query) or contains(item.subtitle, query) then
        return true
    end
    for _, alias in ipairs(item.aliases or {}) do
        if contains(alias, query) then
            return true
        end
    end
    return false
end

wowie.on("query", function(query, context)
    local locale = context.locale or wowie.locale()
    local workspace = preference(context, "workspace", "repository")
    local command = context.command_id
    local all_items = {
        guide_item(locale, workspace, command),
        todo_item(locale, command),
        gallery_item(locale, command),
        states_item(locale, command),
        safety_item(locale, command)
    }
    local items = {}

    -- The `dev`/`plugin` command is the everyday entry point. Keep the
    -- intentionally failing safety/state fixtures out of that result set;
    -- they remain available through explicit `preview`/`safety` searches.
    if command == "open" and query == "" then
        items = {all_items[1], all_items[2]}
    else
        for _, item in ipairs(all_items) do
            if matches_query(item, query) then
                table.insert(items, item)
            end
        end
    end

    return {
        request_id = context.request_id,
        items = items
    }
end)

wowie.on("execute", function(item_id, action_id, context)
    local locale = context.locale or wowie.locale()
    local label = text(locale, "Executed ", "已执行 ")
    return {
        request_id = context.request_id,
        title = label .. action_id,
        subtitle = item_id .. " · " .. preference(context, "workspace", "repository")
    }
end)
