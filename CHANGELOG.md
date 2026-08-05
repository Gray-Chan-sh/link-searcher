# Link-Searcher 变更日志

> 2026年7月30日 — 8月5日，共 45+ commit，修复 80+ Bug，完成 35+ 功能改进

---

## 2026-08-05（Dock 图标根治：原生优先 + 批量转换取代 LSUIElement hack）

**根因诊断**（受控实验证伪三种压制方案）：
- `SAL_USE_VCLPLUGIN=svp`：不阻止 soffice 注册前台 app（`lsappinfo list` 仍出现 ASN）
- `LSUIElement=true` + `lsregister -f` 强刷缓存：仍注册前台 app（直接 exec 二进制不读 LSUIElement）
- `DYLD_INSERT_LIBRARIES` 注入 dylib：adhoc 签名不够，dyld 直接剥掉（marker 文件未创建）
→ **结论：直接执行 LibreOffice 二进制时，其启动代码必定把自己注册为前台应用，外部手段全部无效。唯一方案是减少进程启动次数。**

**修复方案**：

- **现代格式原生优先**：`.docx`→`extract_docx`、`.xlsx/.xls`→`extract_xlsx`（calamine 原生支持 `.xls`）、`.pptx`→`extract_pptx`，全部原生解析优先；仅原生解析失败或返回空时才回退 LibreOffice。此前连现代 OOXML 格式都先调 LO→ 导致大量不必要的 soffice 进程启动→Dock 图标泛滥。`.xls` 此前被路由到 LO-only 分支，calamine 的 xls 支持路径处于休眠状态——现已激活（`src-tauri/src/extractor/office/mod.rs`）

- **旧格式批量转换**：新增 `LoBatcher`——请求合并调度器。Rayon `par_iter` 并行提交的 `.doc/.ppt` 提取请求进入全局队列，leader 线程收集聚合成批（最多 32 个/批，300ms 收集窗口），单次 `soffice --convert-to` 进程转换整批。`extract_many_via_libreoffice` 内部处理 stem 碰撞（同名输出文件覆盖→分 sub-round 转换）。Leader-election 模式保证并行 Rayon 线程不饿死且无死锁。**索引器零改动**（`src-tauri/src/extractor/office/mod.rs`）
  - 附带疗效：serialize 了 LibreOffice 调用，根治旧日志里的 `DeploymentException` 并发崩溃
  - 超时按批大小缩放：30s + 15s×N，上限 600s

- **LO 路径缓存**：`lo_binary()` 用 `OnceLock` 缓存进程内第一次 `check_binary` 结果，后续每文件不再 spawn `soffice --version`（每个 `--version` 本身也是一次 Dock 图标）
  - 保留原 `is_libreoffice_available()` 用于设置/面板 UI（不缓存，支持用户换路径后即时检测）

- **移除 LoBackgroundGuard::enter 三处调用**：对 `lib.rs` 启动扫描、`index.rs` 的 `trigger_scan` 和 `rebuild_index` 三处 `LoBackgroundGuard::enter()` 已证实无效且会修改用户已签名 LibreOffice 的 Info.plist（破坏签名 + LaunchServices 缓存不认 → 白改）。保留 `ensure_lo_background_mode`（= recover）在启动时清理残余 LSUIElement（`src-tauri/src/lib.rs`、`src-tauri/src/commands/index.rs`）

- **修复预存测试竞态**：`test_index_file_creates_document` 与 `test_delete_file` 并行时共享 `tmp_file("test.txt")` 导致文件覆盖。改名 `test_create.txt` 避免冲突（`src-tauri/src/indexer.rs`）

- **macOS `open -gj` 彻底消除遗留 Dock 图标**：批量转换中残留的每次 soffice 进程启动仍会产生一次 Dock 闪现。改为通过 `open -gj -b org.libreoffice.script --args ...` 启动——LaunchServices 以 hidden 模式运行（`lsappinfo` 显示 `(hidden)`，等效 LSUIElement），彻底无 Dock 图标。PID 通过 `pgrep` 差集追踪，超时时用 PID 精确 kill。`open -gj` 失败时自动回退直接 exec（`src-tauri/src/extractor/office/mod.rs`）

- **批量转换批大小可配置**：原硬编码 32 文件/批改为用户设置项 `lo_batch_size`（1–100）。全局 `AtomicUsize` 零锁读取，保存设置后即时生效无需重启。新增前端 `NumberField` 控件 + i18n 文案（zh/en）（`src-tauri/src/extractor/office/mod.rs`、`lib.rs`、`commands/settings.rs`、`db/mod.rs`、`Settings.tsx`、`zh.ts`、`en.ts`）

- **Windows OCR 实现**：新增 `windows_ocr.rs` 模块，使用 Windows 10+ 原生 `Windows.Media.Ocr.OcrEngine`，同步 `.get()` 阻塞模式。与 Apple Vision 镜像设计：同签名、同语言映射、同 `_with_regions` 诊断接口。非 Windows 平台保留错误提示桩（`src-tauri/src/extractor/windows_ocr.rs`）
- **依赖**：新增 `windows` 0.61，target-conditional（`cfg(target_os = "windows")`），macOS/Linux 编译零影响（`Cargo.toml`）
- **引擎分发**：`ocr.rs` 两处 `WindowsOcr` 分支从 PaddleOCR 桩替换为 `windows_ocr::recognize_from_path` / `_with_regions`；`mod.rs` 注册模块（`src-tauri/src/extractor/ocr.rs`、`mod.rs`）

- **PDF 视觉水印 OCR 污染**：扫描件 PDF 中，水印文字被 `pdftoppm` 渲染到页面图像上，导致 OCR 回退后水印仍被读出。修复：检测到文字层含水印后，优先使用 `pdfimages` 提取原始图像层（不解码文字层/注解层叠加），再对每页最大图像做 OCR，从根本上避免水印污染。`pdfimages` 不可用时回退原有 `pdftoppm` 路径（`src-tauri/src/extractor/pdf.rs`）

- **Tauri GUI PATH 不含 Homebrew → `pdfimages`/`pdftoppm` 找不到**：Tauri macOS 应用 PATH 不包含 `/opt/homebrew/bin`，导致 `Command::new` 找不到 poppler 二进制，OCR 回退完全跳过。修复：新增 `find_poppler_binary` 按 `/opt/homebrew/bin` → `/usr/local/bin` → `/usr/bin` 回退查找，结果用 `OnceLock` 缓存；`is_*_available` 和所有 OCR 函数均改为使用缓存的绝对路径。附带：`[WATCHER] file Modify` 日志对排除文件（`.DS_Store` 等）降级为 `debug!` 级别（`src-tauri/src/extractor/pdf.rs`、`src-tauri/src/lib.rs`）

---

## 2026-08-03（消除残留硬编码：图片 OCR + OCR 回退接入引擎分发）

- **`ocr_image` 便利函数硬编码 PaddleOCR**：图片文件索引和 indexer 短文本 OCR 回退均通过 `ocr_image` 走 PaddleOCR，忽略引擎设置。修复：`ocr_image` 新增 `engine: Option<OcrEngineType>` 参数并通过 `ocr_image_with_engine` 分发；`mod.rs` 图片分支改用 `ocr_image_with_engine`（不再依赖 `ImageExtractor.extract`）；`indexer.rs` 短文本回退传入 `ocr_engine`（`src-tauri/src/extractor/ocr.rs`、`mod.rs`、`indexer.rs`、`tests/integration.rs`）

---

## 2026-08-03（PDF OCR 接入引擎分发：不再硬编码 PaddleOCR）

- **PDF OCR 始终走 PaddleOCR，忽略用户引擎选择**：`pdf.rs` 的 `ocr_pdf_via_pdftoppm` 硬编码 `paddleocr::recognize_from_path_with_regions`，完全绕过 `ocr.rs` 的引擎分发。即使设置页选了 Apple Vision，PDF OCR 仍跑 PaddleOCR → 用户无法感知 Vision 加速。修复：`ocr_pdf_via_pdftoppm`/`extract_with_lang`/`extract_text` 新增 `engine: Option<OcrEngineType>` 参数；`indexer.rs` 从 `app_settings.ocr_engine` 读取配置传入；`ocr.rs` 新增 `ocr_image_with_regions` 调度函数（PaddleOCR/AppleVision/Tesseract 三路）；`apple_vision.rs` 新增 `recognize_from_path_with_regions` 输出 region 数（`src-tauri/src/extractor/pdf.rs`、`mod.rs`、`ocr.rs`、`apple_vision.rs`、`indexer.rs`、`test_pdf_ocr.rs`）

---

## 2026-08-03（Apple Vision OCR 引擎：macOS 原生 OCR，ANE 推理）

- **Apple Vision OCR 实现**：新增 `apple_vision.rs` 模块，使用 macOS 10.15+ 原生 `VNRecognizeTextRequest`（Accurate 级别），运行在 ANE（Neural Engine）上独立于 CPU。与 tract 单线程 CPU 推理相比，预期单区域 0.05-0.2s（当前 1.5-5s），无需手动引擎池管理。实现参考 thuki（Tauri 2 + Vision OCR）和 Pointra-Pub（缓存请求 + 启动预热）生产级项目，`performRequests_error:` 同步调用模式，`autoreleasepool` 包裹防 ObjC 临时对象泄漏（`src-tauri/src/extractor/apple_vision.rs`）
- **依赖**：新增 `objc2` 0.6、`objc2-foundation` 0.3、`objc2-vision` 0.3（`Cargo.toml`）
- **引擎分发**：`ocr.rs` 的 `AppleVision` 分支从 PaddleOCR 桩替换为 `apple_vision::recognize_from_path`；`mod.rs` 注册模块（`src-tauri/src/extractor/ocr.rs`、`mod.rs`）
- **启动预热**：`lib.rs` 启动时后台线程用 64×64 空白图跑一次 Vision，预加载 CoreML/ANE 模型，消除首次调用 1-3s 延迟（Pointra-Pub 模式）（`src-tauri/src/lib.rs`）
- **语言映射**：`eng→en-US`、`chi_sim→zh-Hans`、`jpn→ja-JP`、`kor→ko-KR`（Fast 模式不支持中日韩，固定 Accurate）

