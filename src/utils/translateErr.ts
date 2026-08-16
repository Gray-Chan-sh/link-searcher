// 后端 IPC 错误消息 → 前端 i18n 映射。
// 后端返回中文消息（调试友好），此处按消息原文查对应 i18n 键；
// 未命中（未知错误/无条目）时回退原文，保证 zh 用户不回归。
export function translateErr(msg: string, t: (key: string, params?: Record<string, string | number>) => string): string {
  const map: Record<string, string> = {
    '问题不能为空': 'err_empty_question',
    '该文件没有可摘要的文本内容': 'err_no_summary_text',
    '所选文件没有可用的文本内容': 'err_no_text_content',
    '未找到相关文档内容': 'err_no_docs_found',
    '请求已取消': 'err_request_cancelled',
    'AI 服务未配置，请在设置页填写 API Base URL': 'err_ai_not_configured',
    'AI 服务未配置，请在设置页配置 API Base URL': 'err_ai_not_configured',
    '当前使用的 LLM 模型不可用，请在设置页重新选择': 'err_llm_model_unavailable',
    '当前使用的 LLM 网关已被删除，请在设置页重新选择': 'err_llm_gateway_deleted',
    'AI 请求失败（检查网关配置或网络）': 'err_ai_request_failed',
    'AI 请求失败（检查 API 配置或网络）': 'err_ai_request_failed',
    '数据库繁忙，迁移未完成，请重试': 'err_db_busy_migrate',
    '数据库繁忙，备份未完成，请重试': 'err_db_busy_backup',
    '数据库繁忙，恢复未完成，请重试': 'err_db_busy_restore',
    'a scan is already in progress': 'err_scan_in_progress',
    '索引重建中，请稍后再试': 'err_index_rebuilding',
    '该 Provider 正在使用中，请先切换当前模型': 'err_provider_in_use',
    'base_url 不能为空': 'err_base_url_empty',
    '当前数据目录不存在': 'err_data_dir_missing',
    '目标目录已包含 data.db，请选择空目录或新目录': 'err_target_has_db',
    '目标目录已包含索引文件夹，请选择空目录或新目录': 'err_target_has_index',
    '目标目录不能是当前数据目录或其子目录': 'err_target_overlaps_data',
    'AI 未配置（embedding_api_base 为空），无法生成语义向量': 'err_embedding_not_configured',
    '正在恢复中，请稍候': 'err_restoring',
    '此目录与数据目录存在交叠，不允许监控': 'err_dir_overlaps_data',
    '此目录位于数据目录内，不允许监控': 'err_dir_inside_data',
    '此目录包含数据目录，不允许监控': 'err_dir_contains_data',
    'FunASR 模型下载已在后台进行中': 'err_funasr_downloading',
    '未在与当前范围匹配的文档中找到依据': 'err_strict_no_evidence',
    'file not found': 'err_file_not_found',
    '对话不能为空': 'err_empty_conversation',
  }
  const key = map[msg]
  if (key) {
    const translated = t(key)
    // 未命中键时 t() 返回 key 本身；此时回退原文而不是显示键名
    return translated === key ? msg : translated
  }
  return msg
}