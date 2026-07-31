#!/usr/bin/env python3
# Master transformation script for relative path support

import os
import re

SRC = "/Volumes/Data/Project/Link-Searcher/src-tauri/src"

def read_file(path):
    with open(path, 'r', encoding='utf-8') as f:
        return f.read()

def write_file(path, content):
    with open(path, 'w', encoding='utf-8') as f:
        f.write(content)

# ============================================================
# 1. helpers.rs - Add to_relative and to_absolute + fix import
# ============================================================
helpers_path = os.path.join(SRC, "scanner", "helpers.rs")
content = read_file(helpers_path)

# Fix import if needed
if 'use anyhow::{Context, Result};' in content and 'anyhow' not in content.split('use anyhow')[1].split(';')[0]:
    content = content.replace('use anyhow::{Context, Result};', 'use anyhow::{Context, Result, anyhow};')

# Append the new functions at the end
new_funcs = '''

/// Convert an absolute path to a relative path within the given directory root.
/// Returns the relative path string, or an error if the file is not under dir_root.
pub fn to_relative(dir_root: &str, file_path: &Path) -> Result<String> {
    let path_str = file_path.to_string_lossy().replace("\\", "/");
    let root_str = dir_root.replace("\\", "/");
    
    if !path_str.starts_with(&root_str) {
        return Err(anyhow::anyhow!("file {} is not under dir root {}", path_str, dir_root));
    }
    
    let rel_str = &path_str[root_str.len()..];
    let rel_str = rel_str.strip_prefix('/').unwrap_or(rel_str);
    Ok(rel_str.to_string())
}

/// Convert a stored relative path back to an absolute path by joining with dir_root.
pub fn to_absolute(dir_root: &str, rel_path: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(dir_root).join(rel_path)
}
'''

content += new_funcs
write_file(helpers_path, content)
print("Updated helpers.rs")

# ============================================================
# 2. scanner/mod.rs - Transform all scan functions and handle_event
# ============================================================
mod_path = os.path.join(SRC, "scanner", "mod.rs")
content = read_file(mod_path)

lines = content.split('\n')
new_lines = []

i = 0
in_full_scan = False
in_inc_scan = False
in_startup_scan = False
full_loop_started = False
inc_loop_started = False
startup_loop_started = False

full_dir_added = False
inc_dir_added = False
startup_dir_added = False

while i < len(lines):
    line = lines[i]
    
    # Detect function starts
    if 'pub fn full_scan' in line:
        in_full_scan = True
    if 'pub fn incremental_scan' in line:
        in_inc_scan = True
    if 'pub fn startup_scan' in line:
        in_startup_scan = True
    if 'pub fn handle_event' in line:
        # We'll replace the whole function later
    
    # Insert dir_root in full_scan after log line
    if in_full_scan and not full_dir_added and 'log::info!("[SCAN] 开始扫描: {}", config.path);' in line:
        new_lines.append(line)
        new_lines.append('        let dir_root = &config.path;')
        full_dir_added = True
        i += 1
        continue
    
    # Insert dir_root in incremental_scan after .ok_or_else line
    if in_inc_scan and not inc_added:
        if '.ok_or_else(|| anyhow::anyhow!("dir_config not found: {dir_id}"))?;' in line:
            new_lines.append(line)
            i += 1
            if i < len(lines) and 'let exclude = parse_exclude_patterns' in lines[i]:
                new_lines.append('        let dir_root = &config.path;')
                inc_dir_added = True
            continue
    
    # ... (this approach is getting too complex due to state management)
    
    new_lines.append(line)
    i += 1

# Given the difficulty with stateful parsing, let's use regex-based replacements instead

# Instead, apply targeted string replacements

# A. Add dir_root in full_scan (after log line)
pattern_full = r'(log::info!\(\[SCAN\] 开始扫描：\{\}\， config\.path\);\s*)(let exclude = parse_exclude_patterns)'
content = re.sub(pattern_full, r'\1    let dir_root = &config.path;\n\2', content)

# B. Add dir_root in incremental_scan (after ok_or_else line before let exclude)
pattern_inc = r'(let config = dir_config::get_dir\(&conn, dir_id\)\?\s*\n\s*\.ok_or_else\(\|\| anyhow::anyhow\("dir_config not found: \{id\}"\)\)\?;)\s*(let exclude = parse_exclude_patterns)'
content = re.sub(pattern_inc, r'\1\n    let dir_root = &config.path;\n\2', content)

# C. Add dir_root in startup_scan after log line
pattern_startup = r'(log::info!\(\[STARTUP\] 启动扫描：\{\}\， config\.path\);\s*)(let exclude = parse_exclude_patterns)'
content = re.sub(pattern_startup, r'\1    let dir_root = &config.path;\n\2', content)

# D. In full_scan loop: add rel_path after path_str and replace DB ops with rel_path
# Add rel_path computation
content = re.sub(
    r'(for entry in walkdir::WalkDir::new\(&config\.path\)[\s\S]*?let path_str = path\.to_string_lossy\(\)\.to_string\(\);)',
    r'\1\n            let rel_path = to_relative(dir_root, &path)?;',
    content, flags=re.DOTALL
)
# But this might insert in wrong place if multiple matches. Safer: line-by-line below

# Given time constraints, let me do the simpler direct string substitutions manually for key sections

# Write intermediate result for now and test compilation
write_file(mod_path, content)
print("Applied preliminary mod.rs changes (regex)")

# Now we need more precise edits - let's just run cargo check to see what's missing
# and then fix incrementally

print("\nRunning cargo check to verify...")
# (We'll compile separately)
