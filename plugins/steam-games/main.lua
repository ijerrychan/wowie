-- Steam is a normal provider plugin: it has no command and no search-engine
-- session.  The host only supplies generic, permission-gated file APIs;
-- discovery and matching stay entirely in this plugin.

local games = {}
local match_candidates = {}
local last_refresh = 0
local last_attempt = 0
local refresh_seconds = 10 * 60
local CACHE_KEY = "steam-games-v2"

local function index_games()
  match_candidates = {}
  for index, game in ipairs(games) do
    match_candidates[index] = {game.title, game.aliases[1]}
  end
end

local function lower(value)
  return string.lower(value or "")
end

local function join(base, child)
  if string.sub(base, -1) == "\\" or string.sub(base, -1) == "/" then
    return base .. child
  end
  return base .. "\\" .. child
end

local function unique_append(list, seen, value)
  if value and value ~= "" and not seen[lower(value)] then
    seen[lower(value)] = true
    list[#list + 1] = value
  end
end

local function field(text, name)
  local pattern = '"' .. name .. '"%s+"([^"]*)"'
  local value = string.match(text, pattern)
  if not value then return nil end
  -- Steam VDF escapes Windows separators as `\\`; collapse them before
  -- passing the path to the host file/icon APIs.
  return string.gsub(value, "\\\\", "\\")
end

local function vdf_path(value)
  if not value then return nil end
  return string.gsub(value, "\\\\", "\\")
end

local function library_roots()
  local roots, seen = {}, {}
  local env_names = {"PROGRAMFILES(X86)", "PROGRAMFILES", "LOCALAPPDATA"}
  for _, env_name in ipairs(env_names) do
    local base = wowie.env(env_name)
    if base then
      unique_append(roots, seen, join(base, "Steam"))
    end
  end

  local result, result_seen = {}, {}
  for _, root in ipairs(roots) do
    unique_append(result, result_seen, root)
    local text = wowie.fs_read_text(join(root, "steamapps\\libraryfolders.vdf"))
    if text then
      for path in string.gmatch(text, '"path"%s+"([^"]+)"') do
        unique_append(result, result_seen, vdf_path(path))
      end
    end
  end
  return result
end

local function executable(path, depth, best)
  if depth > 2 then return best end
  local entries = wowie.fs_list(path)
  for _, entry in ipairs(entries) do
    if entry.is_dir then
      best = executable(entry.path, depth + 1, best)
    else
      local name = lower(entry.name)
      if string.sub(name, -4) == ".exe"
          and not string.find(name, "unins", 1, true)
          and not string.find(name, "setup", 1, true)
          and not string.find(name, "install", 1, true)
          and not string.find(name, "redist", 1, true)
          and (not best or (entry.size or 0) > best.size) then
        best = {path = entry.path, size = entry.size or 0}
      end
    end
  end
  return best
end

local function icon_index(roots, installed)
  local icons = {}
  local nested = {}
  for _, root in ipairs(roots) do
    for _, entry in ipairs(wowie.fs_list(root)) do
      local name = lower(entry.name)
      if entry.is_dir and installed[name] then
        nested[#nested + 1] = {appid = name, path = entry.path}
      elseif not entry.is_dir then
        local appid = string.match(name, "^(%d+)_icon%.png$")
            or string.match(name, "^(%d+)_icon%.jpg$")
        if appid and installed[appid] and not icons[appid] then
          icons[appid] = entry.path
        end
      end
    end
  end
  -- Prefer the stable *_icon file. Newer Steam caches keep hashed images in
  -- librarycache/<appid>; inspect only directories for installed games.
  for _, entry in ipairs(nested) do
    if not icons[entry.appid] then
      for _, image in ipairs(wowie.fs_list(entry.path)) do
        local name = lower(image.name)
        if not image.is_dir and (string.match(name, "^%x+%.jpg$")
            or string.match(name, "^%x+%.png$")) then
          icons[entry.appid] = image.path
          break
        end
      end
    end
  end
  return icons
end

local function discover()
  local found, seen, installed = {}, {}, {}
  local libraries = library_roots()
  for _, library in ipairs(libraries) do
    local steamapps = join(library, "steamapps")
    for _, entry in ipairs(wowie.fs_list(steamapps)) do
      local appid = string.match(entry.name, "^appmanifest_(%d+)%.acf$")
      if appid then
        local text = wowie.fs_read_text(entry.path)
        if text then
          local name = field(text, "name")
          local installdir = field(text, "installdir")
          if name and installdir and appid ~= "228980"
              and not string.find(lower(name), "redistributable", 1, true) then
            local install = join(join(steamapps, "common"), installdir)
            if not seen[appid] then
              seen[appid] = true
              installed[appid] = true
              local exe = executable(install, 0, nil)
              found[#found + 1] = {
                id = "steam-" .. appid,
                title = name,
                category = "game",
                subtitle = "Steam",
                aliases = {appid},
                target = "steam://rungameid/" .. appid,
                icon = exe and {type = "system", target = exe.path} or nil,
                -- Category and user history are sufficient root tie-breakers;
                -- do not hard-code a penalty for every game.
                ranking_bias = 0
              }
            end
          end
        end
      end
    end
  end
  local icon_roots, root_seen = {}, {}
  for _, library in ipairs(libraries) do
    unique_append(icon_roots, root_seen, join(join(library, "appcache"), "librarycache"))
  end
  local icons = icon_index(icon_roots, installed)
  for _, game in ipairs(found) do
    local appid = game.aliases[1]
    if icons[appid] then
      game.icon = {type = "file", path = icons[appid]}
    end
  end
  return found
end

local function configure_refresh(context)
  local preferences = type(context) == "table" and context.preferences or nil
  local value = preferences and tonumber(preferences.refresh_minutes) or nil
  if value and value == value then
    refresh_seconds = math.floor(math.max(1, math.min(1440, value))) * 60
  end
end

local function refresh(now)
  last_attempt = now
  local ok, found = pcall(discover)
  if not ok then return false end
  -- An empty result is valid when Steam has been uninstalled.
  games = found
  index_games()
  last_refresh = now
  pcall(wowie.storage_write, CACHE_KEY, {saved_at = now, games = games})
  return true
end

wowie.on("initialize", function(context)
  configure_refresh(context)
  local now = os.time()
  local ok, cached = pcall(wowie.storage_read, CACHE_KEY)
  if ok and cached and type(cached.games) == "table"
      and type(cached.saved_at) == "number" then
    games = cached.games
    index_games()
    last_refresh = cached.saved_at
  end
  if (now - last_refresh >= refresh_seconds or last_refresh > now)
      and (now - last_attempt >= 60 or last_attempt > now) then
    refresh(now)
  end
end)

wowie.on("query", function(query, context)
  configure_refresh(context)
  local now = os.time()
  if (now - last_refresh >= refresh_seconds or last_refresh > now)
      and (now - last_attempt >= 60 or last_attempt > now) then
    refresh(now)
  end
  local needle = lower(query)
  -- Discovery is cached, but rendering every installed game on an empty root
  -- query still drowns out useful favorites and recents from other engines.
  if needle == "" then
    return {request_id = context.request_id, items = {}}
  end
  local items = {}
  for _, index in ipairs(wowie.fuzzy_filter(match_candidates, needle)) do
    items[#items + 1] = wowie.result(games[index])
  end
  return {request_id = context.request_id, items = items}
end)
