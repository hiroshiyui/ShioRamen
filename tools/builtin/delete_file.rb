# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "delete_file",
  "Delete a file from the filesystem.",
  {
    "type" => "object",
    "properties" => {
      "path" => { "type" => "string", "description" => "Path to the file to delete" }
    },
    "required" => ["path"]
  }
) do |args|
  path = args["path"] or raise ArgumentError, "missing 'path'"
  Shio.delete_file(path)
  "Deleted #{path}"
end
