import * as client from './client'

export interface FileDetail {
  id: string
  path: string
  file_name: string
  file_ext: string
  mtime: number
  file_size: number
  md5: string | null
  indexed: boolean
}

export interface DuplicateGroup {
  md5: string
  count: number
  paths: string[]
}

export interface SummaryResult {
  file_id: string
  summary: string
  cached: boolean
}

export async function summarizeFile(fileId: string): Promise<SummaryResult> {
  return client.invoke<SummaryResult>('summarize_file', { fileId })
}

export async function askDocuments(fileIds: string[], question: string): Promise<string> {
  return client.invoke<string>('ask_documents', { fileIds, question })
}

export interface AiCapabilities {
  embedding: boolean
  llm: boolean
}

export async function aiCapabilities(): Promise<AiCapabilities> {
  return client.invoke<AiCapabilities>('ai_capabilities')
}

export interface TopicCluster {
  topic: string
  files: string[]
}

export async function getTopicClusters(limit?: number): Promise<TopicCluster[]> {
  return client.invoke<TopicCluster[]>('ai_topic_clusters', { limit })
}

export interface EvidenceItem {
  file_id: string
  path: string
  snippet: string
  bm25_score?: number | null
  semantic_score?: number | null
  rrf_score?: number | null
  rewritten?: boolean
  rewritten_query?: string | null
  from_history?: boolean
}

export interface SmartSearchResponse {
  answer: string
  source_ids: string[]
  source_files: string[]
  evidence?: EvidenceItem[]
}

export interface ChatMessage {
  role: string
  content: string
}

export interface PerTurnEvidence {
  turn_index: number
  file_ids: string[]
  items?: EvidenceItem[]
  /** 本轮唯一追溯 ID（日志关联键） */
  trace_id?: string
  /** 本轮生成耗时（毫秒） */
  took_ms?: number
  /** 本轮使用的 LLM 模型 ID */
  llm_model?: string
  /** 本轮使用的 Embedding 模型 ID */
  embedding_model?: string
  /** 改写后的最终检索查询 */
  search_query?: string
  /** BM25 合并前命中数 */
  hits?: number
}

export interface PerTurnScope {
  turn_index: number
  /** 该轮发送时的完整检索范围快照（跨轮累计合并后），用于导出追溯 */
  scope: string[]
}

export interface ChatSession {
  id: string
  title: string
  created_at: number
  updated_at: number
  messages: ChatMessage[]
  source_ids: string[]
  source_files: string[]
  pending_query?: string | null
  pending_started_at?: number | null
  per_turn_evidence?: PerTurnEvidence[]
  per_turn_scopes?: PerTurnScope[]
  /** 会话级统一检索范围（跨轮累计的路径条目，父路径吞并子路径） */
  retrieval_scope?: string[]
  /** P2：严格模式——范围内无命中时拒绝回答 */
  strict_docs?: boolean
}

export async function cancelAiRequest(): Promise<void> {
  return client.invoke<void>('cancel_ai_request')
}

export interface ChatSessionMeta {
  id: string
  title: string
  updated_at: number
}

export async function smartSearch(query: string): Promise<SmartSearchResponse> {
  return client.invoke<SmartSearchResponse>('smart_search', { query })
}

export interface TurnScope {
  mention_files: string[]
  mention_dirs: string[]
  conditions: ScopeCondition[]
}

export interface ScopeCondition {
  kind: string
  value: string
  parsed?: string | null
}

export async function conversationAsk(messages: ChatMessage[], sourceIds: string[], scope?: TurnScope, sessionRetrievalScope?: string[], strictDocs?: boolean): Promise<string> {
  return client.invoke<string>('conversation_ask', { messages, sourceIds, scope: scope ?? {}, sessionRetrievalScope: sessionRetrievalScope ?? [], strictDocs: strictDocs ?? false })
}

// ── Streaming AI (Tauri events) ──
export interface AiDonePayload {
  session_id: string
  full_text: string
  took_ms: number
  cancelled: boolean
  source_ids: string[]
  source_files: string[]
  evidence?: EvidenceItem[]
  trace_id?: string
  search_query?: string
  hits?: number
  llm_model?: string
  embedding_model?: string
}

export async function smartSearchStream(query: string, sessionId: string): Promise<void> {
  return client.invoke<void>('smart_search_stream', { query, sessionId })
}

