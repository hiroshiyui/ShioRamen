# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "exit_plan_mode",
  "Exit plan mode and restore access to all tools " \
  "(write_file, patch_file, run_shell, etc.). " \
  "Call this when you have finished exploring and are ready to make changes.",
  {
    "type" => "object",
    "properties" => {
      "plan" => { "type" => "string", "description" => "Optional: brief summary of the plan before exiting" }
    }
  }
) do |_args|
  # The TUI agent loop intercepts this tool call before it reaches here.
  # This stub exists so the tool is registered and appears in the schema.
  "Plan mode control is handled by the agent loop."
end