---

## 2026-08-03（引擎池诊断：曝光 set_pool_size 静默失败 + macOS P-core 绑定）

- **`set_pool_size` 从未生效（池始终=4）**：`lib.rs` 启动时读 `ocr_concurrent` 的 if-let 链**三层静默吞错**（`db_pool.get()`/`query_row`/`parse` 任意失败均无日志）。修复：改为 `match` 逐层 `log::warn!`，成功后 `log::info!` 记录池大小（`lib.rs`）
- **E-core 拖慢实锤**：9 页 OCR 实测 5 页落在 E-core（效率核），最慢页 `1.76s/区域 → 5.35s/区域`（3× 差距），导致池=4 实际加速比仅 1.48×（预期 3×+）。修复：`paddleocr.rs` 新增 `pthread_set_qos_class_self_np(USER_INTERACTIVE)`，在每次 OCR 推理前向 macOS 调度器声明需要性能核偏好（`paddleocr.rs`）。池构建时追加 `log::info!` 打印引擎数
- 涉及文件：`src-tauri/src/extractor/paddleocr.rs`、`src-tauri/src/lib.rs`

---

## 2026-08-03（设置保存失败：前端回传整个 settings 对象触发白名单拒绝）

- **修改任何设置都报 "Failed to save setting"**：根因是 `Settings.tsx` 的 `handleFieldChange` 通过 `updateSettings({ ...settings, [key]: value })` 把**整个 settings 对象**发回后端，而该对象来自 `get_settings` 返回的 DB **所有行**，包含非白名单键（`theme`、`onboarding_done`、`last_scan_*`、`schema_version` 等）。后端 `update_settings` 白名单校验遇到第一个非法键即整体拒绝 → 改任何设置都失败。修复：只发送被修改的单键 `updateSettings({ [key]: value })`（`src/pages/Settings.tsx`）。附带确认 `Settings.tsx` 用到的 9 个键全部在后端 `ALLOWED_KEYS` 白名单内；`onboarding_done` 写入失败被 `.catch` 静默吞掉且已有 localStorage 兜底，无功能影响

---

## 2026-08-03（PDF OCR 每页诊断日志 + 池大小暴露）

- **PDF OCR 性能疑点定位**：9 页扫描件 OCR 耗时 319s（平均 35s/页），接近串行耗时（9×35s），与 2 引擎并行预期（~175s）不符。原因待定：可能是引擎被 Rayon 调度到 E-core（效率核）拖慢，或池实际大小非预期。此前日志无池大小与每页耗时，无法区分。修复：`paddleocr.rs` 新增 `active_pool_size()`（惰性池未构建时为 0）和 `recognize_from_path_with_regions()`（返回文本 + 区域数）；`pdf.rs` OCR 循环每页记录耗时/区域数/字符数，起始日志打印池大小（`src-tauri/src/extractor/paddleocr.rs`、`src-tauri/src/extractor/pdf.rs`）

---

## 2026-08-03（PDF OCR 提速：引擎池 + 多页并行 + 渲染 DPI 300→200）

- **PDF OCR 每页 30-60 秒过慢（实测验证）**：用 `pure_onnx_ocr::run_with_metrics_from_path` 对 200 DPI 单页实测：总 54.4s = 检测推理 16.5s（30%）+ 识别推理 34.9s（64%）+ 缩放/后处理 ~3s。识别批张量恒为 `[N,3,48,320]`（`RecPreProcessor` 满宽 320 分配），tract 无 intra-op 并行且 batch 线性展开，故「分块降 3-5 倍」对 tract 不成立。确定性的收益来自两处：
  - **全局单引擎 Mutex 串行化**：`paddleocr.rs` 原为 `OnceLock<SendEngine>`，所有 OCR 调用（多页 PDF 逐页、Rayon batch_index 多文件）在同一个 Mutex 上排队，多核闲置。修复：改为 `EnginePool`（N = min(可用核数, 4)，每个引擎独立 Mutex + round-robin 负载均衡），并发 OCR 调用分散到多核
  - **多页 PDF 逐页串行 OCR**：`pdf.rs` 原来 `loop` 逐页 `ocr_image`，N 页线性累加 54.4s/页。修复：改为 Rayon `par_iter` 并行处理全部页面，按页码顺序收集结果，多页 PDF 总耗时接近单页耗时 × 页数/核数
  - **渲染 DPI 300→200**：`-r 300` 渲染 870 万像素（A4 2480×3508），检测模型 `det_limit_side_len=960` 只消费 960px，剩余像素浪费在 Lanczos3 下采样。降到 200（1654×2339），缩放计算减少 ~2.25 倍，仍高于 960px 需求（150 仅再省 ~1s，准确率风险不值得）
  - **新增分阶段耗时诊断**：`paddleocr::recognize_with_metrics_from_path` 输出 decode/det(pre,inf,post)/rec(pre,inf,post) 各阶段秒数；`tests/test_pdf_ocr.rs` 新增 `test_ocr_bench_single_page` 实测基准
  - **修复 `extract_text` 签名变更遗漏**：`b688d1c` 将 `extract_text(path)` 改为 `extract_text(path, lang)`，但 `tests/integration.rs` 5 处调用未同步 → 编译错误。补上 `"eng"` 参数（`src-tauri/tests/integration.rs`）
  - **引擎池大小尊重 `ocr_concurrent` 设置**：新增 `paddleocr::set_pool_size(n)`（0=自动，上限 8），`lib.rs` 启动时在 DB 初始化后、health_check 前读取 `app_settings.ocr_concurrent` 注入（health_check 会惰性构建池，必须提前注入）。顺带修复前后端键名不一致：前端 `Settings.tsx` 用 `ocr_concurrency` 但后端白名单 + DB 种子均为 `ocr_concurrent` → 保存被后端以 "unknown setting key" 拒绝，该设置从未生效（`src-tauri/src/extractor/paddleocr.rs`、`src-tauri/src/lib.rs`、`src/pages/Settings.tsx`）
  - 涉及文件：`src-tauri/src/extractor/paddleocr.rs`、`src-tauri/src/extractor/pdf.rs`、`src-tauri/src/lib.rs`、`src-tauri/tests/integration.rs`、`src-tauri/tests/test_pdf_ocr.rs`、`src/pages/Settings.tsx`、`CHANGELOG.md`

---

## 2026-08-03（PDF OCR 提速：渲染 DPI 300→200）

- **PDF OCR 每页 30-60 秒过慢**：根因是 `ocr_pdf_via_pdftoppm` 硬编码 `-r 300` 渲染 870 万像素大图（A4 2480×3508），而检测模型 `det_limit_side_len=960` 只消费 960px，剩余像素全浪费在 Lanczos3 下采样上。修复：渲染 DPI 降到 200（A4 1654×2339），缩放计算量减少约 2.25 倍，仍高于检测模型所需 960px。实测各阶段耗时验证见 `tests/test_pdf_ocr.rs`（`src-tauri/src/extractor/pdf.rs`）

---

## 2026-08-03（浏览页扩展名排序修复 + 已索引文件手动重索引确认 + macOS LibreOffice 路径探测）

- **浏览页分页「翻到第 N 页后空白」根因修复**：前端传 `page_size`（snake_case）但 Tauri 2 `#[tauri::command]` 默认将 Rust 参数名 `page_size` 转为前端 `pageSize`（camelCase）→ 参数丢失，后端走默认 `ps=50`，而前端按 `pageSize=20` 计算 `totalPages` → 翻页时 offset 与实际页面对不上（例：第 109 页前端 offset=2160，后端 offset=5400>5358→零行）。修复：前端 `listFilesDb` 参数名 `pageSize` 对齐 Tauri 自动驼峰命名（`src/api/files.ts`、`src/pages/Browse.tsx`）
- **浏览页「扩展名 A-Z」排序失效 + FileTypes 类型统计为空**：根因是 `file_tracking` 表从未存在 `file_ext` 列，但 `list_files_db` 的 `ORDER BY file_ext` 和 `get_file_type_stats` 的 `GROUP BY file_ext` 都引用了它 → 两个查询直接报错（前端 `.catch` 静默吞掉）。修复：schema 升级到 v2，新增 `file_ext` 列 + `ensure_file_ext_column` 幂等迁移（ALTER TABLE + Rust 侧 `Path::extension()` 回填，避免目录名含点误判）；`upsert_file`/`update_file_path`/`migrate_paths_to_relative` 同步维护该列（`src-tauri/src/db/mod.rs`、`src-tauri/src/db/tracker.rs`）
- **已索引文件手动重索引无确认**：右键菜单「手动索引」对已索引（indexed=1）文件直接执行，可能覆盖现有索引。修复：前端 `handleReindex` 用 `ask()` 弹确认框「该文件已索引，重新索引将重新提取并覆盖现有索引数据」，确认后才执行；新增 `confirm_reindex` i18n 键。失败/待索引文件仍直接执行（`src/pages/Browse.tsx`、`src/i18n/zh.ts`、`src/i18n/en.ts`）
- **浏览页页码越界空白**：当结果集缩小时（如重扫后失败文件减少），`page` 可能超过 `totalPages`，停留在空页。修复：新增 effect 将 `page` 钳制到有效范围（`src/pages/Browse.tsx`）
- **macOS 默认 soffice 路径不可解析**：`determine_lo_binary` 先返回 config 中默认的裸 `"soffice"`，macOS GUI 应用 PATH 不含 brew 路径 → 永远找不到真路径。修复：config 默认改为空（自动探测）；`determine_lo_binary` 在 config 为默认值时跳过，按顺序探测 `/opt/homebrew/bin/soffice` → `/usr/local/bin/soffice` → `/Applications/LibreOffice.app/...`；新增 `resolved_lo_binary()`，依赖面板显示真实解析路径而非 `soffice`（`src-tauri/src/extractor/office/mod.rs`、`src-tauri/src/config.rs`、`src-tauri/src/commands/tesseract.rs`）
- **新增分页回归测试**：`tests/test_pagination.rs` 验证 507 行数据时第 11 页返回 20 行、带 ext 参数时 LIMIT/OFFSET 绑定正确（`src-tauri/tests/test_pagination.rs`）

