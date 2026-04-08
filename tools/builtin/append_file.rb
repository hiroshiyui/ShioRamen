# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "append_file",
  "Append content to the end of a file, creating it if it does not exist. " \
  "Use this ONLY when you need to add content at the very end and " \
  "do not know or care about the insertion line number. " \
  "If you know which line to insert after (e.g. from read_file_range), " \
  "use insert_after_line instead. " \
  "Do NOT use this to replace, rewrite, or refactor existing lines — " \
  "use patch_file for in-place edits.",
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
  Shio.append_file(path, content)
  "Appended #{content.length} bytes to #{path}"
end
