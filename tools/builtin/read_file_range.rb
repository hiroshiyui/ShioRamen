# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "read_file_range",
  "Read a specific range of lines from a file. " \
  "Prefer this over read_file for large files when you already " \
  "know which section you need (e.g. from grep_files results).",
  {
    "type" => "object",
    "properties" => {
      "path"       => { "type" => "string",  "description" => "Path to the file" },
      "start_line" => { "type" => "integer", "description" => "First line to return, 1-indexed (inclusive)" },
      "end_line"   => {
        "type" => "integer",
        "description" => "Last line to return, 1-indexed (inclusive). Omit to read to end of file."
      }
    },
    "required" => ["path", "start_line"]
  }
) do |args|
  path = args["path"] or raise "missing 'path' argument"
  start_line = args["start_line"].to_i
  raise "'start_line' must be a positive integer" if start_line < 1

  content = Shio.read_file(path)
  # split with -1 preserves trailing empty fields; otherwise Ruby drops blank
  # lines before EOF and under-reports the line count.
  lines   = content.split("\n", -1)
  # A final "" element represents the terminating newline, not a real line.
  lines.pop if content.end_with?("\n") && lines.last == ""
  total   = lines.length

  end_line = args["end_line"] ? [args["end_line"].to_i, total].min : total
  raise "'end_line' must be a positive integer" if args["end_line"] && end_line < 1

  if start_line > end_line
    raise "start_line (#{start_line}) is after end_line (#{end_line})"
  end

  body = lines[(start_line - 1)..(end_line - 1)] || []

  if body.empty?
    "(no lines in range #{start_line}\u2013#{end_line}; file has #{total} lines)"
  else
    "Lines #{start_line}\u2013#{end_line} of #{path} (file has #{total} lines):\n#{body.join("\n")}"
  end
end
