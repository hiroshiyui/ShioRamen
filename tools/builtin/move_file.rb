# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "move_file",
  "Move or rename a file or directory.",
  {
    "type" => "object",
    "properties" => {
      "src" => { "type" => "string", "description" => "Source path" },
      "dst" => { "type" => "string", "description" => "Destination path" }
    },
    "required" => ["src", "dst"]
  }
) do |args|
  src = args["src"] or raise ArgumentError, "missing 'src'"
  dst = args["dst"] or raise ArgumentError, "missing 'dst'"
  Shio.rename(src, dst)
  "Moved #{src} → #{dst}"
end
