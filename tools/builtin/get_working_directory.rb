# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "get_working_directory",
  "Return the current working directory. " \
  "Call this to resolve relative paths or orient yourself " \
  "before constructing file paths.",
  { "type" => "object", "properties" => {} }
) do |_args|
  Shio.current_dir
end
