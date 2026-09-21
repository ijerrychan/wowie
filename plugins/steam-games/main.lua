-- Steam is a normal provider plugin: it has no command and no search-engine
-- session.  The host only supplies generic, permission-gated file APIs;
-- discovery and matching stay entirely in this plugin.

local games = {}

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

local function cached_icon(path, appid, depth)
  if depth > 2 then return nil end
  for _, entry in ipairs(wowie.fs_list(path)) do
    local name = lower(entry.name)
    if not entry.is_dir then
      if name == string.lower(appid .. "_icon.jpg")
          or name == string.lower(appid .. "_icon.png") then
        return entry.path
      end
    end
  end
  -- Steam's newer cache stores hashed images under librarycache/<appid>/.
  if depth == 0 then
    local nested_root = join(path, appid)
    for _, entry in ipairs(wowie.fs_list(nested_root)) do
      local name = lower(entry.name)
      if not entry.is_dir and (string.match(name, "^%x+%.jpg$")
          or string.match(name, "^%x+%.png$")) then
        return entry.path
      end
    end
  end
  return nil
end

local function discover()
  local found, seen = {}, {}
  local icon_roots = {}
  for _, env_name in ipairs({"PROGRAMFILES(X86)", "PROGRAMFILES", "LOCALAPPDATA"}) do
    local base = wowie.env(env_name)
    if base then icon_roots[#icon_roots + 1] = join(base, "Steam") end
  end
  for _, library in ipairs(library_roots()) do
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
              local cache = join(join(library, "appcache"), "librarycache")
              local icon_path = cached_icon(cache, appid, 0)
              if not icon_path then
                for _, root in ipairs(icon_roots) do
                  icon_path = cached_icon(
                    join(join(root, "appcache"), "librarycache"), appid, 0
                  )
                  if icon_path then break end
                end
              end
              local exe = executable(install, 0, nil)
              found[#found + 1] = {
                id = "steam-" .. appid,
                title = name,
                category = "game",
                subtitle = "Steam",
                aliases = {appid},
                target = "steam://rungameid/" .. appid,
                icon = icon_path and {type = "file", path = icon_path}
                    or (exe and {type = "system", target = exe.path} or nil),
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
  return found
end

wowie.on("initialize", function(_context)
  games = discover()
end)

wowie.on("query", function(query, context)
  local needle = lower(query)
  -- Discovery is cached, but rendering every installed game on an empty root
  -- query still drowns out useful favorites and recents from other engines.
  if needle == "" then
    return {request_id = context.request_id, items = {}}
  end
  local items = {}
  for _, game in ipairs(games) do
    if wowie.fuzzy_match(game.title, needle) or wowie.fuzzy_match(game.aliases[1], needle) then
      items[#items + 1] = wowie.result(game)
    end
  end
  return {request_id = context.request_id, items = items}
end)
