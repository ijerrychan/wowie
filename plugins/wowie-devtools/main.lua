-- Wowie Developer Tools
-- A small practical plugin that keeps the local plugin documentation searchable.

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

wowie.on("query", function(query, context)
    local locale = wowie.locale()
    local workspace = preference(context, "workspace", "repository")
    local command = context.command_id
    local suffix = text(locale, "Workspace: " .. workspace, "工作区：" .. workspace)

    local items = {
        wowie.result({
            id = "developer-guide",
            title = text(locale, "Plugin Development Guide", "插件开发指南"),
            subtitle = suffix,
            aliases = {"plugin guide", "lua api", "插件文档"},
            target = "docs/插件开发.md",
            icon = icon("named", "book"),
            command_id = command,
            actions = {
                wowie.action("open", icon("system", "folder")),
                wowie.action("inspect", icon("named", "calculator"))
            },
            detail = {
                title = text(locale, "Plugin Development", "插件开发"),
                subtitle = text(locale, "Embedded Lua API reference", "内嵌 Lua API 参考"),
                source = "wowie-devtools",
                blocks = {
                    {type = "text", text = text(locale, "Runs inside Wowie without an external runtime.", "运行在 Wowie 内部，不依赖外部运行时。"), color = "primary"},
                    {type = "metric", label = text(locale, "Runtime", "运行时"), value = "Embedded Lua", color = "accent"},
                    {type = "icon", glyph = "★", color = "accent", icon = icon("named", "book")},
                    {type = "separator"}
                },
                actions = {
                    {id = "open", title = text(locale, "Open guide", "打开指南"), icon = icon("system", "folder")},
                    {id = "inspect", title = text(locale, "Inspect API", "检查 API"), icon = icon("named", "calculator")}
                },
                width = {preferred = 500, min = 360, max = 680}
            }
        }),
        wowie.result({
            id = "plugin-todo",
            title = text(locale, "Plugin Support TODO", "插件支持 TODO"),
            subtitle = text(locale, "Track implementation progress", "查看实现进度"),
            aliases = {"plugin status", "roadmap", "插件计划"},
            target = "docs/plugin-support-todo.md",
            icon = icon("named", "checklist"),
            command_id = command,
            actions = {
                wowie.action("open", icon("system", "folder"))
            },
            detail = {
                title = text(locale, "Plugin Support Roadmap", "插件支持路线图"),
                blocks = {
                    {type = "metric", label = text(locale, "Phase", "阶段"), value = "1-3 ready", color = "accent"},
                    {type = "text", text = text(locale, "Phase 4 detail enhancements are intentionally paused.", "Phase 4 详情增强已暂缓。"), color = "secondary"}
                },
                actions = {
                    {id = "open", title = text(locale, "Open TODO", "打开 TODO"), icon = icon("system", "folder")}
                },
                width = {preferred = 440, min = 320, max = 600}
            }
        })
    }

    return {
        request_id = context.request_id,
        items = items
    }
end)

wowie.on("execute", function(item_id, action_id, context)
    local locale = wowie.locale()
    local label = text(locale, "Executed ", "已执行 ")
    return {
        request_id = context.request_id,
        title = label .. action_id,
        subtitle = item_id .. " · " .. preference(context, "workspace", "repository")
    }
end)
