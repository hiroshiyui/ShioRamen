# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "save_memory",
  "Append a fact or note to SHIO.md for future reference. " \
  "Use this to persist important information across sessions: " \
  "user preferences, project conventions, architectural decisions, " \
  "or anything you want to remember.",
  {
    "type" => "object",
    "properties" => {
      "memory" => {
        "type"        => "string",
        "description" => "The fact or note to remember"
      },
      "file" => {
        "type"        => "string",
        "description" => "Memory file path (default: SHIO.md)"
      }
    },
    "required" => ["memory"]
  }
) do |args|
  memory = args["memory"] or raise "missing 'memory' argument"
  file   = args["file"] || "SHIO.md"

  existing = begin
    Shio.read_file(file)
  rescue
    ""
  end

  next "Already in #{file} (skipped duplicate)" if existing.include?("- #{memory}\n")

  new_content = if existing.empty?
    "# Shio Memory\n\n- #{memory}\n"
  else
    "#{existing.rstrip}\n- #{memory}\n"
  end

  tmp_path = "#{file}.tmp"
  Shio.write_file(tmp_path, new_content)
  Shio.rename(tmp_path, file)
  "Saved to #{file}: #{memory}"
end
