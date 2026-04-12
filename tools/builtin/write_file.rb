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
  raise "missing 'content' argument" if args["content"].nil?
  content = args["content"].to_s
  Shio.write_file(path, content)

  # Report the line count so the caller has a correct anchor for any
  # follow-up insert_after_line call. Use bytesize (not length, which
  # returns characters in Ruby) so the byte count is accurate for
  # non-ASCII content.
  lines = content.split("\n", -1)
  lines.pop if content.end_with?("\n") && lines.last == ""
  "Wrote #{content.bytesize} bytes (#{lines.length} lines) to #{path}"
end
