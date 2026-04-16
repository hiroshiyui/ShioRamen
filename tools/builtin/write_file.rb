# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "write_file",
  "Write content to a file, creating it or overwriting it. " \
  "BOTH 'path' and 'content' are REQUIRED — never omit 'path'. " \
  "If the user names a file (e.g. \"save to story.txt\"), pass that as 'path'. " \
  "If unsure where to write, call get_working_directory first; " \
  "do not call write_file with only 'content'.",
  {
    "type" => "object",
    "properties" => {
      "path" => {
        "type" => "string",
        "description" => "REQUIRED. Destination file path (absolute, or relative to the working directory). Must always be provided."
      },
      "content" => {
        "type" => "string",
        "description" => "REQUIRED. Full content to write. Overwrites the file if it exists."
      }
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
