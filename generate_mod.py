#!/usr/bin/env python3
"""Generate modified scanner/mod.rs with relative path support."""

import os
import re

SRC = "/Volumes/Data/Project/Link-Searcher/src-tauri/src"
mod_path = os.path.join(SCR, "scanner", "mod.rs")  # Need to fix this

# Actually read from the backup we have
original_path = "/Volumes/Data/Project/Link-Searcher/src-tauri/src/scanner/mod.rs.orig"
if not os.path.exists(original_path):
    # Try without .orig
    original_path = "/Volumes/Data/Project/Link-Searcher/src-tauri/src/scanner/mod.rs"

with open(original_path, 'r', encoding='utf-8') as f:
    lines = f.readlines()

# We'll build new_lines incrementally
new_lines = []

i = 0
while i < len(lines):
    line = lines[i]
    
    # Track which scan function we're in
    in_full_scan = False
    in_incremental_scan = False
    in_startup_scan = False
    
    # Detect function starts
    if 'pub fn full_scan' in line:
        in_full_scan = True
    elif 'pub fn incremental_scan' in line:
        in_incremental_scan = True
    elif 'pub fn startup_scan' in line:
        in_startup_scan = True
    
    # ===== Change 1: Add dir_root assignment after config log in full_scan =====
    if in_full_scan and 'log::info!("[SCAN] 开始扫描: {}", config.path);' in line:
        new_lines.append(line)
        new_lines.append('        let dir_root = &config.path;\n')
        i += 1
        continue
    
    # ===== Change 2: In full_scan loop, add rel_path computation =====
    if in_full_scan and 'let path = entry.path().to_path_buf();' in line:
        new_lines.append(line)
        new_lines.append('            let path_str = path.to_string_lossy().to_string();\n')
        new_lines.append('            let rel_path = to_relative(dir_root, &path)?;\n')
        i += 1
        continue
    
    # Skip the duplicate path_str assignment in full_scan (the one originally at line 133)
    if in_full_scan and 'let path_str = path.to_string_lossy().to_string();' in lines[i:i+1]:
        # Already added above, skip
        i += 1
        continue
    
    # ===== Change 3: Replace disk_paths.push with rel_path in full_scan =====
    if in_full_scan and 'disk_paths.push(path_str.clone());' in line:
        new_line = line.replace('path_str', 'rel_path')
        new_lines.append(new_line)
        i += 1
        continue
    
    # ===== Change 4: Replace get_file_by_path with rel_path lookup in full_scan =====
    if in_full_scan and 'tracker::get_file_by_path' in line and '&path_str' in line:
        new_line = line.replace('&path_str', '&rel_path')
        new_lines.append(new_line)
        i += 1
        continue
    
    # ===== Change 5: Replace upsert_file with rel_path in full_scan =====
    if in_full_scan and 'tracker::upsert_file' in line and '&path_str' in line:
        new_line = line.replace('&path_str', '&rel_path')
        new_lines.append(new_line)
        i += 1
        continue
    
    # ===== Change 6: Similar changes for incremental_scan =====
    if in_incremental_scan and 'let path = entry.path().to_path_buf();' in line:
        new_lines.append(line)
        new_lines.append('            let path_str = path.to_string_lossy().to_string();\n')
        new_lines.append('            let rel_path = to_relative(dir_root, &path)?;\n')
        i += 1
        continue
    
    if in_incremental_scan and 'let path_str = path.to_string_lossy().to_string();' in line:
        # Already added above, skip
        i += 1
        continue
    
    if in_incremental_scan and 'on_disk.push(path_str.clone());' in line:
        new_line = line.replace('path_str', 'rel_path')
        new_lines.append(new_line)
        i += 1
        continue
    
    if in_incremental_scan and 'tracker::get_file_by_path' in line and '&path_str' in line:
        new_line = line.replace('&path_str', '&rel_path')
        new_lines.append(new_line)
        i += 1
        continue
    
    if in_incremental_scan and 'tracker::upsert_file' in line and '&path_str' in line:
        new_line = line.replace('&path_str', '&rel_path')
        new_lines.append(new_line)
        i += 1
        continue
    
    # ===== Change 7: Startup scan needs similar treatment =====
    if in_startup_scan and 'let path = entry.path().to_path_buf();' in line:
        new_lines.append(line)
        new_lines.append('            let path_str = path.to_string_lossy().to_string();\n')
        new_lines.append('            let rel_path = to_relative(dir_root, &path)?;\n')
        i += 1
        continue
    
    if in_startup_scan and 'let path_str = path.to_string_lossy().to_string();' in line:
        i += 1
        continue
    
    if in_startup_scan and 'on_disk.push(DiskEntry { path: path_str, size, name });' in line:
        new_line = line.replace('path_str', 'rel_path')
        new_lines.append(new_line)
        i += 1
        continue
    
    if in_startup_scan and 'tracker::get_file_by_path' in line and '&path_str' in line:
        new_line = line.replace('&path_str', '&rel_path')
        new_lines.append(new_line)
        i += 1
        continue
    
    if in_startup_scan and 'tracker::upsert_file' in line and '&path_str' in line:
        new_line = line.replace('&path_str', '&rel_path')
        new_lines.append(new_line)
        i += 1
        continue
    
    # ===== Change 8: handle_event needs to convert event.path to rel_path =====
    # First, we need to detect handle_event and add dir_root access there.
    # But handle_event doesn't have direct access to dir_root - it needs to be passed somehow.
    # Actually, looking at the code, handle_event receives FileChangeEvent which has dir_id but not dir_root.
    # We need to store dir_root mapping somewhere, or get it from DB inside handle_event.
    # For now, let's just modify handle_event to use relative paths.
    if 'pub fn handle_event' in line and 'ChangeKind::Create | ChangeKind::Modify =>' in lines[i+3:i+6]:
        # This is tricky - let's handle it differently
        pass
    
    new_lines.append(line)
    i += 1

# Write output
output_path = os.path.join(SCR, "scanner", "mod.rs")
with open(output_path, 'w', encoding='utf-8') as f:
    f.writelines(new_lines)

print(f"Wrote modified file to {output_path}")
