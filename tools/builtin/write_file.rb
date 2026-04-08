# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "write_file",
  "Write content to a file, creating it or overwriting it.",
  {
    "type" => "object",
    "properties" => {
      "path"    => { "type" => "string" },
      "content" => { "type" => "string" }
    },
    "required" => ["path", "content"]
  }
) do |args|
  path    = args["path"]    or raise "missing 'path' argument"
  content = args["content"] or raise "missing 'content' argument"
  Shio.write_file(path, content)
  "Wrote #{content.length} bytes to #{path}"
end
