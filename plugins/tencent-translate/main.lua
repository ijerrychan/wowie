-- Tencent Hy-MT2 translation through the TokenHub translation API.
-- Preferences are supplied by the host from config.json; see README.md.
local languages = {
    { code = "AUTO", zh = "自动检测", en = "Auto detect" },
    { code = "zh-Hans", zh = "简体中文", en = "Chinese (Simplified)" },
    { code = "zh-Hant", zh = "繁体中文", en = "Chinese (Traditional)" },
    { code = "en", zh = "英语", en = "English" },
    { code = "ja", zh = "日语", en = "Japanese" },
    { code = "ko", zh = "韩语", en = "Korean" },
    { code = "fr", zh = "法语", en = "French" },
    { code = "de", zh = "德语", en = "German" },
    { code = "es", zh = "西班牙语", en = "Spanish" },
    { code = "pt", zh = "葡萄牙语", en = "Portuguese" },
    { code = "ru", zh = "俄语", en = "Russian" },
    { code = "it", zh = "意大利语", en = "Italian" },
    { code = "ar", zh = "阿拉伯语", en = "Arabic" },
    { code = "hi", zh = "印地语", en = "Hindi" },
    { code = "th", zh = "泰语", en = "Thai" },
    { code = "vi", zh = "越南语", en = "Vietnamese" },
    { code = "id", zh = "印尼语", en = "Indonesian" },
    { code = "tr", zh = "土耳其语", en = "Turkish" },
    { code = "pl", zh = "波兰语", en = "Polish" },
    { code = "cs", zh = "捷克语", en = "Czech" },
    { code = "nl", zh = "荷兰语", en = "Dutch" },
    { code = "ms", zh = "马来语", en = "Malay" },
    { code = "fil", zh = "菲律宾语", en = "Filipino" },
    { code = "km", zh = "高棉语", en = "Khmer" },
    { code = "my", zh = "缅甸语", en = "Burmese" },
    { code = "fa", zh = "波斯语", en = "Persian" },
    { code = "gu", zh = "古吉拉特语", en = "Gujarati" },
    { code = "ur", zh = "乌尔都语", en = "Urdu" },
    { code = "te", zh = "泰卢固语", en = "Telugu" },
    { code = "mr", zh = "马拉地语", en = "Marathi" },
    { code = "he", zh = "希伯来语", en = "Hebrew" },
    { code = "bn", zh = "孟加拉语", en = "Bengali" },
    { code = "ta", zh = "泰米尔语", en = "Tamil" },
    { code = "uk", zh = "乌克兰语", en = "Ukrainian" },
    { code = "bo", zh = "藏语", en = "Tibetan" },
    { code = "kk", zh = "哈萨克语", en = "Kazakh" },
    { code = "mn", zh = "蒙古语", en = "Mongolian" },
    { code = "ug", zh = "维吾尔语", en = "Uyghur" },
    { code = "yue", zh = "粤语", en = "Cantonese" }
}

local function is_chinese(locale)
    return type(locale) == "string" and locale:lower():match("^zh") ~= nil
end

local function label(chinese, english, locale)
    return is_chinese(locale) and chinese or english
end

local function language(code)
    for _, item in ipairs(languages) do
        if item.code == code then return item end
    end
    return nil
end

local function language_name(code, locale, kind)
    if code == "AUTO" then
        if kind == "target" then
            return label("自动选择", "Auto select", locale)
        end
        return label("自动检测", "Auto detect", locale)
    end
    local item = language(code)
    if item then return is_chinese(locale) and item.zh or item.en end
    return code
end

local function normalized_language(code)
    local value = tostring(code or ""):lower():gsub("_", "-")
    if value == "zh" or value == "zh-cn" or value == "zh-sg" or value == "zh-hans" then
        return "zh-hans"
    end
    if value == "zh-tw" or value == "zh-hk" or value == "zh-mo" or value == "zh-hant" or value == "zh-tr" then
        return "zh-hant"
    end
    return value:match("^[^-]+") or value
end

local function api_language(code)
    if code == "zh-Hans" then return "zh" end
    if code == "zh-Hant" then return "zh-TR" end
    return code