---

## 2026-08-02（Bug 修复：macOS LibreOffice headless 调用失败）

- **扫描会话级 LibreOffice Dock 图标抑制（revert 持久写入）**：`自启动时持久注入 LSUIElement=true` 改为 `LoBackgroundGuard` RAII guard——仅在扫描会话期间临时设置，扫描完成后自动恢复。避免持久写入导致用户正常使用 LO 时无 Dock 图标。新增 crash recovery：启动时若检测到残留 LSUIElement（上次扫描崩溃），自动清除。Guard 覆盖 `trigger_scan`、`rebuild_index`、`startup_scan` 三个入口（`src-tauri/src/extractor/office/mod.rs`、`src-tauri/src/commands/index.rs`、`src-tauri/src/lib.rs`）

- **并发 soffice 共享默认 profile 锁竞争**：`batch_index` Rayon `par_iter` 并发提取 .doc/.xls/.ppt 时多个 `soffice` 进程争用同一用户 profile `.lock` → 超时/崩溃。修复：每次调用使用独立 temp profile（`-env:UserInstallation=file://{unique}` + `--norestore --nolockcheck --nofirststartwizard`）
- **超时后子进程不 kill**：`extract_via_libreoffice` 60s 超时后 orphan 进程继续持锁 → 后续调用全挂。修复：`Arc<Mutex<Child>>` 跟踪子进程，超时时 kill + wait
- **移除无用的 LSUIElement guard**：`defaults write org.libreoffice.script LSUIElement 1` 写 preferences 域不会影响 LaunchServices（只读 Info.plist）。`SAL_USE_VCLPLUGIN=svp` 已绕过 AppKit，故删除死代码（`src-tauri/src/extractor/office/mod.rs`）
- **`check_binary` 无超时**：首次运行 profile 创建可超过默认超时。改为 `spawn + try_wait` 轮询 + 15s 超时 kill（`src-tauri/src/extractor/office/mod.rs`）

- **batch_index 定期 auto-commit 持有 MutexGuard 时调用 self.commit() 导致自死锁**：`batch_index`（263 行）获取 `self.writer` Mutex 后，`guard` 存活期间调用 `self.commit()`（347 行），`commit()` 内部再次 `self.lock_writer()` 拿同一把锁 → `std::sync::Mutex` 不可重入 → 自己等自己永久卡死。修复：`self.commit()` 改为 `Indexer::commit(writer)` 直接复用已持有的 writer。同样修复 `index_file`（`src-tauri/src/indexer.rs`）

## 2026-08-02（Batch 2+3：浏览页右键菜单 + 列宽拖拽 + 页码输入 + 复制修复）

- **浏览页右键菜单**：文件行新增 `onContextMenu`，弹出菜单含 **打开**（`openFile`）、**在 Finder 中显示**（`revealInFolder`）、**手动索引**（调 `reindex_file` 后刷新列表）。移植自 `ResultList.tsx` 的右键模式，document click 自动关闭。修复 `open_file`/`reveal_in_folder` 相对路径未解析为绝对路径的 bug——DB 存相对路径，需通过 `dir_config` 拼接（`src/pages/Browse.tsx`、`src-tauri/src/commands/files.rs`）
- **新增 `reindex_file` 命令**：支持手动逐文件重索引，查 DB 记录 → 解析绝对路径 → 调用 `indexer.index_file`。已注册 invoke_handler，前端封装 `reindexFile`（`src-tauri/src/commands/index.rs`、`lib.rs`、`src/api/index.ts`）。i18n 新增 `reindex` 键
- **列宽可拖拽**：每列独立 width 状态，表头间加 `cursor-col-resize` 拖拽手柄（onMouseDown → mousemove → mouseup），最低 80px。移植自 `PreviewPanel.tsx` 的 drag resize 模式（`src/pages/Browse.tsx`）
- **页码输入框**：前后翻页按钮间插入 `go_to` 数字输入框，Enter/失焦跳转，1..totalPages 校验。移植自 `SearchPage.tsx` 模式（`src/pages/Browse.tsx`）
- **复制命令去除平台前缀**：`Settings.tsx` `filterGuide` 现在 strip 了 `"macOS:"`/`"Windows:"`/`"Linux:"` 前缀，复制的是纯命令（`src/pages/Settings.tsx`）
- **分页空页确认**：`b1ba768` 已修复 SQL 参数错位，count/data 查询共享同一 WHERE，当前 HEAD 代码正确，无需修改

- **取消扫描无效**：`cancel_scan` 标志只在 commands/index.rs 的目录边界检查，scanner 和 indexer 的 walk 循环从不读取。修复：`Scanner`/`IndexerService` 加 `cancel_scan: Arc<AtomicBool>` 字段，通过 `with_cancel()` 构造注入；三个 walk 循环（full/incremental/startup）每文件检查标志，`batch_index` Phase 1 par_iter + Phase 2 循环均检查，取消后跳过剩余文件并提交已完成部分。取消触发的文件不会标记 failed（`src-tauri/src/scanner/mod.rs`、`indexer.rs`、`lib.rs`）
- **启动/增量扫描不重试失败文件**：用户意图——失败文件仅通过手动触发（右键"手动索引"）重试，不应自动重试。原 `needs_reindex` 对 Failed(indexed=2) 返回 true 导致 startup_scan 也自动重试。修复：去掉 Failed 条件，失败文件与正常已索引文件行为一致（仅 mtime 变化时重试）。同步修复增量扫描的 mtime 门（`src-tauri/src/scanner/helpers.rs`、`mod.rs`）
- **水印扫描件 PDF 不触发 OCR**：`pdf.rs` 的"干净文本"判定仅用 >50 字符 + 水印/乱码检测，单页或页间变化水印被漏过。修复：新增 `is_repetitive()`（≥100 字符 + ≥3 行 + >60% 重复行 ratio），阈值提至 100 字符，加 `is_rep` 条件。同时修复 `indexer.rs` 的 OCR 回退对 PDF 的错误调用（`ocr_image` 不解码 PDF → 统一跳过，PDF 内已有 OCR 逻辑）（`src-tauri/src/extractor/pdf.rs`、`indexer.rs`）

- **add_dir 内部触发扫描 + 前端 triggerScan 并发导致 IndexWriter 死锁**：`add_dir` 命令内部 `spawn_blocking(incremental_scan)`（扫描 A）和前端 `useDirs.ts` 的 `triggerScan()`（扫描 B）并发执行，两个 `full_scan` 竞争同一个 Tantivy `IndexWriter` Mutex → 两者都卡在 `lock_writer()` 上，Tantivy 线程全部 idle，扫描永远不会打印"扫描完成"。修复：去掉 `add_dir` 内部的 `incremental_scan`，仅保留 watcher 启动；扫描由前端 `triggerScan()` 独占执行（已有 `compare_exchange` 并发保护）。根因使用 `sample` 命令栈分析确认（`src-tauri/src/commands/dirs.rs`）

---

- **Semgrep 静态分析集成**：新增 `.semgrep/custom.yml`（13 条自定义规则：Rust 锁中毒/panic/fs::copy/错误吞没 + TS JSON.parse/setInterval/clipboard）；叠加官方规则集 p/rust、p/typescript、p/react、p/owasp-top-ten、p/secrets；分 ERROR/WARNING/INFO 三级，ERROR 阻塞提交零容忍；`rust-unwrap-panic`/`rust-expect-panic` 排除测试目录；`rust-rwlock-read-unwrap` 在 5 处内联测试加 `nosemgrep` 注释（`.semgrep/custom.yml`、`AGENTS.md`、`src-tauri/src/indexer.rs`、`src-tauri/src/scanner/mod.rs`）
- **AGENTS.md 提交流程**：新增步骤 3.5 "Semgrep 检查 → semgrep scan --severity ERROR 零发现" + "静态分析节"三级体系说明 + 子任务禁止改规则声明（`AGENTS.md`）

---

## 2026-08-02（三项安全加固：原子迁移 + 单实例 + 交叠检测）

