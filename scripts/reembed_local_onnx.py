#!/usr/bin/env python3
"""一次性迁移脚本：用本地 bge-small-zh-v1.5 (ONNX/CPU) 全量重嵌 doc + chunk。

背景：原向量是 bge-large-zh (1024维) 由 Rust tract CPU 生成，慢（~1/s）且
模型切换后维度不一致。现切到 bge-small-zh-v1.5 (512维, 本地 ONNX)：
- 实测 48 条/s（比 large 快 ~10x），12.4 万 chunk ≈ 43 分钟
- chunk 级检索质量实测够用（违约金条款查询 top10 全命中正样本）
- pooling 与 Rust local_embed 一致（取 [CLS] + L2 归一化），查询/存储可比

用法：python3 scripts/reembed_local_onnx.py <data.db>
幂等：执行前清空 doc_embeddings / chunk_embeddings 防新旧混合。先备份。
"""
import argparse
import sqlite3
import struct
import time

import numpy as np
import onnxruntime as ort
from tokenizers import Tokenizer

DOC_MAX_CHARS = 2000  # 与 Rust truncate_for_embed 一致
BATCH = 128
QUERY_PREFIX = "为这个句子生成表示以用于检索相关文章："


def load_engine(model_dir):
    onnx = f"{model_dir}/model.onnx"
    tok_path = f"{model_dir}/tokenizer.json"
    sess = ort.InferenceSession(onnx, providers=["CPUExecutionProvider"])
    tok = Tokenizer.from_file(tok_path)
    tok.enable_padding(pad_id=0, pad_token="[PAD]")
    tok.enable_truncation(max_length=512)
    return sess, tok


def truncate_for_embed(s: str) -> str:
    chars = list(s)
    return s if len(chars) <= DOC_MAX_CHARS else "".join(chars[:DOC_MAX_CHARS])


def embed_texts(sess, tok, texts):
    """CLS pooling + L2 归一化（与 Rust local_embed.rs 一致）。"""
    out_vecs = []
    for i in range(0, len(texts), BATCH):
        batch = texts[i : i + BATCH]
        enc = tok.encode_batch(batch)
        ids = np.array([e.ids for e in enc], dtype=np.int64)
        mask = np.array([e.attention_mask for e in enc], dtype=np.int64)
        types = np.array([e.type_ids for e in enc], dtype=np.int64)
        out = sess.run(None, {"input_ids": ids, "attention_mask": mask, "token_type_ids": types})
        hidden = out[0]  # (B, seq, 512)
        cls = hidden[:, 0, :]
        norm = np.linalg.norm(cls, axis=1, keepdims=True)
        out_vecs.extend((cls / np.maximum(norm, 1e-9)).tolist())
    return out_vecs


def _to_blob(v):
    return b"".join(struct.pack("<f", x) for x in v)


def reembed_docs(conn, sess, tok, dry=False):
    rows = conn.execute(
        "SELECT ft.id, ci.text_content FROM file_tracking ft "
        "JOIN content_index ci ON ft.md5 = ci.md5 "
        "WHERE ft.status='active' AND ft.md5 IS NOT NULL"
    ).fetchall()
    total = len(rows)
    print(f"[doc] 需重嵌 {total} 个文件", flush=True)
    if dry or total == 0:
        return
    conn.execute("DELETE FROM doc_embeddings")
    conn.commit()
    conn.execute("PRAGMA synchronous=OFF")
    t0 = time.time()
    try:
        for i in range(0, total, BATCH):
            batch_rows = rows[i : i + BATCH]
            texts = [truncate_for_embed(r[1]) for r in batch_rows]
            vecs = embed_texts(sess, tok, texts)
            conn.executemany(
                "INSERT OR REPLACE INTO doc_embeddings (file_id, dim, vector, updated_at) "
                "VALUES (?, 512, ?, ?)",
                [(r[0], _to_blob(v), int(time.time())) for r, v in zip(batch_rows, vecs)],
            )
            done = i + len(batch_rows)
            if done % (BATCH * 10) == 0:
                conn.commit()
                print(f"[doc] {done}/{total} ({done/(time.time()-t0):.0f}/s)", flush=True)
        conn.commit()
        print(f"[doc] 完成 {total} ({total/(time.time()-t0):.0f}/s)", flush=True)
    finally:
        conn.execute("PRAGMA synchronous=NORMAL")


def reembed_chunks(conn, sess, tok, dry=False):
    rows = conn.execute("SELECT md5, chunk_index, text FROM doc_chunks").fetchall()
    total = len(rows)
    print(f"[chunk] 需重嵌 {total} 个块", flush=True)
    if dry or total == 0:
        return
    conn.execute("DELETE FROM chunk_embeddings")
    conn.commit()
    conn.execute("PRAGMA synchronous=OFF")
    t0 = time.time()
    try:
        for i in range(0, total, BATCH):
            batch_rows = rows[i : i + BATCH]
            texts = [r[2] for r in batch_rows]
            vecs = embed_texts(sess, tok, texts)
            conn.executemany(
                "INSERT OR REPLACE INTO chunk_embeddings "
                "(md5, chunk_index, dim, vector, updated_at) VALUES (?, ?, 512, ?, ?)",
                [(r[0], r[1], _to_blob(v), int(time.time())) for r, v in zip(batch_rows, vecs)],
            )
            done = i + len(batch_rows)
            if done % (BATCH * 10) == 0:
                conn.commit()
                print(f"[chunk] {done}/{total} ({done/(time.time()-t0):.0f}/s)", flush=True)
        conn.commit()
        print(f"[chunk] 完成 {total} ({total/(time.time()-t0):.0f}/s)", flush=True)
    finally:
        conn.execute("PRAGMA synchronous=NORMAL")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("db")
    ap.add_argument("--model-dir", default="")
    ap.add_argument("--skip-doc", action="store_true")
    ap.add_argument("--skip-chunk", action="store_true")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    model_dir = args.model_dir or "/Volumes/Data/index/models/bge-small-zh-v1.5"
    t0 = time.time()
    sess, tok = load_engine(model_dir)
    # 预热
    embed_texts(sess, tok, ["预热文本内容"])
    print(f"模型就绪 {time.time()-t0:.1f}s: {model_dir}", flush=True)

    conn = sqlite3.connect(args.db)
    conn.execute("PRAGMA journal_mode=WAL")
    if not args.skip_doc:
        reembed_docs(conn, sess, tok, args.dry_run)
    if not args.skip_chunk:
        reembed_chunks(conn, sess, tok, args.dry_run)
    conn.close()
    print(f"完成，总耗时 {time.time()-t0:.0f}s", flush=True)


if __name__ == "__main__":
    main()
