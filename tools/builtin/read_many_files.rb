# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "read_many_files",
  "Read the contents of multiple files in a single call. " \
  "Returns each file's content separated by a header showing its path. " \
  "More efficient than calling read_file repeatedly when you need " \
  "several related files at once.",
  {
    "type" => "object",
    "properties" => {
      "paths" => {
        "type"        => "array",
        "items"       => { "type" => "string" },
        "description" => "List of file paths to read"
      }
    },
    "required" => ["paths"]
  }
) do |args|
  paths = args["paths"]
  raise "missing 'paths' array" if paths.nil?
  raise "'paths' array is empty" if paths.empty?

  out = ""
  paths.each do |path|
    next unless path.is_a?(String)
    out += "=== #{path} ===\n"
    begin
      out += Shio.read_file(path)
    rescue => e
      out += "[Error: #{e.message}]"
    end
    out += "\n\n"
  end
  out.rstrip
end