end

local function display_language(code)
    if type(code) ~= "string" then return "AUTO" end
    local normalized = normalized_language(code)
    if normalized == "zh-hans" then return "zh-Hans" end
    if normalized == "zh-hant" then return "zh-Hant" end
    return normalized
end

local function locale_target(locale)
    local value = tostring(locale or "en"):lower():gsub("_", "-")
    if value:match("^zh") then
        if value:match("hant") or value:match("tw") or value:match("hk") or value:match("mo") then
            return "zh-Hant"
        end
        return "zh-Hans"
    end
    local base = value:match("^[^-]+") or "en"
    if language(base) then return base end
    return "en"
end

local function alternate_target(code)
    local normalized = normalized_language(code)
    if normalized == "en" then
        return "zh-Hans"
    end
    return "en"
end

local function preference_string(preferences, name)
    local value = type(preferences) == "table" and preferences[name] or nil
    return type(value) == "string" and value:match("^%s*(.-)%s*$") or ""
end

local function request_translation(text, source, target, preferences, locale)
    local api_key = preference_string(preferences, "api_key")
    if api_key == "" then
        return nil, label(
            "尚未配置 TokenHub API Key。请按插件 README 设置 config.json。",
            "TokenHub API key is missing. Configure it in config.json as described in the plugin README.",
            locale
        )
    end

    local endpoint = preference_string(preferences, "endpoint")
    if endpoint == "" then endpoint = "https://tokenhub.tencentmaas.com/v1/api/translations" end
    endpoint = endpoint:gsub("/+$", "")
    if endpoint:match("/v1$") then
        endpoint = endpoint .. "/api/translations"
    elseif not endpoint:match("/v1/api/translations$") then
        endpoint = endpoint .. "/v1/api/translations"
    end

    local model = preference_string(preferences, "model")
    if model == "" then model = "hy-mt2-plus" end
    local payload = {
        model = model,
        text = text,
        target = api_language(target)
    }
    if source ~= "AUTO" then
        payload.source = api_language(source)
    end

    local headers = {
        ["Authorization"] = "Bearer " .. api_key,
        ["Content-Type"] = "application/json"
    }

    local ok, response = pcall(wowie.http_post_json, endpoint, payload, headers)
    if not ok then
        return nil, label("腾讯翻译请求失败：", "Tencent Translator request failed: ", locale) .. tostring(response)
    end
    local choices = type(response) == "table" and response.choices or nil
    local first = type(choices) == "table" and choices[1] or nil
    local message = type(first) == "table" and first.message or nil
    if type(message) ~= "table" or type(message.content) ~= "string" then
        local api_error = type(response) == "table" and response.error or nil
        local detail = type(api_error) == "table" and api_error.message or nil
        if type(detail) == "string" and detail ~= "" then
            return nil, label("腾讯翻译请求失败：", "Tencent Translator request failed: ", locale) .. detail
        end
        return nil, label("腾讯翻译没有返回译文。", "Tencent Translator returned no translation.", locale)
    end
    if first.finish_reason == "sensitive" then
        return nil, label("腾讯翻译无法处理这段内容。", "Tencent Translator could not process this content.", locale)
    end
    return {
        text = message.content,
        detected = display_language(response.source),
        target = display_language(response.target)
    }
end

local function translate(text, source, target, locale, preferences)
    if text:match("^%s*$") then
        return label("输入要翻译的内容。", "Enter text to translate."), nil, nil
    end

    local automatic_target = target == "AUTO"
    local destination = automatic_target and locale_target(locale) or target
    local api_source = source

    -- When the source is explicitly known to match the default destination,
    -- avoid an unnecessary first request and translate to the other common
    -- language directly.
    if automatic_target and source ~= "AUTO" and normalized_language(source) == normalized_language(destination) then
        destination = alternate_target(destination)
    end

    local result, err = request_translation(text, api_source, destination, preferences, locale)
    if not result then return err, nil, nil end

    local detected = source ~= "AUTO" and source or result.detected or "AUTO"
    if automatic_target and source == "AUTO" and detected ~= "AUTO"
        and normalized_language(detected) == normalized_language(destination) then
        destination = alternate_target(destination)
        result, err = request_translation(text, "AUTO", destination, preferences, locale)
        if not result then return err, detected, destination end
    end
    return result.text, detected, destination
