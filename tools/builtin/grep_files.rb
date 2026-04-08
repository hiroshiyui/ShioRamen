# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "grep_files",
  "Search for a regex pattern in files and return matching lines with line numbers.",
  {
    "type" => "object",
    "properties" => {
      "pattern"          => { "type" => "string",  "description" => "Regex pattern" },
      "path"             => { "type" => "string",  "description" => "File or directory to search (default: .)" },
      "case_insensitive" => { "type" => "boolean" }
    },
    "required" => ["pattern"]
  }
) do |args|
  pattern = args["pattern"] or raise "missing 'pattern' argument"
  path    = args["path"] || "."
  ci      = args["case_insensitive"] ? true : false
  result  = Shio.grep(pattern, path, ci)
  result.empty? ? "(no matches)" : result
end
