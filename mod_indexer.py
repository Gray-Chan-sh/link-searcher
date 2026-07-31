#!/usr/bin/env python3
import os
import re

indexer_path = "/Volumes/Data/Project/Link-Searcher/src-tauri/src/indexer.rs"

with open(indexer_path, 'r', encoding='utf-8') as f:
    content = f.read()

# Find Phase 2 section
phase2_start = content.find('let mut guard = self.lock_writer()?')
for_loop_pos = content.find('for extraction in extracted', phase2_start)
match_ok_pos = content.find('Ok(data) =>', for_loop_pos)
add_doc_pos = content.find('Indexer::add_document', match_ok_pos)

# Determine indentation
before_add = content[:add_doc_pos]
lines = before_add.split('\n')
last_line = lines[-1].rstrip()
indent = len(last_line) - len(last_line.lstrip())

# Insert code before add_document
insert_lines = []
insert_lines.append(' ' * indent + 'let dir_config = dir_config::get_dir(&conn, &data.dir_id)?;')
insert_lines.append(' ' * indent + 'let dir_root = &dir_config.path;')
insert_lines.append(' ' * indent + 'let rel_path = to_relative(dir_root, &data.file_path_str)?;')
insert_code = '\n'.join(insert_lines) + '\n'

# Replace file_path_str with rel_path in the add_document call
# Use regex to find the specific occurrence near add_doc_pos
tail = content[add_doc_pos:]
# Replace &data.file_path_str with &rel_path in the function arguments
new_tail = re.sub(r'\&data\.file_path_str', '&rel_path', tail, count=1)

content = content[:add_doc_pos] + insert_code + new_tail

with open(indexer_path, 'w', encoding='utf-8') as f:
    f.write(content)

print("Modified batch_index Phase 2")
