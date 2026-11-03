local COMMAND_ID = "search"
local EVERYTHING_ICON_TARGET = wowie.everything_icon_target()

local function is_zh(locale)
    locale = locale or ""
    return string.sub(locale, 1, 2) == "zh"
end

local function text(en, zh, locale)
    return is_zh(locale) and zh or en
end

local function icon(backend)
    if backend == "everything" and EVERYTHING_ICON_TARGET then
        return {type = "system", target = EVERYTHING_ICON_TARGET}
    end
    return {type = "named", id = "folder-open"}
end

local function selected_backend(context)
    local preferences = context and context.preferences or {}
    return wowie.file_search_backend(preferences.backend or "auto")
end

local function backend_title(backend, locale)
    if backend == "everything" then
        return text("Search with Everything", "使用 Everything 搜索", locale)
    elseif backend == "windows-index" then
        return text("Search files", "文件搜索", locale)
    elseif backend == "everything-unavailable" then
        return text("Everything is unavailable", "Everything 当前不可用", locale)
    end
    return text("File search unavailable", "文件搜索不可用", locale)
end

local function backend_description(backend, locale)
    if backend == "everything" then
        return text(
            "Search files with Everything; all Everything syntax is supported",
            "使用 Everything 搜索文件，支持全部 Everything 语法",
            locale
        )
    elseif backend == "windows-index" then
        return text(
            "Search file names in locations included in the Windows Search index",
            "使用 Windows 文件索引搜索文件名；结果受索引位置设置限制",
            locale
        )
    elseif backend == "everything-unavailable" then
        return text(
            "Start Everything or choose Automatic or Windows Search in settings",
            "请启动 Everything，或在设置中选择“自动”或“Windows Search”",
            locale
        )
    end
    return text("File search is available on Windows", "文件搜索目前仅支持 Windows", locale)
end

local function entry(query, backend, locale)
    local empty = query == ""
    local title = backend_title(backend, locale)
    return wowie.result({
        id = "entry",
        title = empty and title or title .. " \"" .. query .. "\"",
        subtitle = backend_description(backend, locale),
        aliases = {"Everything", "everything", "File Search", "文件搜索"},
        category = "other",
        command_id = COMMAND_ID,
        session_entry = true,
        visible_when = empty and "empty_query" or "always",
        ranking_bias = empty and 0 or -1000,
        icon = icon(backend)
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
    -- recognized, only the active session performs the file search.
    if context.command_id ~= COMMAND_ID then
        local backend = selected_backend(context)
        return {
            request_id = context.request_id,
            items = {entry(query, backend, context.locale)}
        }
    end

    if query == "" then
        return {request_id = context.request_id, items = {}}
    end

    -- Keep each search page small; the host can request another page as the
    -- result list approaches its end.
    local limit = math.min(context.limit or 30, 30)
    local preferences = context.preferences or {}
    local rows = wowie.file_search(query, context.offset or 0, limit, preferences.backend or "auto")
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
    local backend = selected_backend(context)
    local target = backend == "everything" and wowie.everything_icon_target() or nil
    return {
        request_id = context.request_id,
        icon = target and {type = "system", target = target} or icon(backend)
    }
end)

wowie.on("empty_state", function(context)
    local backend = selected_backend(context)
    return {
        request_id = context.request_id,
        title = backend_title(backend, context.locale),
        description = backend_description(backend, context.locale),
        icon = icon(backend)
    }
end)
