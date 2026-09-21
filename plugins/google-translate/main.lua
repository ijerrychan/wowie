-- The host supplies context.session only while this search-engine session is
-- active. session_updates below make ordinary result rows behave as menus.
local languages = {
    { code = "AUTO", name = "自动检测" },
    { code = "ZH", name = "中文" },
    { code = "EN", name = "英语" },
    { code = "JA", name = "日语" },
    { code = "DE", name = "德语" },
    { code = "FR", name = "法语" },
    { code = "ES", name = "西班牙语" }
}

local function language_name(code)
    for _, language in ipairs(languages) do
        if language.code == code then return language.name end
    end
    return code
end

local google_codes = {
    AUTO = "auto", ZH = "zh-CN", EN = "en", JA = "ja",
    DE = "de", FR = "fr", ES = "es"
}
local GOOGLE_TRANSLATE_URL = "https://translate-pa.googleapis.com/v1/translateHtml"
local GOOGLE_TRANSLATE_API_KEY = "AIzaSyATBXajvzQLTDHEQbcpq0Ihe0vWDHmO520"

local function translate(text, source, target)
    if text == "" then return "输入要翻译的内容" end
    local body = {
        { { text }, google_codes[source] or "auto", google_codes[target] or "en" },
        "wt_lib"
    }
    local headers = {
        ["content-type"] = "application/json+protobuf",
        ["x-goog-api-key"] = GOOGLE_TRANSLATE_API_KEY
    }
    local ok, response = pcall(wowie.http_post_json,
        GOOGLE_TRANSLATE_URL, body, headers)
    if not ok then return "谷歌翻译请求失败：" .. tostring(response) end
    local translated = {}
    local result = response[1]
    if type(result) == "table" then
        for _, segment in ipairs(result) do
            if type(segment) == "string" then
                translated[#translated + 1] = segment
            elseif type(segment) == "table" and segment[1] then
                translated[#translated + 1] = segment[1]
            end
        end
    elseif type(result) == "string" then
        translated[#translated + 1] = result
    end
    return #translated > 0 and table.concat(translated) or "谷歌翻译没有返回译文。"
end

local function language_menu(kind, state)
    local items = {}
    for _, language in ipairs(languages) do
        if kind == "source" and language.code == "AUTO" or language.code ~= "AUTO" then
            items[#items + 1] = wowie.result({
                id = kind .. "-" .. language.code,
                title = language.name,
                subtitle = language.code,
                visible_when = "empty_query",
                session_updates = { [kind] = language.code, menu = "" }
            })
        end
    end
    return items
end

wowie.on("query", function(query, context)
    if context.command_id ~= "translate" then
        -- The translation session is entered with `t `. Do not fill an empty
        -- root search with a generic command entry.
        return {
            request_id = context.request_id,
            items = {
                wowie.result({
                    id = "entry",
                    title = query:match("^%s*$") and "Google Translate" or "用谷歌翻译搜索 \"" .. query .. "\"",
                    subtitle = query:match("^%s*$") and "Translate text with Google Translate" or "翻译文本",
                    category = "other",
                    command_id = "translate",
                    session_entry = true,
                    ranking_bias = query:match("^%s*$") and 0 or -1000,
                    visible_when = query:match("^%s*$") and "empty_query" or "always"
                })
            }
        }
    end
    local state = type(context.session) == "table" and context.session or {}
    local source = state.source or "AUTO"
    local target = state.target or "EN"
    if state.menu == "source" then return { request_id = context.request_id, items = language_menu("source", state) } end
    if state.menu == "target" then return { request_id = context.request_id, items = language_menu("target", state) } end

    return {
        request_id = context.request_id,
        items = {
            wowie.result({
                id = "translation",
                title = "翻译",
                subtitle = "Google Translate：" .. source .. " → " .. target,
                -- Session rows still pass through the host ranker. Keep the
                -- primary answer first without bypassing normal ordering.
                category = "other",
                ranking_bias = 20,
                detail = { title = "谷歌翻译", blocks = {
                    { type = "text", text = translate(query, source, target), color = "primary", copyable = true }
                }, scrollable = true }
            }),
            wowie.result({ id = "source", title = "源语言", subtitle = language_name(source), category = "other", session_updates = { menu = "source" } }),
            wowie.result({ id = "target", title = "目标语言", subtitle = language_name(target), category = "other", session_updates = { menu = "target" } })
        }
    }
end)
