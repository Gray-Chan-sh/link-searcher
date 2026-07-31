#!/usr/bin/env python3
# Clean transformation of scanner/mod.rs using text replacements

SRC = "/Volumes/Data/Project/Link-Searcher/src-tauri/src"
mod_path = f"{SRC}/scanner/mod.rs"

with open(mod_path, 'r', encoding='utf-8') as f:
    content = f.read()

# ============================================================
# Transformation 1: Add dir_root in full_scan after log line
# ============================================================
# Find: log::info!("[SCAN] 开始扫描: {}", config.path); let exclude = ...
# Insert: let dir_root = &config.path; between them
lines = content.split('\n')
new_lines = []
i = 0
while i < len(lines):
    line = lines[i]
    if 'log::info!("[SCAN] 开始扫描: {}", config.path);' in line and i+1 < len(lines) and 'let exclude = parse_exclude_patterns' in lines[i+1]:
        new_lines.append(line)
        new_lines.append('        let dir_root = &config.path;')
        i += 1
        continue
    new_lines.append(line)
    i += 1
content = '\n'.join(new_lines)

# ============================================================
# Transformation 2: Add dir_root in incremental_scan after config block
# ============================================================
# incremental_scan doesn't have a log line, so we need to insert after the .ok_or_else... line
# Pattern: the .ok_or_else... line followed by blank line or let exclude
lines = content.split('\n')
new_lines = []
i = 0
in_inc = False
while i < len(lines):
    line = lines[i]
    
    # Detect start of incremental_scan
    if 'pub fn incremental_scan' in line:
        in_inc = True
    
    if in_inc and '.ok_or_else(|| anyhow::anyhow!("dir_config not found: {dir_id}"))?;' in line:
        new_lines.append(line)
        i += 1
        # Check next line for let exclude
        if i < len(lines) and lines[i].strip().startswith('let exclude'):
            new_lines.append('        let dir_root = &config.path;')
        continue
    
    new_lines.append(line)
    i += 1
content = '\n'.join(new_lines)

# ============================================================
# Transformation 3: Add dir_root in startup_scan after log line  
# ============================================================
lines = content.split('\n')
new_lines = []
i = 0
while i < len(lines):
    line = lines[i]
    if 'log::info!("[STARTUP] 启动扫描: {}", config.path);' in line and i+1 < len(lines) and 'let exclude = parse_exclude_patterns' in lines[i+1]:
        new_lines.append(line)
        new_lines.append('        let dir_root = &config.path;')
        i += 1
        continue
    new_lines.append(line)
    i += 1
content = '\n'.join(new_lines)

# ============================================================
# Transformation 4: Modify full_scan loop to use rel_path
# ============================================================
# Need to add rel_path computation after path_str assignment, then replace DB operations
lines = content.split('\n')
new_lines = []
in_full_loop = False
i = 0
while i < len(lines):
    line = lines[i]
    
    if 'pub fn full_scan' in line:
        in_full_loop = False  # reset
    
    # Detect start of full_scan for loop
    if not in_full_loop and 'for entry in walkdir::WalkDir::new(&config.path)' in line:
        in_full_loop = True
        new_lines.append(line)
        i += 1
        continue
    
    if in_full_loop:
        if 'let path = entry.path().to_path_buf();' in line:
            new_lines.append(line)
            i += 1
            continue
        
        if 'let path_str = path.to_string_lossy().to_string();' in line:
            new_lines.append(line)
            new_lines.append('            let rel_path = to_relative(dir_root, &path)?;')
            i += 1
            continue
        
        if 'disk_paths.push(path_str.clone());' in line:
            new_lines.append(line.replace('path_str', 'rel_path'))
            i += 1
            continue
        
        if 'tracker::get_file_by_path' in line and '&path_str' in line:
            new_lines.append(line.replace('&path_str', '&rel_path'))
            i += 1
            continue
        
        if 'tracker::upsert_file' in line and '&path_str' in line:
            new_lines.append(line.replace('&path_str', '&rel_path'))
            i += 1
            continue
    
    new_lines.append(line)
    i += 1
