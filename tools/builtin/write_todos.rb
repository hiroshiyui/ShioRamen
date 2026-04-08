# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "write_todos",
  "Write a task list to TODO.md, replacing the file's entire contents. " \
  "Useful for tracking multi-step plans or progress on complex tasks.",
  {
    "type" => "object",
    "properties" => {
      "todos" => {
        "type"  => "array",
        "items" => {
          "type"       => "object",
          "properties" => {
            "task"   => { "type" => "string", "description" => "Task description" },
            "status" => { "type" => "string", "description" => "pending, in_progress, or completed" }
          }
        },
        "description" => "List of todo items"
      },
      "file" => {
        "type"        => "string",
        "description" => "Output file path (default: TODO.md)"
      }
    },
    "required" => ["todos"]
  }
) do |args|
  todos = args["todos"]
  raise "missing 'todos' array" if todos.nil? || !todos.is_a?(Array)

  file    = args["file"] || "TODO.md"
  content = "# TODO\n\n"

  todos.each do |todo|
    task     = todo["task"] || "(unnamed task)"
    status   = todo["status"] || "pending"
    checkbox = case status
               when "completed"  then "[x]"
               when "in_progress" then "[-]"
               else                    "[ ]"
               end
    content += "- #{checkbox} #{task}\n"
  end

  Shio.write_file(file, content)
  "Wrote #{todos.length} todo(s) to #{file}"
end
