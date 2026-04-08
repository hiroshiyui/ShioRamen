# SPDX-License-Identifier: GPL-3.0-or-later
define_tool(
  "patch_file",
  "Apply a targeted find-and-replace edit to a file. " \
  "Finds the exact string old_str (must appear exactly once) and " \
  "replaces it with new_str. Use this for ALL in-place edits: " \
  "modifying, refactoring, or rewriting existing lines. " \
  "Safer than write_file for focused edits because the rest of the file is untouched. " \
  "old_str must be the exact text from the file as returned by read_file or " \
  "read_file_range (which outputs raw lines with no line-number prefixes). " \
  "new_str is written verbatim.",
  {
    "type" => "object",
    "properties" => {
      "path"    => { "type" => "string", "description" => "File to edit" },
      "old_str" => { "type" => "string", "description" => "Exact text to find (must appear exactly once)" },
      "new_str" => { "type" => "string", "description" => "Replacement text" }
    },
    "required" => ["path", "old_str", "new_str"]
  }
) do |args|
  path    = args["path"]    or raise "missing 'path' argument"
  old_str = args["old_str"] or raise "missing 'old_str' argument"
  new_str = args["new_str"].to_s   # nil → "", empty string means delete

  content = begin
    Shio.read_file(path)
  rescue => e
    next "Error reading #{path}: #{e.message}"
  end

  # ── Level 1: exact substring match ──────────────────────────────────────
  first = content.index(old_str)
  if first
    second = content.index(old_str, first + old_str.length)
    if second
      count = 0
      pos = 0
      while (idx = content.index(old_str, pos))
        count += 1
        pos = idx + old_str.length
      end
      next "Error: old_str appears #{count} times in #{path} — " \
           "make it more specific so it matches exactly once"
    end
    patched = content[0...first] + new_str + content[(first + old_str.length)..-1]
    begin
      Shio.write_file(path, patched)
    rescue => e
      next "Error writing #{path}: #{e.message}"
    end
    next "Patched #{path}: old_str replaced with new_str in place. " \
         "The new content is already written — do NOT call append_file or write_file."
  end

  # ── Level 2: line-by-line with trim_end tolerance ───────────────────────
  old_lines  = old_str.split("\n")
  if old_lines.empty? || old_lines.all? { |l| l.strip.empty? }
    next "Error: old_str not found in #{path}"
  end
  file_lines = content.split("\n")
  n = old_lines.length

  hits = []
  (0..(file_lines.length - n)).each do |i|
    next unless (0...n).all? { |j| file_lines[i + j].rstrip == old_lines[j].rstrip }
    hits << i
  end

  if hits.length == 1
    start  = hits[0]
    result = file_lines[0...start]
    new_str.split("\n").each { |l| result << l }
    result << "" if new_str[-1] == "\n"
    result.concat(file_lines[(start + n)..-1] || [])
    patched = result.join("\n")
    patched = patched[0..-2] if content[-1] != "\n" && patched[-1] == "\n"
    patched += "\n"          if content[-1] == "\n" && patched[-1] != "\n"
    begin
      Shio.write_file(path, patched)
    rescue => e
      next "Error writing #{path}: #{e.message}"
    end
    next "Patched #{path} (line-by-line fallback): old_str replaced with new_str. " \
         "The new content is already written — do NOT call append_file or write_file."
  elsif hits.length > 1
    next "Error: old_str matches #{hits.length} locations in #{path} " \
         "(line-by-line fallback) — make it more specific so it matches exactly once"
  end

  # ── Level 3: anchor fallback (≥4 lines) ─────────────────────────────────
  if n >= 4
    a0 = old_lines[0].rstrip
    a1 = old_lines[1].rstrip
    z0 = old_lines[n - 2].rstrip
    z1 = old_lines[n - 1].rstrip
    start_ok = !a0.strip.empty? || !a1.strip.empty?
    end_ok   = !z0.strip.empty? || !z1.strip.empty?
    if start_ok && end_ok
      anchor_hits = []
      (0..(file_lines.length - n)).each do |i|
        if file_lines[i].rstrip     == a0 && file_lines[i + 1].rstrip == a1 &&
           file_lines[i + n - 2].rstrip == z0 && file_lines[i + n - 1].rstrip == z1
          anchor_hits << i
        end
      end
      if anchor_hits.length == 1
        start  = anchor_hits[0]
        result = file_lines[0...start]
        new_str.split("\n").each { |l| result << l }
        result << "" if new_str[-1] == "\n"
        result.concat(file_lines[(start + n)..-1] || [])
        patched = result.join("\n")
        patched = patched[0..-2] if content[-1] != "\n" && patched[-1] == "\n"
        patched += "\n"          if content[-1] == "\n" && patched[-1] != "\n"
        begin
          Shio.write_file(path, patched)
        rescue => e
          next "Error writing #{path}: #{e.message}"
        end
        next "Patched #{path} (anchor fallback): block identified by " \
             "first/last lines of old_str replaced with new_str. " \
             "The new content is already written — do NOT call append_file or write_file."
      end
    end
  end

  "Error: old_str not found in #{path}"
end
