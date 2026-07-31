#!/usr/bin/env python3
"""Convert file paths from absolute to relative in Link-Searcher codebase."""

import os
import re

SRC = "/Volumes/Data/Project/Link-Searcher/src-tauri/src"

def read_file(path):
    with open(path, 'r', encoding='utf-8') as f:
        return f.read()

def write_file(path, content):
    with open(path, 'w', encoding='utf-8') as f:
        f.write(content)

print("Starting transformation...")

# ============================================================
# 2. scanner/mod.rs - major transformation
# ============================================================

mod_path = os.path.join(SRC, "scanner", "mod.rs")
content = read_file(mod_path)

# ---- Add dir_root variable after config log in full_scan ----
pattern1 = r'(log::info!\(\[SCAN\] 开始扫描：\{\}\， config\.path\);\s*)(let exclude = parse_exclude_patterns\(&config\.exclude_patterns\);)'
new_content = re.sub(pattern1, r'\1    let dir_root = &config.path;\n\2', content, flags=re.DOTALL)

if new_content == content:
    print("Warning: dir_root insertion didn't match, trying alternative...")
    # Try more flexible matching
    lines = content.split('\n')
    for idx, line in enumerate(lines):
        if 'log::info!("[SCAN] 开始扫描: {}", config.path);' in line and idx+1 < len(lines) and 'let exclude = parse_exclude_patterns' in lines[idx+1]:
            lines.insert(idx+1, '        let dir_root = &config.path;')
            new_content = '\n'.join(lines)
            break
content = new_content

# ---- Modify the main loop in full_scan using line-by-line ----
lines = content.split('\n')
new_lines = []
i = 0
in_loop = False

while i < len(lines):
    line = lines[i]
    
    # Detect start of for loop in full_scan
    if not in_loop and 'for entry in walkdir::WalkDir::new(&config.path)' in line:
        in_loop = True
        new_lines.append(line)
        i += 1
        continue
    
    if in_loop:
        # Original: let path = entry.path().to_path_buf();
        # We keep this, then add path_str and rel_path computations
        if 'let path = entry.path().to_path_buf();' in line:
            new_lines.append(line)
            new_lines.append('            let path_str = path.to_string_lossy().to_string();')
            new_lines.append('            let rel_path = to_relative(dir_root, &path)?;')
            i += 1
            continue
        
        # Skip original path_str assignment (already handled above)
        if 'let path_str = path.to_string_lossy().to_string();' in line:
            i += 1
            continue
        
        # Check extension_allowed - need to change to use path (absolute) for this check
        # Actually we should still use absolute path for extension_allowed - keep as is
        if 'if !extension_allowed(&path, &include_exts) { continue; }' in line:
            new_lines.append(line)
            i += 1
            continue
        
        # Replace disk_paths.push with rel_path
        if 'disk_paths.push(path_str.clone());' in line:
            new_line = line.replace('path_str', 'rel_path')
            new_lines.append(new_line)
            i += 1
            continue
        
        # Replace get_file_by_path with relative path
        if 'tracker::get_file_by_path' in line and '&path_str' in line:
            new_line = line.replace('&path_str', '&rel_path')
            new_lines.append(new_line)
            i += 1
            continue
        
        # Replace upsert_file with relative path
        if 'tracker::upsert_file' in line and '&path_str' in line:
            new_line = line.replace('&path_str', '&rel_path')
            new_lines.append(new_line)
            i += 1
            continue
        
        # The progress line should keep showing absolute path for current_file
        # Keep it as-is
    
    new_lines.append(line)
    i += 1

content = '\n'.join(new_lines)
write_file(mod_path, content)
print("Modified scanner/mod.rs")
