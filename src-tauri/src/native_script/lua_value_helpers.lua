json = require 'json'

local decodeJSON
do
    local original = json
    local decode = json.decode
    local kind, rawnext = type, next
    local getmeta = getmetatable
    local stringMeta = getmeta('')
    local stringIndex = stringMeta.__index
    local originals = {string.byte, string.find, string.sub, string.match, string.format, string.char, math.floor,
        tonumber, tostring, error, type, pairs, table.insert, table.concat}
    local entries, size = {}, 0
    local function copy(value)
        if kind(value) ~= 'table' then return value end
        local result = {}
        for key, item in rawnext, value do result[key] = copy(item) end
        return result
    end
    decodeJSON = function(value)
        local current = {string.byte, string.find, string.sub, string.match, string.format, string.char, math.floor,
            tonumber, tostring, error, type, pairs, table.insert, table.concat}
        if json ~= original or json.decode ~= decode then return json.decode(value) end
        if getmeta('') ~= stringMeta or stringMeta.__index ~= stringIndex then return json.decode(value) end
        for i = 1, #originals do
            if current[i] ~= originals[i] then return json.decode(value) end
        end
        if kind(value) ~= 'string' or #value > 512 * 1024 then return decode(value) end
        for i = 1, #entries do
            if entries[i].source == value then return copy(entries[i].value) end
        end
        local result = decode(value)
        if #entries >= 4 or size + #value > 512 * 1024 then entries, size = {}, 0 end
        entries[#entries + 1] = {source = value, value = result}
        size = size + #value
        return copy(result)
    end
end

function getChat(id, index)
    return decodeJSON(getChatMain(id, index))
end

function getFullChat(id)
    return decodeJSON(getFullChatMain(id))
end

function getRecentChats(id, count)
    return decodeJSON(getRecentChatsMain(id, count))
end

function getState(id, name)
    local escapedName = "__"..name
    return decodeJSON(getChatVar(id, escapedName))
end

function setState(id, name, value)
    local escapedName = "__"..name
    setChatVar(id, escapedName, json.encode(value))
end

function setStateChanged(id, name, value)
    local escapedName = "__"..name
    return setChatVarChanged(id, escapedName, json.encode(value))
end