- **原子化迁移**：`migrate_data` 重写为 async，采用 tmp→fsync→原子 rename 模式（先拷到 `.migrate-tmp-{uuid}`，完整落盘后 `fs::rename` 到目标）。迁移期间暂停扫描+watcher，emit 进度事件（前端显示进度条）。拷贝失败则回退清理 tmp（旧目录不动）；拷贝成功+删除旧目录失败仅弹警告，迁移仍算完成。新增「目标不能是旧目录子目录」防护。`save_config` 在 `remove_dir_all` 之前执行，防止删除后写配置失败导致指向空目录（`src-tauri/src/commands/config.rs`、`src/pages/Settings.tsx`、`src/api/config.ts`）
- **单实例限制**：新增 `tauri-plugin-single-instance` v2，注册在 Builder 最前面。第二实例启动时激活已有窗口（show+set_focus）后自动退出。`--data-dir` 实例同样受限制（`src-tauri/Cargo.toml`、`src-tauri/src/lib.rs`）
- **数据目录与监控目录交叠检测**：新增 `commands/helpers.rs` `check_data_dir_overlap`（canonicalize + 组件感知 `starts_with`，带非存在路径词法回退 + macOS `/tmp`→`/private/tmp` symlink 一致化）。三个入口全部检测——`add_dir` 拒绝、`migrate_data`/`update_config` 拒绝、`--data-dir` 启动拒绝。启动时对已存在交叠仅 `log::warn` 不阻断（`src-tauri/src/commands/helpers.rs`、`dirs.rs`、`config.rs`、`main.rs`、`lib.rs`）
- **ipc_test 适配**：fixture data_dir 改为子目录，避免 TempDir 与 add_dir 目标碰撞新交叠检查（`src-tauri/tests/ipc_test.rs`）

---

## 2026-08-02（测试修复：绝对路径→相对路径重构后集成测试）

- **test_incremental_scan 查询路径格式错误**：`file_tracking` 表 `path` 字段已改为存相对路径，但 `integration.rs` 的 `test_incremental_scan` 仍用 `env.dir_path.join("<filename>")` 绝对路径查询，导致 `None.unwrap()` panic。改为传相对路径字符串（`src-tauri/tests/integration.rs`）
- **test_pdf_multiple_pages OCR 断言不稳定**：PaddleOCR（PP-OCRv5）对程序化 PDF 渲染页识别存在大小写/空格误差（"PageTwo"→"PageTWo"），原精确匹配断言导致测试失败。新增 `contains_ignore_case` 辅助函数（大小写+空白归一化后子串匹配），断言改用该函数（`src-tauri/src/extractor/pdf.rs`）
- **ipc_test init_db 签名未同步**：`init_db` 改为 `&Connection` 参数后，`ipc_test.rs` 仍按旧签名传 `db_str` 导致编译错误。改为先 `Connection::open(db_str)` 再传 `&conn`（`src-tauri/tests/ipc_test.rs`）

---

## 2026-08-02（性能测试套件）

- **新增 perf_scan.sh**：`scripts/perf_scan.sh` 提供扫描性能基准测试能力，自动清理临时数据目录、启动应用、监控 RSS 内存（每 5s），扫描完成后输出文件数、索引/DB 大小、内存峰值/均值报告（`scripts/perf_scan.sh`）；同步更新 README 测试章节（`README.md`、`CHANGELOG.md`）

---

## 2026-08-02（UX 修复：Onboarding 重复 + 路径溢出 + ESC 关闭 + 大小写 + 导出 + 加载态）

- **OnboardingWizard 反复出现**：`App.tsx` 原来只检查 settings 中的 `onboarding_done`，清空目录后 settings 可能被重置导致弹窗重现。改为优先读 `localStorage['onboarding_completed']`，关闭时同时写入 localStorage 和 settings（`src/App.tsx`）
- **Browse 路径列溢出**：`Browse.tsx` 路径 `<td>` 的 `max-w-[280px]` 改为 `max-w-[200px]`，配合已有的 `truncate` 和 `title` 属性（`src/pages/Browse.tsx`）
- **PreviewPanel 全屏无 ESC 退出**：`PreviewPanel.tsx` 新增 `useEffect` 监听 `keydown Escape`，`fullscreen` 为 true 时调用 `onClose()`（`src/components/PreviewPanel.tsx`）
- **搜索英文大小写敏感**：`useSearch.ts` `setQuery` 统一 `toLowerCase()` 后再存 state；`search.rs` 两个 `SearchParams`（搜索/导出）构造时均 `query.to_lowercase()`，Tantivy 查询完全大小写不敏感（`src/hooks/useSearch.ts`、`src-tauri/src/commands/search.rs`）
- **导出失败无具体原因**：`SearchPage.tsx` 已有 `export_failed` 带 `error` 占位符，确认无需改动（`src/pages/SearchPage.tsx`）
- **搜索中导出按钮可重复触发**：`SearchPage.tsx` 导出按钮加 `disabled={search.status === 'loading'}` 防止并发（`src/pages/SearchPage.tsx`）

---

## 2026-08-02（中危修复：栈溢出 + DB 错误致命化 + OOM 风险）

### 🟡 中危修复（MED-1 ~ MED-9）
- **MED-1 backup dir_size 无界递归**：`backup.rs` 原 `dir_size` 递归遍历深层目录可导致栈溢出。改为迭代式 breadth-first 遍历（`vec` + `while let Some(dir) = dirs.pop()`）（`src-tauri/src/commands/backup.rs`）
- **MED-2 indexer dedup DB 错误致命化**：`indexer.rs` 原 `get_content` 瞬时 DB 错误直接 `return Err`，导致该文件索引完全放弃。改为 `log::warn!` 后落入提取逻辑（`src-tauri/src/indexer.rs`）
- **MED-3 export page_size 绕过 max_results**：`search.rs` `export_search_results` 原硬编码 `page_size: 10000`，无视用户设置的最大结果数。改为读取 `app_settings.max_results`，上限钳制至 5000（`src-tauri/src/commands/search.rs`）
- **MED-4 list_files 全量加载（已废弃）**：`files.rs` `list_files` 命令不再被前端调用（仅 `list_files_db` 使用），保留代码不变，标记废弃（`src-tauri/src/commands/files.rs`）
- **MED-5 metadata 失败绕过下载检查**：`files.rs` `download_files` 原 `metadata().map(...).unwrap_or(0)` 在权限失败时静默返回 0，绕过 500MB 检查。改为区分"文件不存在"（继续处理）和"权限不足"（报错）（`src-tauri/src/commands/files.rs`）
- **MED-6 list_files_db 缺重建守卫**：`files.rs` `list_files_db` 原缺少 `is_rebuilding` 检查，索引重建期间可读到空表。补充守卫，与 `search` 命令保持一致（`src-tauri/src/commands/files.rs`）
- **MED-7 FilterPanel 类型统计静默吞错**：`FilterPanel.tsx` 原 `getFileTypeStats` 失败 `.catch(() => {})` 静默忽略。改为 `.catch(e => console.error(...))` 记录错误（`src/components/FilterPanel.tsx`）
- **MED-8 list_files_db page_size 无上限**：`files.rs` `list_files_db` 原 `page_size.max(1)` 无上界，极端参数可致 OOM。补充 `.min(1000)` 上限（`src-tauri/src/commands/files.rs`）
- **MED-9 list_dir_entries 已删除文件仍显示**：`files.rs` `list_dir_entries` 原对软删除文件仍展示状态。改为跳过（`continue`）`status='deleted'` 的记录，不纳入目录列表（`src-tauri/src/commands/files.rs`）

---

## 2026-08-01（高危后端安全修复：线程 OOM + 进程无超时 + 路径失配）

