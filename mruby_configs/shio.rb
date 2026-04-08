# mruby_configs/shio.rb
MRuby::Build.new do |conf|
  conf.toolchain
  conf.gembox File.join(File.dirname(__FILE__), "mcp_safe")
  conf.enable_test
end
