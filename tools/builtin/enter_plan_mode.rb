# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "enter_plan_mode",
  "Switch to plan mode, restricting tool access to read-only operations " \
  "(read_file, search_files, grep_files, lsp, fetch_url, web_search, etc.). " \
  "Use this before making changes: explore the codebase, understand the structure, " \
  "draft a plan, then call exit_plan_mode to restore full tool access.",
  {
    "type" => "object",
    "properties" => {
      "reason" => { "type" => "string", "description" => "Optional: why you are entering plan mode" }
    }
  }
) do |_args|
  # The TUI agent loop intercepts this tool call before it reaches here.
  # This stub exists so the tool is registered and appears in the schema.
  "Plan mode control is handled by the agent loop."
end
