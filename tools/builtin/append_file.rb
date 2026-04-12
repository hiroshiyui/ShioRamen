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
  raise "missing 'content' argument" if args["content"].nil?
  content = args["content"].to_s
  Shio.append_file(path, content)

  # Report the new total line count so the caller has a correct anchor
  # for any follow-up insert_after_line call. Use bytesize (not length,
  # which returns characters in Ruby) so the byte count is accurate for
  # non-ASCII content.
  full = Shio.read_file(path)
  lines = full.split("\n", -1)
  lines.pop if full.end_with?("\n") && lines.last == ""
  "Appended #{content.bytesize} bytes to #{path} " \
    "(file now has #{lines.length} lines)"
end
