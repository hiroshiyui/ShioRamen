# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "insert_after_line",
  "Insert new content immediately after a specific line number in a file. " \
  "Use this when you need to add lines at a precise position — for example, " \
  "right after a range you read with read_file_range. Lines are 1-indexed. " \
  "The content is inserted after the given line; existing lines below that " \
  "point are shifted down. " \
  "Do NOT use append_file when you know the insertion point — use this instead.",
  {
    "type" => "object",
    "properties" => {
      "path"    => { "type" => "string" },
      "line"    => { "type" => "integer", "description" => "1-indexed line number after which to insert" },
      "content" => { "type" => "string",  "description" => "Text to insert. A trailing newline is added automatically if absent." }
    },
    "required" => ["path", "line", "content"]
  }
) do |args|
  path     = args["path"]    or raise "missing 'path' argument"
  line_arg = args["line"]
  raise "missing or invalid 'line' argument (expected integer)" if line_arg.nil?
  content  = args["content"] or raise "missing 'content' argument"

  line_num = line_arg.to_i
  raise "line must be >= 0 (got #{line_num})" if line_num < 0
  content  = content.end_with?("\n") ? content : content + "\n"

  text  = Shio.read_file(path)
  # split with -1 preserves trailing empty fields; otherwise Ruby drops blank
  # lines before EOF and under-reports the line count.
  lines = text.split("\n", -1)
  # A final "" element represents the terminating newline, not a real line.
  lines.pop if text.end_with?("\n") && lines.last == ""
  total = lines.length

  raise "line #{line_num} is out of range (file has #{total} lines)" if line_num > total

  lines.insert(line_num, content.chomp)
  result = lines.join("\n")
  result += "\n" if text.end_with?("\n")

  Shio.write_file(path, result)

  # Report the new total line count so the caller has a correct anchor
  # for any follow-up insert_after_line call. Also use bytesize (not length,
  # which returns characters in Ruby) so the byte count is accurate for
  # non-ASCII content.
  new_lines = result.split("\n", -1)
  new_lines.pop if result.end_with?("\n") && new_lines.last == ""
  "Inserted #{content.bytesize} bytes after line #{line_num} in #{path} " \
    "(file now has #{new_lines.length} lines)"
end
