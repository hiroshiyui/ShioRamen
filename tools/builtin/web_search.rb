# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "web_search",
  "Search the web using DuckDuckGo and return a list of results " \
  "with titles, URLs, and snippets. Use this when you need current " \
  "information, documentation, or examples that may not be in your " \
  "training data.",
  {
    "type" => "object",
    "properties" => {
      "query"       => { "type" => "string",  "description" => "Search query" },
      "max_results" => { "type" => "integer", "description" => "Maximum number of results to return (default: 5, max: 20)" }
    },
    "required" => ["query"]
  }
) do |args|
  query       = args["query"] or raise "missing 'query' argument"
  max_results = args["max_results"] ? [[args["max_results"].to_i, 1].max, 20].min : 5
  Shio.web_search(query, max_results)
end