end

local function language_menu(kind, selected, query, menu_query, locale)
    local items = {}
    local normalized_query = query:match("^%s*(.-)%s*$")
    -- Opening a language menu keeps the translation text in the search box.
    -- Preserve the full menu for that initial query, then use later edits as
    -- language-name/code filters.
    local inherited_query = query == (menu_query or "")
    local ordered = {}
    for _, item in ipairs(languages) do
        if item.code == selected then ordered[#ordered + 1] = item end
    end
    for _, item in ipairs(languages) do
        if item.code ~= selected then ordered[#ordered + 1] = item end
    end

    for _, item in ipairs(ordered) do
        local name = language_name(item.code, locale, kind)
        local matches = inherited_query or normalized_query == ""
            or wowie.fuzzy_match(name, normalized_query)
            or wowie.fuzzy_match(item.code, normalized_query)
            or wowie.fuzzy_match(item.zh, normalized_query)
            or wowie.fuzzy_match(item.en, normalized_query)
        if matches then
            local current = item.code == selected
            local title = (current and "✓ " or "") .. name
            local subtitle = item.code
            local aliases = { item.code, item.en, item.zh }
            if inherited_query and normalized_query ~= "" then
                aliases[#aliases + 1] = query
            end
            if current then
                subtitle = subtitle .. " · " .. label("当前选择", "Current", locale)
            end
            items[#items + 1] = wowie.result({
                id = kind .. "-" .. item.code,
                title = title,
                subtitle = subtitle,
                aliases = aliases,
                category = "other",
                ranking_bias = current and 30 or 0,
                session_updates = { [kind] = item.code, menu = "", menu_query = "" }
            })
        end
    end
    return items
end

wowie.on("query", function(query, context)
    local locale = context.locale or "en"
    local translator_name = label("腾讯翻译", "Tencent Translator", locale)
    if context.command_id ~= "translate" then
        return {
            request_id = context.request_id,
            items = {
                wowie.result({
                    id = "entry",
                    title = query:match("^%s*$") and translator_name or label("用腾讯翻译搜索 \"" .. query .. "\"", "Search with Tencent Translator for \"" .. query .. "\"", locale),
                    subtitle = query:match("^%s*$") and label("输入文本开始翻译", "Enter text to translate", locale) or label("腾讯翻译文本", "Tencent translation", locale),
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
    local target = state.target or "AUTO"
    if state.menu == "source" then
        return {
            request_id = context.request_id,
            items = language_menu("source", source, query, state.menu_query, locale)
        }
    end
    if state.menu == "target" then
        return {
            request_id = context.request_id,
            items = language_menu("target", target, query, state.menu_query, locale)
        }
    end

    local translated, detected, actual_target = translate(query, source, target, locale, context.preferences)
    local source_label = language_name(detected or source, locale, "source")
    local target_label = language_name(target == "AUTO" and (actual_target or locale_target(locale)) or target, locale, "target")
    local items = {
        wowie.result({
            id = "translation",
            title = label("翻译", "Translation", locale),
            subtitle = translator_name .. " · " .. source_label .. " → " .. target_label,
            category = "other",
            ranking_bias = 20,
            icon = { type = "resource", path = "icons/tencent-translate.svg" },
            detail = {
                title = translator_name,
                blocks = {{ type = "text", text = translated, color = "primary", copyable = true }},
                scrollable = true
            }
        }),
        wowie.result({
            id = "source",
            title = label("源语言", "Source language", locale),
            subtitle = language_name(source, locale, "source"),
            category = "other",
            session_updates = { menu = "source", menu_query = query }
        }),
        wowie.result({
            id = "target",
            title = label("目标语言", "Target language", locale),
            subtitle = language_name(target, locale, "target"),
            category = "other",
            session_updates = { menu = "target", menu_query = query }
        })
    }
    return { request_id = context.request_id, items = items }
end)
