# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "create_directory",
  "Create a directory and any missing parent directories " \
  "(equivalent to `mkdir -p`). Safe to call even if the directory " \
  "already exists.",
  {
    "type" => "object",
    "properties" => {
      "path" => { "type" => "string", "description" => "Directory path to create" }
    },
    "required" => ["path"]
  }
) do |args|
  path = args["path"] or raise ArgumentError, "missing 'path'"
  Shio.create_dir_all(path)
  "Created directory: #{path}"
end
