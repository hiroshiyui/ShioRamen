# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "lsp",
  "Query a Language Server Protocol (LSP) server for semantic " \
  "information about source code: type signatures, documentation (hover), " \
  "jump-to-definition, find-all-references, and diagnostics (errors/warnings). " \
  "The server is started and cached automatically; no setup required.",
  {
    "type" => "object",
    "properties" => {
      "operation" => {
        "type" => "string",
        "enum" => ["hover", "definition", "references", "diagnostics"],
        "description" => "What to query: " \
                         "hover = type/doc at position; " \
                         "definition = where a symbol is declared; " \
                         "references = all usages of a symbol; " \
                         "diagnostics = errors and warnings in the file"
      },
      "file"   => { "type" => "string",  "description" => "Path to the source file" },
      "line"   => { "type" => "integer", "description" => "1-indexed line number (required for hover, definition, references)" },
      "column" => { "type" => "integer", "description" => "1-indexed column number (default: 1)" }
    },
    "required" => ["operation", "file"]
  }
) do |args|
  operation = args["operation"] || "hover"
  file      = args["file"] or raise "missing 'file' argument"
  line      = args["line"]   ? args["line"].to_i   : 1
  column    = args["column"] ? args["column"].to_i : 1
  Shio.lsp_query(operation, file, line, column)
end
