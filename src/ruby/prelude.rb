# src/ruby/prelude.rb
# Tool DSL — evaluated once at VM startup.

class Tool
  attr_reader :name, :description, :parameters, :handler

  def initialize(name, description, parameters, &handler)
    @name        = name
    @description = description
    @parameters  = parameters
    @handler     = handler
  end

  def call(args)
    @handler.call(args)
  end
end

$shio_tools = {}

def define_tool(name, description, parameters, &block)
  $shio_tools[name] = Tool.new(name, description, parameters, &block)
end

# Called by ShioVm::tool_schemas() to export ToolDef data as a
# flat array of [name, description, parameters_json_string] triples.
# Uses only stdlib (no JSON gem) — parameters hash is serialized by
# a hand-rolled mini serializer defined below.
def shio_tool_schemas
  $shio_tools.map do |name, tool|
    [name, tool.description, shio_hash_to_json(tool.parameters)]
  end
end

# Called by ShioVm::tool_schemas() to export all registered tool definitions
# as one JSON object per line (newline-joined).  Each line is a complete JSON
# object: {"name":..., "description":..., "parameters":...}.
# Returns an empty string when no tools are registered.
def shio_tool_schemas_json
  $shio_tools.map do |name, tool|
    "{\"name\":#{shio_hash_to_json(name)}," \
    "\"description\":#{shio_hash_to_json(tool.description)}," \
    "\"parameters\":#{shio_hash_to_json(tool.parameters)}}"
  end.join("\n")
end

# Minimal JSON serializer — handles String, Integer, Float, TrueClass,
# FalseClass, NilClass, Array, Hash only (sufficient for JSON Schema objects).
def shio_hash_to_json(val)
  case val
  when Hash
    pairs = val.map { |k, v| "#{shio_hash_to_json(k.to_s)}:#{shio_hash_to_json(v)}" }
    "{#{pairs.join(",")}}"
  when Array
    "[#{val.map { |v| shio_hash_to_json(v) }.join(",")}]"
  when String
    "\"#{val.gsub("\\", "\\\\\\\\").gsub("\"", "\\\"")}\""
  when Integer, Float
    val.to_s
  when TrueClass then "true"
  when FalseClass then "false"
  when NilClass then "null"
  else
    "\"#{val.to_s}\""
  end
end
