//! Cross-end chat-session coordination (`docs/LARK_BOT.md` §5.5).
//!
//! The three ends — desktop UI (Tauri IPC), Web (axum) and Feishu (bot over
//! the Web API) — all reach the same core through `commands::*`, so this is
//! the only place where their behaviour can be unified.
//!
//! Stage 1 (implemented here): AI generation is arbitrated **per chat
//! session** instead of by a process-global flag, so cancelling one end's
//! answer no longer aborts another end's.
//!
//! Stage 2 (`ChatSession.origin` isolation) and stage 3 (Feishu
//! `chat_id ↔ session_id` mapping) land in this module as well; see §5.5 for
//! the staged plan and the risks that gate them.

pub mod in_flight;
pub mod lark;
pub mod lark_gate;
