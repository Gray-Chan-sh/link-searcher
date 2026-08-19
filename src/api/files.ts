import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

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
  return invoke<SummaryResult>('summarize_file', { fileId })
}

export async function askDocuments(fileIds: string[], question: string): Promise<string> {
  return invoke<string>('ask_documents', { fileIds, question })
}

export interface AiCapabilities {
  embedding: boolean
  llm: boolean
}

export async function aiCapabilities(): Promise<AiCapabilities> {
  return invoke<AiCapabilities>('ai_capabilities')
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
  return invoke<void>('cancel_ai_request')
}

export interface ChatSessionMeta {
  id: string
  title: string
  updated_at: number
}

export async function smartSearch(query: string): Promise<SmartSearchResponse> {
  return invoke<SmartSearchResponse>('smart_search', { query })
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
  return invoke<string>('conversation_ask', { messages, sourceIds, scope: scope ?? {}, sessionRetrievalScope: sessionRetrievalScope ?? [], strictDocs: strictDocs ?? false })
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
}

export async function smartSearchStream(query: string, sessionId: string): Promise<void> {
  return invoke<void>('smart_search_stream', { query, sessionId })
}

export async function conversationAskStream(messages: ChatMessage[], sourceIds: string[], sessionId: string, scope?: TurnScope, sessionRetrievalScope?: string[], strictDocs?: boolean): Promise<void> {
  return invoke<void>('conversation_ask_stream', { messages, sourceIds, sessionId, scope: scope ?? {}, sessionRetrievalScope: sessionRetrievalScope ?? [], strictDocs: strictDocs ?? false })
}

export async function searchFilePaths(prefix: string, limit?: number): Promise<string[]> {
  return invoke<string[]>('search_file_paths', { prefix, limit: limit ?? 20 })
}

/** Listen for streaming chunks/done of one session. Returns an unlisten fn. */
export async function listenAiStream(
  sessionId: string,
  onChunk: (delta: string) => void,
  onDone: (p: AiDonePayload) => void,
): Promise<() => void> {
  const unChunk = await listen<{ session_id: string; delta: string }>('ai-chunk', e => {
    if (e.payload.session_id === sessionId) onChunk(e.payload.delta)
  })
  const unDone = await listen<AiDonePayload>('ai-done', e => {
    if (e.payload.session_id === sessionId) onDone(e.payload)
  })
  return () => { unChunk(); unDone() }
}

export async function listChatSessions(): Promise<ChatSessionMeta[]> {
  return invoke<ChatSessionMeta[]>('list_chat_sessions')
}

export async function createChatSession(): Promise<string> {
  return invoke<string>('create_chat_session')
}

export async function deleteChatSession(id: string): Promise<void> {
  return invoke<void>('delete_chat_session', { id })
}

export async function loadChatSession(id: string): Promise<ChatSession | null> {
  return invoke<ChatSession | null>('load_chat_session', { id })
}

export async function saveChatSession(session: ChatSession): Promise<void> {
  return invoke<void>('save_chat_session', { session })
}

export async function exportChatSession(id: string): Promise<string> {
  return invoke<string>('export_chat_session', { id })
}

export async function exportChatSessionJson(id: string): Promise<string> {
  return invoke<string>('export_chat_session_json', { id })
}

export async function getFile(id: string): Promise<FileDetail> {
  return invoke<FileDetail>('get_file', { id })
}

export async function getDuplicates(): Promise<DuplicateGroup[]> {
  return invoke<DuplicateGroup[]>('get_duplicates')
}

export async function previewFile(id: string): Promise<string> {
  return invoke<string>('preview_file', { id })
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
  return invoke<FilePreview>('get_file_preview', { id })
}

export async function revealInFolder(id: string): Promise<void> {
  return invoke('reveal_in_folder', { id })
}

export async function openFile(id: string): Promise<void> {
  return invoke('open_file', { id })
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
  return invoke<FileListResponse>('list_files_db', params)
}

export async function getBrowseFileTypes(): Promise<string[]> {
  return invoke<string[]>('get_browse_file_types')
}