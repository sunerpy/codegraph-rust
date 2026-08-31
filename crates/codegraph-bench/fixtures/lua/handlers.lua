local function helper()
  return 1
end

local localFn = function()
  return helper()
end

local M = {
  callbacks = {
    onStart = function()
      return helper()
    end,
    ["onStop"] = function()
      return helper()
    end,
    [DYNAMIC] = function()
      return helper()
    end,
  },
}

M.assignedFn = function()
  return helper()
end

M["bracketFn"] = function()
  return helper()
end

localFn()
M.assignedFn()
M:assignedFn()

return M
