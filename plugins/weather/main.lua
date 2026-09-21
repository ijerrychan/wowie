local cache = {}
local CACHE_SECONDS = 600

local function is_zh()
    local locale = wowie.locale("current") or ""
    return string.sub(locale, 1, 2) == "zh"
end

local function text(en, zh)
    return is_zh() and zh or en
end

local function weather_condition(code)
    if code == 0 then return text("Clear sky", "晴朗") end
    if code <= 3 then return text("Cloudy", "多云") end
    if code == 45 or code == 48 then return text("Fog", "雾") end
    if code <= 57 then return text("Drizzle", "毛毛雨") end
    if code <= 67 or (code >= 80 and code <= 82) then return text("Rain", "雨") end
    if code <= 77 then return text("Snow", "雪") end
    if code >= 95 then return text("Thunderstorm", "雷暴") end
    return text("Cloudy", "多云")
end

local function wind_direction(degrees)
    local directions = is_zh() and {"北", "东北", "东", "东南", "南", "西南", "西", "西北"}
        or {"N", "NE", "E", "SE", "S", "SW", "W", "NW"}
    local index = math.floor(((degrees or 0) + 22.5) / 45) % 8 + 1
    return directions[index]
end

local function weather_glyph(code)
    if code == 0 then return "☀" end
    if code <= 3 then return "☁" end
    if code == 45 or code == 48 then return "〰" end
    if code <= 67 or (code >= 80 and code <= 82) then return "☂" end
    if code <= 77 then return "❄" end
    if code >= 95 then return "⚡" end
    return "☁"
end

local function fetch()
    local location = wowie.http_get_json("https://ipapi.co/json/")
    if not location or not location.latitude or not location.longitude then
        location = wowie.http_get_json("https://ipwho.is/")
    end
    local latitude = location.latitude
    local longitude = location.longitude
    if not latitude or not longitude then error("location unavailable") end
    local url = "https://api.open-meteo.com/v1/forecast?latitude=" .. tostring(latitude)
        .. "&longitude=" .. tostring(longitude)
        .. "&current=temperature_2m,relative_humidity_2m,weather_code,wind_speed_10m,wind_direction_10m"
        .. "&temperature_unit=celsius&wind_speed_unit=kmh&timezone=auto"
    local weather = wowie.http_get_json(url)
    local current = weather.current or {}
    return {
        location = location.city or location.region or location.country_name or text("Current location", "当前位置"),
        temperature = current.temperature_2m or 0,
        humidity = current.relative_humidity_2m or 0,
        code = current.weather_code or 0,
        wind_speed = current.wind_speed_10m or 0,
        wind_direction = current.wind_direction_10m or 0,
    }
end

local function detail(item_id)
    local now = os.time()
    local data = cache.value
    if not data or now - (cache.time or 0) >= CACHE_SECONDS then
        local ok, result = pcall(fetch)
        if not ok then
            return {
                request_id = nil,
                detail = {
                    title = text("Weather", "天气"),
                    blocks = {{type = "error", message = text("Weather unavailable", "天气信息不可用")}}
                }
            }
        end
        data = result
        cache = {time = now, value = data}
    end
    local condition = weather_condition(data.code)
    return {
        detail = {
            title = text("Weather", "天气"),
            subtitle = data.location,
            source = "Open-Meteo · IP location",
            blocks = {
                {type = "icon", glyph = weather_glyph(data.code), color = "primary"},
                {type = "text", text = string.format("%.1f°C  %s", data.temperature, condition)},
                {type = "separator"},
                {type = "metric", label = text("Humidity", "湿度"), value = string.format("%d%%", data.humidity)},
                {type = "metric", label = text("Wind", "风力"), value = string.format("%s %.1f km/h", wind_direction(data.wind_direction), data.wind_speed)}
            }
        }
    }
end

wowie.on("query", function(query, context)
    local keyword = is_zh() and "天气" or "weather"
    local aliases = {keyword, is_zh() and "weather" or "天气"}
    local matches = false
    for _, alias in ipairs(aliases) do
        if wowie.fuzzy_match(alias, query) then matches = true end
    end
    if query == "" then
        return {
            request_id = context.request_id,
            items = {wowie.result({
                id = "current",
                title = text("Weather", "天气"),
                subtitle = text("View weather information", "查看天气信息"),
                aliases = aliases,
                category = "other",
                visible_when = "empty_query",
                icon = {type = "named", id = "weather"},
                actions = {wowie.action("open_detail")}
            })}
        }
    end
    if not matches then
        return {request_id = context.request_id, items = {}}
    end
    return {
        request_id = context.request_id,
        items = {wowie.result({
            id = "current",
            title = text("Weather", "天气"),
            subtitle = text("View weather information", "查看天气信息"),
            aliases = aliases,
            category = "other",
            icon = {type = "named", id = "weather"},
            actions = {wowie.action("open_detail")}
        })}
    }
end)

wowie.on("detail", function(item_id, context)
    local result = detail(item_id)
    result.request_id = context.request_id
    return result
end)
