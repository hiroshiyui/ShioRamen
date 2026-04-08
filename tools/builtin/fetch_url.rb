# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "fetch_url",
  "Fetch the text content of an HTTP or HTTPS URL. " \
  "HTML pages are stripped to readable text. " \
  "Use this whenever the user shares a URL or asks about a web page.",
  {
    "type" => "object",
    "properties" => {
      "url"       => { "type" => "string",  "description" => "The http:// or https:// URL to fetch" },
      "max_chars" => { "type" => "integer", "description" => "Maximum characters of extracted text to return (default: 8000)" }
    },
    "required" => ["url"]
  }
) do |args|
  url       = args["url"] or raise "missing 'url' argument"
  max_chars = args["max_chars"] ? args["max_chars"].to_i : 8000
  Shio.fetch_url(url, max_chars)
end
