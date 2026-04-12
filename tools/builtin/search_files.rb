# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "search_files",
  "Find files by glob pattern (e.g. \"src/**/*.rs\").",
  {
    "type" => "object",
    "properties" => {
      "pattern" => { "type" => "string" },
      "path" => {
        "type"        => "string",
        "description" => "Root directory to search in (default: .)"
      }
    },
    "required" => ["pattern"]
  }
) do |args|
  pattern = args["pattern"] or raise "missing 'pattern' argument"
  base = (args["path"] || ".").chomp("/")
  full_pattern = base == "." ? pattern : "#{base}/#{pattern}"
  result = Shio.glob(full_pattern)
  result.empty? ? "(no matches)" : result
end
