# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "read_file",
  "Read a file. Large files are returned in chunks; pass `cursor` (1-indexed " \
  "line number) to fetch the next chunk. The result trails with a hint line " \
  "telling you the next cursor value, or that EOF was reached.",
  {
    "type" => "object",
    "properties" => {
      "path"        => { "type" => "string",  "description" => "Path to the file" },
      "cursor"      => { "type" => "integer", "description" => "1-indexed line to start at (default 1)" },
      "chunk_lines" => { "type" => "integer", "description" => "Lines per chunk (default 400)" }
    },
    "required" => ["path"]
  }
) do |args|
  path = args["path"] or raise ArgumentError, "missing 'path'"
  cursor      = (args["cursor"]      || 1).to_i
  chunk_lines = (args["chunk_lines"] || 400).to_i
  raise "'cursor' must be a positive integer"      if cursor      < 1
  raise "'chunk_lines' must be a positive integer" if chunk_lines < 1

  content = Shio.read_file(path)
  # split with -1 preserves trailing empty fields; otherwise Ruby drops blank
  # lines before EOF and under-reports the line count.
  lines = content.split("\n", -1)
  lines.pop if content.end_with?("\n") && lines.last == ""
  total = lines.length

  if cursor > total
    next "(cursor #{cursor} is past EOF; file has #{total} lines)"
  end

  last = [cursor + chunk_lines - 1, total].min
  body = lines[(cursor - 1)..(last - 1)] || []
  hint =
    if last < total
      "\n\n[lines #{cursor}–#{last} of #{total}; call read_file again with cursor=#{last + 1} to continue]"
    else
      "\n\n[lines #{cursor}–#{last} of #{total}; end of file]"
    end
  "#{body.join("\n")}#{hint}"
end