### 🔴 高危修复（HIGH-1 ~ HIGH-4）
- **HIGH-1 watcher 无限制线程 spawn**：`lib.rs` 原每个文件事件内层 `std::thread::spawn` 导致大量文件变更时 OOM。改为单线程串行处理（恢复旧行为），`handle_event` 本身很快无需独立线程（`src-tauri/src/lib.rs`）
- **HIGH-2 pdftoppm 无超时阻塞扫描**：`pdf.rs` 原用 `.status()` 无限等待大 PDF 渲染。改为 `.spawn()` + 后台线程 `recv_timeout(120s)`，超时后用 `pkill -f pdftoppm` 终止进程（`src-tauri/src/extractor/pdf.rs`）
- **HIGH-3 PaddleOCR/Tesseract 无超时锁死全局 Mutex**：`paddleocr.rs` 新增 `with_engine_timed`（后台线程 + 120s channel timeout），`recognize_from_image` 改用；`ocr.rs` Tesseract 同样改为 spawn + 后台线程 wait + 120s timeout，超时 pkill（`src-tauri/src/extractor/paddleocr.rs`、`src-tauri/src/extractor/ocr.rs`）
- **HIGH-4 Windows 路径分隔符 mismatch**：`watcher.rs` `find_matching_dir` 原用 `Path::starts_with`，DB 存 `/` 但 watcher 给 `\` 导致失配。改为两边均 normalize 为 `/` 后比较（`src-tauri/src/scanner/watcher.rs`、`src-tauri/src/scanner/helpers.rs`）

---

## 2026-08-01（错误处理与内存序修复）

### 前端
- **clipboard 复制静默吞错**：`PreviewPanel.tsx`、`ResultList.tsx` 中 `.catch(() => {})` 改为 `.catch(e => console.warn('复制失败:', e))`，失败时记录警告而非静默忽略；启动加载/i18n 等合理静默降级路径保留不变
- **navigator.platform 弃用注释**：`Settings.tsx:470` 保留 `navigator.platform` 判断（项目未引入 `@tauri-apps/plugin-os`），加注释说明弃用状态及升级路径

### 后端
- **is_scanning 原子序加强**：`commands/index.rs` 两处 `load(Ordering::Relaxed)` 改为 `Ordering::Acquire`（线程序列化读取扫描状态），`cancel_scan` 已有 `Release`/`Acquire` 无需改动；`commit_counter`/`commit_interval` 保留 `Relaxed`（纯统计无同步语义）（`src-tauri/src/commands/index.rs`）

---

## 2026-08-01（安全修复：大文件 OOM + ReDoS + unsafe impl）

### P1 级安全修复
- **P1-2 大文件无大小限制导致 OOM**：`commands/files.rs` 下载时检查 `metadata.len()`，>500MB 直接报错"文件过大，无法下载"；`scanner/mod.rs` 移位检测跳过 >10MB 文件的 MD5 计算；`extractor/text.rs` 用 `.take(10*1024*1024)` 限制纯文本提取读取上限（`src-tauri/src/commands/files.rs`、`src-tauri/src/scanner/mod.rs`、`src-tauri/src/extractor/text.rs`）
- **P1-3 unsafe impl Send/Sync 加说明**：`paddleocr.rs` 中 `SendEngine` 的 `unsafe impl Send` / `unsafe impl Sync` 注释补充说明原因（`OcrEngine` 含非 Send 内部可变性，Mutex 串行化访问保证安全），保留 unsafe impl 不可移除（`src-tauri/src/extractor/paddleocr.rs`）
- **P1-4 ReDoS 已知低风险**：`PreviewPanel.tsx` 的 `highlightText` 已有转义 + 限 20 词，加注释说明 ReDoS 风险已评估为低风险，保持现状（`src/components/PreviewPanel.tsx`）
- **P1-1 JSON.parse 已修复**：`useSearch.ts` 中 `loadFromStorage` 已有 try/catch，无需改动

---

## 2026-08-01（第七轮：WAL 一致性修复）

### 🔴 数据库备份/迁移一致性
- **trigger_backup 直接 fs::copy 活跃 WAL DB**：`backup.rs` 原用 `std::fs::copy(&state.db_path, &db_dest)` 复制活跃 SQLite 数据库，WAL 模式下源文件与 WAL/SHM 分离，产生数据撕裂。改为 `rusqlite::backup::Backup::new(&src_conn, &mut dst_conn)` + `step(-1)` 在线备份，Busy/Locked 重试 3 次（与 `restore_backup` 模式一致，`backup.rs`）
- **migrate_data 直接 fs::copy 活跃 WAL DB**：`config.rs` 迁移时同样 `fs::copy` 活跃 DB。改为相同 Backup API 模式，目标 DB 由 `Connection::open` 创建（新文件），源 DB 保持活跃，Busy/Locked 重试 3 次（`config.rs`）

---

## 2026-08-01（今日）

### 生产代码 unwrap/expect 清理
- **lib.rs 启动链 4 处 expect → ?）**：`.setup()` 闭包已返回 `Result`，将 `db::get_pool`、`db_pool.get()`、`db::init_db`、`IndexManager::open_or_create` 四处的 `.expect()` 改为 `?` 传播，启动失败时返回错误而非 panic（`src-tauri/src/lib.rs`）
- **cli.rs 3 处 expect → ?）**：`run_cli()` 返回类型改为 `Result<()>`，三处 `.expect()` 改 `.context(...)?`，`main.rs` 捕获错误并 `exit(1)` 输出到 stderr（`src-tauri/src/cli.rs`、`src-tauri/src/main.rs`）

---

## 2026-08-01（第七轮：WAL 一致性修复）
- **3-21 getDuplicates 高频触发**：`IndexStatus.tsx` 原 useEffect 依赖 `status?.total_files`，total_files 每次变化都重新调用。改为监听 `scan-completed` 事件，仅在扫描完成后调用一次（`src/pages/IndexStatus.tsx`）
- **3-23 clipboard.writeText 未 catch**：`PreviewPanel.tsx` 和 `ResultList.tsx` 的 `navigator.clipboard.writeText` 调用缺失 `.catch()`，可能未处理拒绝。添加空 catch 处理（`src/components/PreviewPanel.tsx`、`src/components/ResultList.tsx`）
- **3-26 rebuild setTimeout(1000) 硬编码等待**：`useIndexStatus.ts` 的 `rebuild` 在 `await rebuildIndex()` 后硬编码 `setTimeout(1000)`。删除该等待，由已有 5s/30s 自适应轮询刷新状态（`src/hooks/useIndexStatus.ts`）
- **3-30 a11y 基础改进**：`SearchBar.tsx` 搜索框加 `aria-label={t('search')}`，清除按钮加 `aria-label={t('clear_search')}`；`FilterPanel.tsx` 面板加 `role="region"` + `aria-label`；同时新增 `clear_search` i18n 键（`src/components/SearchBar.tsx`、`src/components/FilterPanel.tsx`、`src/i18n/en.ts`、`src/i18n/zh.ts`）
- **3-31 FilterPanel selectedSet 每次渲染重建**：`selectedSet = new Set(dirPaths)` 改为 `useMemo(() => new Set(dirPaths), [dirPaths])`；扩展名列表从 `getFileTypeStats()` 读取真实类型分布，无数据时回退到 `COMMON_EXTS`（`src/components/FilterPanel.tsx`）
- **3-32 LogViewer key=索引导致 DOM 复用错位**：`LogViewer.tsx` 日志列表原用数组索引作为 key，过滤切换时 React 错误复用 DOM 元素。改为 `logKey(line, i)` 基于行内容前 24 字符生成唯一 key（`src/pages/LogViewer.tsx`）

---

## 2026-08-01 (今日)

### R4-A 并发与状态修复（5 项）
- **3-1 PaddleOCR Mutex.lock().unwrap() 中毒 panic**：`with_engine` 内 `lock().unwrap()` 改为 `lock().unwrap_or_else(|e| e.into_inner())`，poisoned mutex 时恢复内层值而非崩溃（`src-tauri/src/extractor/paddleocr.rs`）
- **3-2 watcher 单线程串行阻塞**：事件循环中 `handle_event` 直接调用会阻塞后续事件接收。改为对每个 event 独立 `std::thread::spawn`，大文件处理不再阻塞 watcher 线程（`src-tauri/src/lib.rs`）
- **3-3 init_db 重复创建连接池**：原 `init_db` 内部调用 `get_pool` 新建独立池，与主池隔离。改为接收 `&Connection` 参数，由调用方传入主池连接（`src-tauri/src/db/mod.rs`、`src-tauri/src/lib.rs`）
- **3-4 启动 VACUUM 无条件执行**：VACUUM 持有 SQLite 独占锁，对小于 100 MiB 的 DB 是浪费。改为先 `std::fs::metadata` 检查文件大小，仅超过 100 MiB 时才执行 VACUUM（`src-tauri/src/lib.rs`）
- **3-5 PRAGMA foreign_keys 仅首个连接生效**：原 `get_pool` 在初始连接上 `execute_batch` 设置 WAL+FK，新池连接不继承。改为 r2d2 `connection_customizer` + `CustomizeConnection::on_acquire`，每个新连接自动执行 `PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;`（`src-tauri/src/db/mod.rs`）

---

## 2026-08-01 (今日)

### R4-B 后端代码质量改进
- **3-6 batch_index/index_file 去重**：抽 `extract_and_index_single` 共享函数，消除 `batch_index` Phase 1 与 `index_file` 间约 150 行重复的读文件→MD5→去重→提取逻辑（`src-tauri/src/indexer.rs`）
- **3-9 IndexedState / FileStatus 枚举**：在 `tracker.rs` 新增 `IndexedState { Pending=0, Indexed=1, Failed=2 }` 和 `FileStatus { Active, Deleted }` 枚举，替换散落的字面量比较（`helpers.rs`、`scanner/mod.rs`、`commands/files.rs`）
- **3-11 SortField 枚举**：在 `searcher.rs` 新增 `SortField { Score, Date, Size, Name }` 枚举，`SearchParams.sort` 从 `String` 改为 `SortField`，`commands/search.rs` 和 `cli.rs` 同步更新（`src-tauri/src/search/searcher.rs`、`src-tauri/src/commands/search.rs`、`src-tauri/src/cli.rs`、`tests/integration.rs`）
- **3-12 日志 hash 截断修复**：原 `{hash:.8}` 对 String 是格式宽度而非截断，改为 `&hash[..8.min(hash.len())]`（`src-tauri/src/indexer.rs`）
- **3-16 settings key 白名单**：`update_settings` 新增 `ALLOWED_KEYS` 白名单，未知 key 直接拒绝（`src-tauri/src/commands/settings.rs`）
- **3-7 scan方法统一DiskEntry**：将 `startup_scan` 内局部 `DiskEntry` 结构体移至 `helpers.rs` 作为公共结构，`full_scan`/`incremental_scan`/`startup_scan` 三处统一使用 `DiskEntry { abs_path, rel_path, size, name }`，消除 `Vec<String>` 与 `Vec<DiskEntry>` 不一致的问题（`src-tauri/src/scanner/helpers.rs`、`src-tauri/src/scanner/mod.rs`）
- **3-8 扩展名判断去重**：在 `extractor/mod.rs` 新增 `pub fn classify_ext(ext: &str) -> &str` 统一分类逻辑，`commands/files.rs` 中 5 处重复的 `matches!(ext.as_str(), ...)` 替换为 `classify_ext` 调用（`src-tauri/src/extractor/mod.rs`、`src-tauri/src/commands/files.rs`）
- **3-10 update_dir 触发 watcher 重启**：`update_dir` 更新目录配置后，停止并重启 watcher，使 exclude/include 模式变更即时生效（`src-tauri/src/commands/dirs.rs`）
- **3-13 add_dir 触发首次扫描**：`add_dir` 添加目录后，在启动 watcher 的同时异步触发 `incremental_scan`，确保新目录内容被立即索引（`src-tauri/src/commands/dirs.rs`）
- **3-14 搜索 page_size 上限保护**：`search` 命令的 `page_size` 增加 `.min(1000)` 上限，防止极端参数导致内存溢出（`src-tauri/src/commands/search.rs`）
- **3-15 download_files 临时目录清理**：`download_files` 已使用 `TempDir::new("ls_download")` 管理 zip 临时文件，Drop 时自动清理（`src-tauri/src/commands/files.rs`）
- **3-17 后台清理任务**：启动扫描完成后已在 `lib.rs` 调用 `cleanup_orphan_content` 和 `vacuum`，无遗漏（`src-tauri/src/lib.rs`）
- **3-18 移位检测 O(n·m)→O(n) 优化**：`startup_scan` 中移位检测由线性 `find` 改为按 `(name, size)` 建 `HashMap` 索引，将每次 DB 记录查找从 O(n) 降至 O(1)，整体从 O(n·m) 降至 O(n+m)（`src-tauri/src/scanner/mod.rs`）

---

## 2026-08-01 (今日)

### R4-C 前端质量改进
- **3-19 焦点判断改用 data-search-input**：SearchPage.tsx 原用 `placeholder.includes()` 判断焦点在搜索框，中文模式下失效。改为 `activeEl?.closest('[data-search-input]')`，与 SearchBar.tsx 的 `data-search-input="true"` 属性匹配（`src/pages/SearchPage.tsx`）
- **3-20 useIndexStatus 动态轮询间隔**：原固定 5s 轮询，改为 `is_scanning` 时 5s、空闲时 30s，减少索引空闲时的无效请求（`src/hooks/useIndexStatus.ts`）
- **3-22 SearchPage setTimeout 泄漏修复**：handleExport 内 3 个 `setTimeout` 无清理，用 `timersRef` + unmount effect 统一清理，防止组件卸载后状态更新崩溃（`src/pages/SearchPage.tsx`）
- **3-24 添加 ErrorBoundary**：新建 `src/components/ErrorBoundary.tsx`，在 App.tsx 外层包裹，渲染错误时显示"应用出错了，请重启或查看日志"而非白屏（`src/components/ErrorBoundary.tsx`、`src/App.tsx`）
- **3-25 暗色模式闪烁修复**：theme.tsx 原 `useState('light')` 初始值导致首次渲染闪烁。改为 `useMemo` 同步计算 resolved 值，DOM 初次渲染即正确（`src/theme.tsx`）
- **3-27 formatSize/formatTime 去重**：新建 `src/utils/format.ts` 统一工具函数，删除 PreviewPanel.tsx、ResultList.tsx 中的重复定义，减少维护成本（`src/utils/format.ts`、`src/components/PreviewPanel.tsx`、`src/components/ResultList.tsx`）
- **3-29 alert() 替换为 Tauri message()**：Settings.tsx 迁移失败提示原用浏览器 `alert()`，改为 `@tauri-apps/plugin-dialog` 的 `message()`，保持应用内 Dialog 风格一致（`src/pages/Settings.tsx`）

---

## 2026-08-01 (今日)

### 全量 i18n 改造
- **前端硬编码字符串全部提取到 en.ts / zh.ts**（`src/i18n/`）：新增 ~112 个翻译 key，覆盖 SearchPage、Browse、IndexStatus、DirManager、LogViewer、FileTypes、SearchBar、ResultList、PreviewPanel、FilterPanel、StatusBar、OnboardingWizard 共 12 个组件/页面
- **t() 支持参数**：`src/i18n/index.tsx` 扩展 `t(key, params?)` 签名，支持 `{placeholder}` 模板替换（如 `t('saved_to', { path })`、`t('results_count', { total })` 等）
- **SearchPage 键盘检查修复**：原检查 `placeholder.includes('your documents')` 在中文模式下失效，改为 `dataset.searchInput` 属性（`src/components/SearchBar.tsx` 加 `data-search-input="true"`，`src/pages/SearchPage.tsx` 改查该属性）
- **涉及文件**：`src/i18n/en.ts`、`src/i18n/zh.ts`、`src/i18n/index.tsx`、`src/pages/SearchPage.tsx`、`src/pages/Browse.tsx`、`src/pages/IndexStatus.tsx`、`src/pages/DirManager.tsx`、`src/pages/LogViewer.tsx`、`src/pages/FileTypes.tsx`、`src/components/SearchBar.tsx`、`src/components/ResultList.tsx`、`src/components/PreviewPanel.tsx`、`src/components/FilterPanel.tsx`、`src/components/StatusBar.tsx`、`src/components/OnboardingWizard.tsx`

---

## 2026-08-01

### 路径处理修复
- **to_relative 前缀误匹配**：原实现用 `path_str.starts_with(&root_str)` 字符串前缀比较，会把 `/tmp/foobar` 误认为 `/tmp/foo` 的子路径。改用 `Path::strip_prefix`（组件感知），并新增回归测试 `to_relative_respects_component_boundary`（`src-tauri/src/scanner/helpers.rs`）
- **路径迁移字节语义**：`migrate_paths_to_relative` 原用 SQL `SUBSTR(path, ?)` 按字节长度截断，中文等多字节路径会截错。改为 Rust 侧逐行迁移：按 `dir_id + prefix%` 查询后用 `path.strip_prefix(prefix)`（带 `/` 边界安全）更新（`src-tauri/src/db/tracker.rs`）

### 扫描统计与启动流程修复
- **扫描总耗时被覆盖**：`trigger_scan` 与 `rebuild_index` 中多目录扫描累加 `total_duration_ms = r.duration_ms` 每次都覆盖为最后一个目录的耗时，改为 `+=` 累加（`src-tauri/src/commands/index.rs`）
- **watcher 启动窗口期丢事件**：原启动流程先启动扫描线程、扫描完成后才发 `StartWatch`，扫描期间的文件变更因 watcher 未启动而丢失。改为先在主线程读取目录列表并发送 `StartWatch`，再启动扫描线程（`src-tauri/src/lib.rs`）
- **delete_file 静默吞错**：`mark_deleted` 失败被 `match` 静默忽略，改为 `if let Err(e)` 记录 `log::warn!`（`src-tauri/src/indexer.rs`）

### 搜索目录筛选修复
- **LIKE `%`/`_` 通配符转义**：dir_paths → file_ids 查询中 `p.replace('%', "%%")` 无效（SQLite LIKE 不识别 `%%`），改用 `ESCAPE '\'` 转义 `%` 和 `_`，避免含特殊字符的目录路径匹配错误。`search` 与 `export_search_results` 两处路径解析均已修复（`src-tauri/src/commands/search.rs`）

