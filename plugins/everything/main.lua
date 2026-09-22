local COMMAND_ID = "search"
local EVERYTHING_ICON_TARGET = wowie.everything_icon_target()

local function is_zh()
    local locale = wowie.locale("current") or ""
    return string.sub(locale, 1, 2) == "zh"
end

local function text(en, zh)
    return is_zh() and zh or en
end

local function icon()
    if EVERYTHING_ICON_TARGET then
        return {type = "system", target = EVERYTHING_ICON_TARGET}
    end
    return {type = "named", id = "folder-open"}
end

local function entry(query)
    local empty = query == ""
    return wowie.result({
        id = "entry",
        title = empty and text("Search with Everything", "使用 Everything 搜索")
            or text("Search with Everything", "使用 Everything 搜索") .. " \"" .. query .. "\"",
        subtitle = text(
            "Search files with Everything; all Everything syntax is supported",
            "使用 Everything 搜索文件，支持全部 Everything 语法"
        ),
        aliases = {"Everything", "everything"},
        category = "other",
        command_id = COMMAND_ID,
        session_entry = true,
        visible_when = empty and "empty_query" or "always",
        ranking_bias = empty and 0 or -1000,
        icon = icon()
    })
end

local function file_result(item)
    local path = item.path or ""
    local name = item.name or path
    return wowie.result({
        id = "file:" .. path,
        title = name,
        subtitle = path,
        category = "document",
        aliases = {name},
        target = path,
        icon = {type = "system", target = path}
    })
end

wowie.on("query", function(query, context)
    -- Root search contributes a discoverable entry. Once the `e ` command is
    -- recognized, only the active session performs the IPC query.
    if context.command_id ~= COMMAND_ID then
        return {
            request_id = context.request_id,
            items = {entry(query)}
        }
    end

    if query == "" then
        return {request_id = context.request_id, items = {}}
    end

    -- Keep each IPC page small; the host can request another page as the
    -- result list approaches its end.
    local limit = math.min(context.limit or 30, 30)
    local rows = wowie.everything_search(query, context.offset or 0, limit)
    local items = {}
    for _, row in ipairs(rows) do
        items[#items + 1] = file_result(row)
    end
    return {
        request_id = context.request_id,
        items = items
    }
end)

wowie.on("search_engine_icon", function(context)
    local target = wowie.everything_icon_target()
    return {
        request_id = context.request_id,
        icon = target and {type = "system", target = target} or icon()
    }
end)

wowie.on("empty_state", function(context)
    return {
        request_id = context.request_id,
        title = text("Search with Everything", "使用 Everything 搜索"),
        description = text(
            "Search files with Everything; all Everything syntax is supported. Press Enter to view the usage guide.",
            "使用 Everything 搜索文件，支持全部 Everything 语法。按 Enter 查看用法说明。"
        ),
        icon = icon(),
        detail = {
            title = text("Everything search guide", "Everything 搜索说明"),
            subtitle = text("Everything native query syntax", "Everything 原生搜索语法"),
            fullscreen = true,
            scrollable = true,
            blocks = {{
                type = "text",
                color = "primary",
                copyable = true,
                text = text(
                    "Everything accepts its complete native query syntax, including filename filters, paths, operators, and regular expressions.",
                    "Everything 支持完整的原生搜索语法，包括文件名筛选、路径、运算符和正则表达式。"
                )
            }}
        }
    }
end)
