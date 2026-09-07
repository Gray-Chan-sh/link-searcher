#!/bin/bash
# G-61: 文本预览→内容正确 [P0]
# 验证: 搜索 → 点击结果 → 预览面板出现文字
set -u
TEST_VISUAL_DIR="${TEST_VISUAL_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"
source "$TEST_VISUAL_DIR/lib.sh"

case_begin "G-61" "文本预览→内容正确"

goto "/"
snap "搜索页"

type "法院" "input[data-search-input=true]"
snap "输入关键词"

clickcss "button[aria-label=搜索]"
snap "点击搜索按钮"

wait_js "document.body.innerText.includes('得分')" 10 "搜索结果加载"

clicktext "得分"
snap "点击第一条结果"

expect_count "
(function(){
  var all = Array.from(document.querySelectorAll('div'));
  var pv = all.filter(d => {
    var r = d.getBoundingClientRect();
    return r.left > 800 && r.width > 50 && d.textContent.length > 30;
  });
  if(!pv.length) return 0;
  return pv[0].textContent.length || 0;
})()
" "预览面板有文字内容"

case_end