### TypeScript strict 模式
- **开启 TS strict 模式**：`tsconfig.app.json` 添加 `"strict": true`，符合 AGENTS.md 规范（strict + 禁止 any）。现有 34 个 TS 文件经 `tsc --noEmit -p tsconfig.app.json` 验证零错误
- **移除 SearchBar 中 `as any[]`**：`dropdown` 合并 suggestions（`string[]`）与 history（`SearchHistoryEntry[]`）改用展开语法 `[...suggestions, ...history]`，类型自然推断为 `(string | SearchHistoryEntry)[]`（`src/components/SearchBar.tsx`）

### 前端功能正确性修复
- **R3-2 预览高亮奇数次错乱**：`highlightText` 用带 `g` 标志的 `regex.test(part)` 判断是否高亮，`lastIndex` 状态导致奇数个匹配时高亮错乱。改用 `Set`（术语小写集合）做成员判断，正则仅用于切分（`src/components/PreviewPanel.tsx`）
- **R3-3 NumberField 清空输入回退异常**：`parseInt(e.target.value, 10) || min` 把空串/`NaN` 静默写成 `min` 且在输入过程中无法清空，改为 `Number.isNaN(v) ? min : Math.max(min, v)` NaN 安全钳制（`src/pages/Settings.tsx`）
- **R3-4 No Results 页 `<a href>` 整页跳转**：HashRouter 下 `<a href="/index">` 触发整页刷新，改用 `react-router-dom` 的 `<Link to="/index">`（`src/pages/SearchPage.tsx`）
- **R3-5 Enter 提交后 debounce 重复请求**：`submitSearch` 立即执行搜索后，300ms debounce effect 又因 query 变化触发一次同参数请求。新增 `lastSubmittedRef` 记录最近一次提交键 `query|page|sortField|sortOrder`，debounce effect 命中即跳过（`src/hooks/useSearch.ts`）
- **R3-14 Browse 搜索无防抖**：搜索框每个字符都触发一次 `listFilesDb` 请求。新增 `debouncedSearch` state + 300ms setTimeout 防抖，`loadFiles` 改用防抖后的值（`src/pages/Browse.tsx`）
- **R3-15 快速点击文件预览竞态**：慢返回覆盖快返回。新增 `previewVersionRef` 版本号，`selectFile` 每次自增并捕获本地版本，await 返回后版本不匹配则丢弃（`src/pages/Browse.tsx`）
- **R3-16 设置项每键写库**：`handleFieldChange` 每个字符都调 `updateSettings`，改用 `saveTimerRef` 300ms 防抖合并写入，卸载时清理未落盘的定时器（`src/pages/Settings.tsx`）

---

## 2026-07-30

### 项目初始化
- **ed1a639** Initial commit：Tauri 2 + React 19 + Tantivy 搜索引擎 + Tesseract OCR
- **874a0e4** chore：忽略 Tantivy 索引缓存文件

---

## 2026-07-31（第一轮：PaddleOCR + 启动流程 + Bug 修复）