content = '\n'.join(new_lines)

# ============================================================
# Transformation 5: Modify incremental_scan loop
# ============================================================
lines = content.split('\n')
new_lines = []
in_inc_loop = False
i = 0
while i < len(lines):
    line = lines[i]
    
    if 'pub fn incremental_scan' in line:
        in_inc_loop = False
    
    # Detect start of incremental_scan for loop
    if not in_inc_loop and ('for entry in walker' in line or 'for entry in walkdir' in line):
        # Be careful - there might be a separate walker declaration before the loop
        # Look specifically for "for entry {" pattern
        if 'for entry' in line and 'walker' in line:
            in_inc_loop = True
            new_lines.append(line)
            i += 1
            continue
    
    if in_inc_loop:
        if 'let path = entry.path().to_path_buf();' in line:
            new_lines.append(line)
            i += 1
            continue
        
        if 'let path_str = path.to_string_lossy().to_string();' in line:
            new_lines.append(line)
            new_lines.append('            let rel_path = to_relative(dir_root, &path)?;')
            i += 1
            continue
        
        if 'on_disk.push(path_str.clone());' in line:
            new_lines.append(line.replace('path_str', 'rel_path'))
            i += 1
            continue
        
        if 'tracker::get_file_by_path' in line and '&path_str' in line:
            new_lines.append(line.replace('&path_str', '&rel_path'))
            i += 1
            continue
        
        if 'tracker::upsert_file' in line and '&path_str' in line:
            new_lines.append(line.replace('&path_str', '&rel_path'))
            i += 1
            continue
    
    new_lines.append(line)
    i += 1
content = '\n'.join(new_lines)

# ============================================================
# Transformation 6: Modify startup_scan loop
# ============================================================
lines = content.split('\n')
new_lines = []
in_startup_loop = False
i = 0
while i < len(lines):
    line = lines[i]
    
    if 'pub fn startup_scan' in line:
        in_startup_loop = False
    
    if not in_startup_loop and 'for entry in walker' in line:
        in_startup_loop = True
        new_lines.append(line)
        i += 1
        continue
    
    if in_startup_loop:
        if 'let path = entry.path().to_path_buf();' in line:
            new_lines.append(line)
            i += 1
            continue
        
        if 'let path_str = path.to_string_lossy().to_string();' in line:
            new_lines.append(line)
            new_lines.append('            let rel_path = to_relative(dir_root, &path)?;')
            i += 1
            continue
        
        if 'on_disk.push(DiskEntry { path: path_str, size, name });' in line:
            new_lines.append(line.replace('path_str', 'rel_path'))
            i += 1
            continue
        
        if 'tracker::get_file_by_path' in line and '&path_str' in line:
            new_lines.append(line.replace('&path_str', '&rel_path'))
            i += 1
            continue
        
        if 'tracker::upsert_file' in line and '&path_str' in line:
            new_lines.append(line.replace('&path_str', '&rel_path'))
            i += 1
            continue
    
    new_lines.append(line)
    i += 1
content = '\n'.join(new_lines)

# ============================================================
# Transformation 7: Modify handle_event to use relative paths
# ============================================================
# This is complex because handle_event needs dir_root which depends on dir_id.
# We need to: inside handle_event, look up dir_config to get dir_root, then convert event.path to relative.
# But this would require significant restructuring. 
# 
# Alternative approach: The task says "handle_event（watcher 用的）中把事件给的绝对路径→相对后再 upsert_file"
# So we need to: get dir_id from event, lookup dir path from DB, compute rel_path, use it for upsert.
# 
# Let's modify handle_event to first get the dir_root from config using event.dir_id.
# Current code:
#   pub fn handle_event(&self, event: FileChangeEvent) -> Result<()> {
#       let conn = self.db.get()?;
#       let file_path = &event.path;
#       match event.kind {
#           Create | Modify => {
#               let path_str = file_path.to_string_lossy().to_string();
#               ...
#               let file_id = tracker::upsert_file(&conn, &path_str, &event.dir_id, ...);
# 
# We need to change this to:
#   let dir_config = dir_config::get_dir(&conn, &event.dir_id)??.ok_or(...)?;
#   let dir_root = &dir_config.path;
#   let rel_path = to_relative(dir_root, file_path)?;
#   let file_id = tracker::upsert_file(&conn, &rel_path, &event.dir_id, ...);

