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
  content  = content.end_with?("\n") ? content : content + "\n"

  text  = Shio.read_file(path)
  lines = text.split("\n")
  total = lines.length

  raise "line #{line_num} is out of range (file has #{total} lines)" if line_num > total

  lines.insert(line_num, content.chomp)
  result = lines.join("\n")
  result += "\n" if text.end_with?("\n") || line_num == total

  Shio.write_file(path, result)
  "Inserted #{content.length} bytes after line #{line_num} in #{path}"
end