### 🚀 PaddleOCR 内置引擎
- **`0e609c4`** feat: PaddleOCR 内置引擎 + 启动扫描 + 实时监控
  - 集成 `pure-onnx-ocr`（tract 纯 Rust ONNX 推理），PP-OCRv5 模型编译进二进制
  - 引擎优先级：PaddleOCR(默认) → Apple Vision → Windows OCR → Tesseract
  - `include_bytes!` 内嵌 21MB 模型，零外部依赖
  - 新增 `startup_scan()` 启动自动扫描
  - 实时文件监控（notify 300ms 防抖）
  - 文件移位检测（MD5 哈希匹配）
  - 默认排除规则（`#` `$` `.` `~` 前缀文件 + `.tmp` `.bak` 后缀等）
  - 移除全局快捷键 Ctrl+Space
  - 更新 README + USER_MANUAL

### 🔴 Bug 修复（12 项）`45db344`
1. `took_ms` 实为微秒 → `as_micros()` → `as_millis()`（searcher.rs）
2. `mem::forget(watcher)` 线程泄漏 → watcher 存入 AppState
3. MD5 哈希不一致（文件字节 vs 文本字节）→ 统一文件字节 MD5
4. `upsert_file` ON CONFLICT 错误重置 `indexed=0` → SQL 加 CASE WHEN
5. `last_scan` 秒 vs `mtime` 微秒精度不匹配 → `timestamp_micros()`
6. CSV 导出 path 列写成 file_name → SearchHit 加 path 字段
7. OCR 引擎检查与 PaddleOCR 默认冲突 → 匹配区分各引擎
8. FileWatcher 只处理 paths[0] → 遍历所有 paths
9. CSV 不转义特殊字符 → 所有列转义
10. `db_path.to_str().unwrap()` 非 ASCII 路径崩溃 → `to_string_lossy()`
11. OCR 预处理临时文件 PID 并发冲突 → UUID 替代 PID
12. macOS LibreOffice Dock 图标闪烁 → LSUIElement RAII guard（`bae64db`）

### 🏗️ 架构改进（16 项）
- **`c898d07`** 架构/性能/安全改进集
  - 定期 commit（每 100 文件自动提交）
  - IndexReader 复用（缓存 + reload）
  - `content_suggest` 字段用于搜索建议
  - `sort=name` Rust 侧排序
  - `filename:` 正则解析（支持任意位置）
  - CLI data_dir 统一
  - 移除非关键 unwrap/expect
  - PaddleOCR `Mutex + Send/Sync` 安全包装
  - 取消扫描功能（`cancel_scan` AtomicBool）
  - 清理孤儿 content_index
  - 数据库 VACUUM
- **`59bb801`** 流式MD5 + WalkDir 超时 + watcher 自动重连
  - MD5 流式计算（BufReader 替代 read_to_end）
  - 文件大小上限 100MB，超大文件只读首尾 1MB
  - WalkDir 计数 3 秒超时保护
  - FileWatcher 后台线程自动重连（3 次重试，500ms 间隔）
- **`75c7501`** Rayon 并行索引：`batch_index` par_iter 并行提取 + 串行 Tantivy 写入
- **`f82c645`** dead code 清理：`process_event`/`handle_create_modify`/`handle_delete`、`RawTokenizer`

### 🎨 前端假功能修复（8 项）`73489ef`
1. 排序选择器"死控件" → 打通前端→API→后端 sort/sortOrder
2. Pause/Resume 假按钮 → 改为取消扫描按钮
3. 文件类型分布假数据 → 新增 `get_file_type_stats` 命令
4. Recent Changes 计算错误 → 新增 `ScanDelta` 追踪真实数据
5. CSV 导出无保存对话框 → 系统 `save()` 对话框
6. DEBUG eprintln 遗留 → 删除

### 🟠 可用性改进（11 项）`789a648`
1. PDF 预览添加 📄 标识 + OCR 文字标题
2. 大文件预览截断 50k 字
3. 图片缩放控件 `[-][100%][+]`
4. Enter 键冲突修复（焦点在搜索框时不触发 openFile）
5. No results 引导：清空筛选 + 索引链接
6. 筛选持久化 localStorage
7. mtime 单位修复（`ts*1000` → `ts/1000`，后端微秒→前端 ms）全部 6 处
8. 侧边栏 File Types i18n
9. 搜索历史在输入时保留
10. 分页加页码输入跳转
11. 设置页自动保存，移除 Save 按钮

---

## 2026-07-31（第二轮：路径重构 + 迁移修复）

### 📁 相对路径存储
- **`843de19`** refactor: 文件路径由绝对→相对路径存储
  - `file_tracking` 和 Tantivy 索引 path 改为相对路径（相对 dir_config.path）
  - 新增 `to_relative()` / `to_absolute()` 辅助函数
  - 支持跨平台索引复用

### 🔧 修复
- **`8c66d08`** fix: LO 路径 onBlur 保存 + ScanDelta 真实 deleted/modified 值
- **`ead6023`** fix: batch 索引错误日志显示文件名+路径
- **`d599b64`** fix: 迁移数据后 data_dir 被设为消息字符串而非新路径
- **`0c65e66`** fix: 迁移数据完整修复（catch 缺失 + 允许空目录）
- **`e8d2ab2`** fix: get_stats 只统计活跃文件（`WHERE status='active'`）+ 绝对→相对路径自动迁移

---

## 2026-08-01（第三轮：扫尾 + 体验修复）

### 🔧 最后 5 项修复
- **`0c7f67f`** fix: `needs_reindex()` 抽取到 helpers.rs + ScanResult.added 分离 + list_dir_entries 过滤 deleted

### 📖 文档
- **`57dd72b`** docs: 基于项目现状全面重写 README 和用户手册

### 🚀 功能
- **`0ed36ae`** feat: 数据迁移后自动重启（`restart_app` 命令）
- **`19c595a`** feat: 设置页添加外部依赖面板（PaddleOCR/pdftoppm/LibreOffice 状态 + 一键复制安装命令）

### 🔧 修复
- **`6181000`** fix: 7个 TypeScript 编译错误
- **`eed560b`** fix: 迁移后改为确认对话框
- **`63d3d06`** fix: 索引状态页 Details 按钮无响应（`get_index_errors` 未注册 Tauri 命令）

---

## 2026-08-01（第四轮：更多 Bug + 文档 + 自动变更日志）

### 🔴 严重 Bug
- **`03949ac`** 修复 5 个 UX 缺陷
  - 删除文件无反应：`mark_deleted` SQL `WHERE path=?` 错误接收 UUID，改为 `WHERE id=?`
  - `.DS_Store` 被实时索引：`handle_event` watcher 回调遗漏 `is_excluded` 检查
  - 设置页安装命令显示三个平台：前端按 `navigator.platform` 过滤当前平台
  - LO 路径输入与依赖检测分离：合并到依赖面板同一行
  - 索引状态 `pending` 和 `errors` 关系不清：Pending 卡片加 `incl. errors` 副标题
- **`ae3857c`** 索引期间 UI 冻结：r2d2 连接池仅 8 个，Rayon 并行任务耗尽连接，前端 IPC 命令 `get()` 阻塞 → `max_size: 8→32` + `connection_timeout: 10s`
- **`8f8980c`** 启动扫描 VACUUM 阻塞：VACUUM 持有 SQLite 独占锁，移到 watcher 之后执行 + 发 `scan-completed` 事件

### 🟠 功能修复
- **`63d3d06`** Details 按钮无响应：`get_index_errors` 命令未注册为 Tauri handler，前端 `invoke` 静默失败
- **`0c65e66`** 迁移数据路径错误：`migrateData` 返回消息字符串，前端误当路径存 → 改 `selected` + 加 catch 弹窗
- **`0ed36ae`** 迁移后自动重启：新增 `restart_app` Tauri 命令 + 确认对话框
- **`19c595a`** 设置页外部依赖面板：PaddleOCR/pdftoppm/LibreOffice 状态 + 一键复制安装命令
- **`6181000`** 7 个 TS 编译错误：泛型类型错误 + 未使用导入 + API 签名变更

### 📖 文档
- **`57dd72b`** README + 用户手册全面重写
- **CHANGELOG.md** 首次创建（27 个 commit 完整记录）

### 🔧 工作流
- **`0adfab5`** 自动变更日志：Git post-commit hook 首次尝试 → 改为 AI 手动编写详细条目
- **`12a678b`** 添加 `AGENTS.md` 项目规范：变更记录规则、代码规范、关键文件索引

---

## 2026-08-01（第五轮：Browse 页重写为表格视图）

### 🚀 新功能
- **`a2e0e16`** Browse 页全面重写：从文件系统目录树浏览改为数据库驱动的表格视图
  - 新增后端 `list_files_db` 命令：分页查询 `file_tracking` 表，支持状态筛选（全部/已索引/未索引/失败）、文件类型筛选、文件名模糊搜索、多字段排序（名称/路径/类型/大小/时间）
  - 前端表格列：文件名（ellipsis 截断）| 路径（ellipsis + title 完整路径）| 类型 | 状态（✓/✗/○ 图标）
  - 工具栏：状态筛选下拉 + 类型筛选 + 搜索框 + 排序选择
  - URL `useSearchParams` 同步所有筛选状态，刷新/分享不丢失
  - 分页控件（上/下页 + 页码跳转）
  - 点击行 → 右侧预览面板（复用 PreviewPanel）
  - 移除旧的目录树递归逻辑和相关 state

### 🟠 IndexStatus 卡片跳转
- 索引状态页 StatCard 支持跳转：Total Files → Browse，Indexed → `?filter=indexed`，Pending → `?filter=pending`。OCR'd 跳全部（暂无对应筛选），Errors 保留展开详情功能

---

