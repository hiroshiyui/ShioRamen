# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "list_directory",
  "List files and directories inside a directory.",
  {
    "type" => "object",
    "properties" => {
      "path" => {
        "type" => "string",
        "description" => "Directory path (default: current directory)"
      }
    }
  }
) do |args|
  path = args["path"] || "."
  entries = Shio.read_dir(path)
  entries.empty? ? "(empty)" : entries
end