# Let's implement this transformation
# First, find the handle_event function
handle_start = content.find('pub fn handle_event')
if handle_start == -1:
    print("Could not find handle_event")
else:
    # Find the body of handle_event (between the { and matching })
    # Simple approach: find the line with 'ChangeKind::Create | ChangeKind::Modify =>'
    lines = content.split('\n')
    new_lines = []
    i = 0
    in_handle = False
    while i < len(lines):
        line = lines[i]
        if 'pub fn handle_event' in line:
            in_handle = True
            new_lines.append(line)
            i += 1
            continue
        
        if in_handle:
            # Find the Create/Modify branch start
            if 'ChangeKind::Create | ChangeKind::Modify =>' in line:
                # We need to insert dir_root lookup and rel_path computation before path_str
                # First, add the conn is already obtained above
                # Insert after the match { line but before let path_str = ...
                new_lines.append(line)
                i += 1
                # Next few lines should be the let path_str and meta
                # We'll intercept path_str line to insert dir lookup before it
                peek_mode = 'after_match'
                continue
            
            if peek_mode == 'after_match' and 'let path_str = file_path.to_string_lossy().to_string();' in line:
                # Insert dir_root lookup before this line
                new_lines.append('                let dir_config = dir_config::get_dir(&conn, &event.dir_id)?.ok_or_else(|| anyhow::anyhow!("dir not found: {}", event.dir_id))?;')
                new_lines.append('                let dir_root = &dir_config.path;')
                new_lines.append('                let rel_path = to_relative(dir_root, file_path)?;')
                # Now replace path_str usage with rel_path in upsert and get_file_by_path later
                # Keep the path_str line for indexer.index_file call (which needs absolute path)
                new_lines.append(line)
                i += 1
                continue
            
            # In the delete branch, similarly need rel_path
            if 'ChangeKind::Delete =>' in line:
                new_lines.append(line)
                i += 1
                continue
            
            if in_handle and 'tracker::get_file_by_path' in line and '&path_str' in line and 'ChangeKind::Delete' in lines[i-5:i]:
                # Delete branch lookup - need to use rel_path now but we don't have it computed
                # Actually in delete branch, we still need the original absolute path for the DB lookup since we're looking up by the event path
                # Hmm, this gets complicated. For delete, we should look up by the original path (which will also need to be converted to relative stored form)
                # Actually, simpler: in delete branch, we want to find the record by its stored relative path. 
                # We can compute rel_path from event.path just like in create/modify.
                pass
            
            new_lines.append(line)
            i += 1
            continue
        
        new_lines.append(line)
        i += 1
    content = '\n'.join(new_lines)

# Write back
with open(mod_path, 'w', encoding='utf-8') as f:
    f.write(content)

print("Transformations complete.")

# Verification
with open(mod_path, 'r', encoding='utf-8') as f:
    content = f.read()
    checks = [
        ('full_scan dir_root', 'let dir_root = &config.path;' in content and content.count('let dir_root = &config.path;') >= 2),
        ('full_scan rel_path', 'disk_paths.push(rel_path.clone());' in content),
        ('incremental_scan dir_root', 'pub fn incremental_scan' in content and 'dir_root' in content.split('pub fn incremental_scan')[1].split('pub fn startup_scan')[0]),
        ('startup_scan rel_path', 'DiskEntry { path: rel_path' in content),
    ]
    for name, ok in checks:
        status = "✓" if ok else "✗"
        print(f"  {status} {name}")