## 2026-08-01（第六轮：扫描流程 + 数据一致性修复）

### 🔴 严重 Bug
- **`b1ba768`** list_files_db SQL 参数错位：`where_clause` 的 `?` 占位符与 `LIMIT ? OFFSET ?` 位置冲突导致查询失败，Browse 页无内容 → 改用 `params_from_iter` 正确绑定；`sort=name` 改用 `path` 排序（file_name 不是 DB 列）
- **`b1ba768`** 删除目录后残留数据：`remove_dir` 只删 `dir_config` 行，file_tracking 孤儿记录（统计虚高）、Tantivy 文档（仍可搜索）、content_index 引用全部残留 → 增加清理：先按 dir_id 从 Tantivy 删文档，再硬删 `file_tracking` 行，最后 `cleanup_orphan_content` 清理孤儿 content

### 🚀 新功能
- **`ede3cce`** 扫描两阶段进度报告：`ScanProgress` 增加 `phase` 字段（`"scan"`/`"index"`），`batch_index` 增加进度回调，Phase 2 串行写入时每处理一个文件上报已索引数；三个扫描函数 walk 阶段发 `phase:"scan"`、索引阶段发 `phase:"index"`，前端状态栏和索引状态页据此显示"正在扫描/正在索引"

### 📖 文档
- **`1b06f2c`** 添加完整 CHANGELOG.md
- **`03684db`** 修复 CHANGELOG 格式

### 🏗️ 索引目录命名重构
- **索引目录撞车**：data_dir 名为 "index" 时与硬编码索引子目录 `data_dir/index` 撞车，产生双重 `index/index`。新增共享常量 `INDEX_DIR_NAME = ".ls-index"`，替换全部硬编码 `join("index")`（`lib.rs`/`cli.rs`/`commands/config.rs`/`commands/backup.rs`）。启动时检测旧布局 `data_dir/index` 并重命名为 `.ls-index`（幂等）。`phase: "index"` 扫描标记与 `data.db` 路径逻辑不受影响

---

## 2026-08-01

### 🏗️ 索引重建改为原子替换
- **重建中断不丢旧索引**：`rebuild_index`（`commands/index.rs`）不再先 `remove_dir_all` 删旧索引，改为：① 建临时目录 `index.tmp-<uuid>`（`uuid::Uuid::new_v4().simple()`，同父目录下 `with_file_name`）→ ② 清空 `file_tracking`/`content_index`（保留原逻辑）→ ③ 在 tmp 目录 `IndexManager::open_or_create` 并 swap 内存 → ④ `reset_writer` → ⑤ 全量扫描（逻辑不变，写入 tmp 索引）→ ⑥ `indexer.commit()` 确保落盘 → ⑦ 原子替换：旧目录 rename 为 `index.old`，tmp rename 为 `index_dir`，成功则删 backup，失败则回滚还原旧索引。所有错误退出路径清理 tmp_dir 并复位 `is_scanning`/`is_rebuilding`/`cancel_scan`。搜索已被 `is_rebuilding` 守卫，重建期间读旧索引不受影响

### 🚀 重建期间搜索守卫
- **重建索引时搜索返回友好错误**：`AppState` 新增 `is_rebuilding: Arc<AtomicBool>` 标志（`state.rs`），`rebuild_index` 启动时置 true、所有退出路径（含 spawn_blocking 内提前 return 与正常结束）置 false（`commands/index.rs`）；`search` 命令开头检查该标志，重建期间直接返回 `"索引重建中，请稍后再试"`（`commands/search.rs`）。`lib.rs` / `tests/ipc_test.rs` 的 `AppState::new` 调用点传入新参数。未改动 rebuild 的目录删除/重建逻辑（R1-3b 单独处理）

### 🔒 安全加固（2 项）
- **Tauri CSP 启用**：`tauri.conf.json` 中 `"csp": null` → 完整 CSP 策略（`default-src 'self'` + 白名单 script/style/img/media/connect/frame/font/worker）。`connect-src` 额外加入 `http://ipc.localhost` 以兼容 Windows/Linux 的 IPC 通道（macOS 走 `ipc://`），防止 IPC 被 CSP 阻断。前端仅本地资源，无远程内容受影响
- **fs 插件权限收窄 + scope**：`capabilities/default.json` 删除 `fs:allow-mkdir` / `fs:allow-remove` / `fs:allow-rename`（前端未使用）；保留读权限与 `fs:allow-write`（SearchPage.tsx 的 CSV 导出 `writeTextFile` 依赖它，且 `save()` 对话框会自动将选中路径加入 fs scope，导出不受影响）；新增 `fs.scope` 白名单（`$APPDATA` / `$APPLOCALDATA` / `$DOCUMENT` / `$DESKTOP` / `$DOWNLOAD` 递归）

### 🔴 Bug 修复
- **lock_writer 并发丢索引**：`lock_writer`（`indexer.rs`）先释放 writer 锁再创建 `IndexWriter`，两个线程并发首次写入时各建一个 writer，后写者覆盖前者、丢文档。改为全程持锁创建（`index_manager` 是 RwLock 读锁、不依赖 writer 锁，不会死锁）
- **切换语言清空 data_dir**：`setLang`（`i18n/index.tsx`）调用 `updateConfig({ data_dir: '', language: l })` 把配置里的 data_dir 清空，切换语言即丢全部数据。改为只传 `{ language: l }`；同时在 `updateConfig`（`api/config.ts`）加防呆，拒绝空 `data_dir` 并抛错 `data_dir cannot be empty`，从源头杜绝此类覆盖
- **上次取消后下次扫描立即被取消**：`cancel_scan` 标志在扫描开始时未复位，上次点过取消后，`trigger_scan`/`rebuild_index` 的循环第一次 `load` 就为 true 直接 break → 两个 `spawn_blocking` 闭包开头先 `cancel_scan.store(false, Ordering::Release)` 复位；循环内 `load(Relaxed)` 改为 `Acquire`，`cancel_scan` 命令 `store(true, ...)` 改为 `Release`，形成 acquire-release 同步对（`commands/index.rs`）
- **restore_backup 直接覆盖活跃 data.db 损坏数据库**：WAL 模式下连接池仍持有 data.db，`fs::copy` 覆盖与 WAL 冲突可能导致损坏。改为 SQLite 在线备份 API（`rusqlite::backup::Backup::new(&src, &mut dst)` + `step(-1)`，`step_to` 在 0.32 已改名；Busy/Locked 重试 3 次）从备份的 data.db 恢复到活跃连接，不再直接覆盖文件。索引目录改为 rebuild_index 的 tmp→rename 原子替换 + 切换 IndexManager。恢复完成 emit `restore-completed` 后自动重启生效。`AppState` 新增 `is_restoring: Arc<AtomicBool>` 防重入（`state.rs`/`lib.rs`/`tests/ipc_test.rs`，顺带修复 ipc_test 缺参编译错误）；`Cargo.toml` 启用 rusqlite `backup` feature（`commands/backup.rs`）

---

## 2026-08-01

### 🏗️ 临时目录 RAII 工具 TempDir
- **临时目录并发冲突与泄漏**：4 处用 `ls_*_{pid}` 命名系统临时目录的代码，并发/多实例运行时共享同一路径互相覆盖，且提前 return 时遗留垃圾目录。新增 `scanner/helpers.rs` 的 `TempDir`（`{prefix}_{pid}_{uuid}` 唯一路径 + Drop 自动 `remove_dir_all`），替换 4 处：
  - `commands/files.rs` `download_files`：`ls_download_{pid}` → `TempDir::new("ls_download")`（zip 打包）
  - `commands/search.rs` `export_search_results`：`ls_export_{pid}.{format}` → `TempDir::new("ls_export")`（CSV/文本导出）
  - `extractor/office/mod.rs` `extract_via_libreoffice`：`ls_lo_{pid}` → `TempDir::new("ls_lo")`（guard 留在函数作用域，路径 clone 进线程，移除手动 `remove_dir_all`）
  - `extractor/pdf.rs` `ocr_pdf_via_pdftoppm`：`ls_pdf_ocr_{pid}` → `TempDir::new("ls_pdf_ocr")`（移除手动 `remove_dir_all`）
  - 新增 2 个单测：drop 后目录被删除、路径唯一（`cargo test --lib scanner::helpers` 通过）

## 2026-08-01

### 🏗️ Tantivy path 字段改为相对路径
- **索引内 path 存绝对路径、与 DB 不一致**：`batch_index` 和 `index_file` 用 `file_path.to_string_lossy()`（绝对路径）写入 Tantivy 的 `path` 字段，而 `file_tracking.path` 存相对路径，搜索结果路径与 Browse 页不一致。修复：
  - `batch_index`：`ExtractedData.file_path_str` 改用 `job.rel_path`（`BatchJob.rel_path` 已由 scanner 传入，DB 也用它做 upsert）
  - `index_file`：调用方未传 rel_path 时，从 `dir_config::get_dir` 取目录根 + `helpers::to_relative` 补算相对路径；读文件内容仍用绝对路径（`file_path` 参数），仅写索引用相对路径（`indexer.rs`）
  - 存量绝对路径：无需单独迁移——`startup_scan` 每次启动会用 rel_path 重写索引；若搜索结果路径仍显示绝对路径，在索引状态页重建索引一次


## 统计

| 类别 | 数量 |
|------|:---:|
| 🔴 Bug 修复 | 30+ |
| 🏗️ 架构/性能改进 | 20 |
| 🎨 UI/UX 修复 | 25+ |
| 🚀 新功能 | 8 |
| 📖 文档 | 5 |
| **总计 commits** | **35** |
| **变更文件数** | **70+** |