export async function conversationAskStream(messages: ChatMessage[], sourceIds: string[], sessionId: string, scope?: TurnScope, sessionRetrievalScope?: string[], strictDocs?: boolean): Promise<void> {
  return client.invoke<void>('conversation_ask_stream', { messages, sourceIds, sessionId, scope: scope ?? {}, sessionRetrievalScope: sessionRetrievalScope ?? [], strictDocs: strictDocs ?? false })
}

export async function searchFilePaths(prefix: string, limit?: number): Promise<string[]> {
  return client.invoke<string[]>('search_file_paths', { prefix, limit: limit ?? 20 })
}

export async function searchTreePrune(term: string): Promise<string[]> {
  return client.invoke<string[]>('search_tree_prune', { term })
}

/** Listen for streaming chunks/done of one session. Returns an unlisten fn. */
export async function listenAiStream(
  sessionId: string,
  onChunk: (delta: string) => void,
  onDone: (p: AiDonePayload) => void,
): Promise<() => void> {
  const unChunk = await client.listen<{ session_id: string; delta: string }>('ai-chunk', e => {
    if (e.session_id === sessionId) onChunk(e.delta)
  })
  const unDone = await client.listen<AiDonePayload>('ai-done', e => {
    if (e.session_id === sessionId) onDone(e)
  })
  return () => { unChunk(); unDone() }
}

export async function listChatSessions(): Promise<ChatSessionMeta[]> {
  return client.invoke<ChatSessionMeta[]>('list_chat_sessions')
}

export async function createChatSession(): Promise<string> {
  return client.invoke<string>('create_chat_session')
}

export async function deleteChatSession(id: string): Promise<void> {
  return client.invoke<void>('delete_chat_session', { id })
}

export async function loadChatSession(id: string): Promise<ChatSession | null> {
  return client.invoke<ChatSession | null>('load_chat_session', { id })
}

export async function saveChatSession(session: ChatSession): Promise<void> {
  return client.invoke<void>('save_chat_session', { session })
}

export async function exportChatSession(id: string): Promise<string> {
  return client.invoke<string>('export_chat_session', { id })
}

export async function exportChatSessionJson(id: string): Promise<string> {
  return client.invoke<string>('export_chat_session_json', { id })
}

export async function getFile(id: string): Promise<FileDetail> {
  return client.invoke<FileDetail>('get_file', { id })
}

export async function getDuplicates(): Promise<DuplicateGroup[]> {
  return client.invoke<DuplicateGroup[]>('get_duplicates')
}

export async function previewFile(id: string): Promise<string> {
  return client.invoke<string>('preview_file', { id })
}

export interface FilePreview {
  content: string | null
  image_path: string | null
  image_base64: string | null
  file_type: string  // 'image' | 'text' | 'pdf' | 'office' | 'unknown'
  char_count: number
  ocr_used: boolean
}

export async function getFilePreview(id: string): Promise<FilePreview> {
  return client.invoke<FilePreview>('get_file_preview', { id })
}

export async function revealInFolder(id: string): Promise<void> {
  return client.invoke('reveal_in_folder', { id })
}

export async function openFile(id: string): Promise<void> {
  return client.invoke('open_file', { id })
}

export interface FileItem {
  file_id: string
  file_name: string
  rel_path: string
  file_ext: string
  indexed: number  // 0=pending, 1=indexed, 2=failed
  error_msg: string | null
  file_size: number
  mtime: number
}

export interface FileListResponse {
  items: FileItem[]
  total: number
  page: number
  page_size: number
}

export type FilterType = 'all' | 'indexed' | 'pending' | 'failed'
export type SortKey = 'name' | 'size' | 'mtime' | 'ext'
export type SortOrder = 'asc' | 'desc'

export async function listFilesDb(params: {
  filter?: FilterType
  ext?: string
  search?: string
  sort?: SortKey
  order?: SortOrder
  page?: number
  pageSize?: number
}): Promise<FileListResponse> {
  return client.invoke<FileListResponse>('list_files_db', params)
}

export async function getBrowseFileTypes(): Promise<string[]> {
  return client.invoke<string[]>('get_browse_file_types')
}

export interface AiEvent {
  id: number
  session_id: string
  turn_number: number
  event_seq: number
  event_type: string
  payload: Record<string, unknown>
  created_at: number
}

export async function getAiEvents(sessionId: string): Promise<AiEvent[]> {
  return client.invoke<AiEvent[]>('get_ai_events', { sessionId })
}

export async function getTurnAiEvents(sessionId: string, turnNumber: number): Promise<AiEvent[]> {
  return client.invoke<AiEvent[]>('get_turn_ai_events', { sessionId, turnNumber })
}