# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "run_shell",
  "Run a shell command and return its stdout and stderr.",
  {
    "type" => "object",
    "properties" => {
      "command" => { "type" => "string", "description" => "The shell command to run" }
    },
    "required" => ["command"]
  }
) do |args|
  command = args["command"] or raise "missing 'command' argument"
  Shio.run_shell(command)
end
