# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "read_file",
  "Read the full contents of a file from the filesystem.",
  {
    "type" => "object",
    "properties" => {
      "path" => { "type" => "string", "description" => "Path to the file" }
    },
    "required" => ["path"]
  }
) do |args|
  path = args["path"] or raise ArgumentError, "missing 'path'"
  Shio.read_file(path)
end
