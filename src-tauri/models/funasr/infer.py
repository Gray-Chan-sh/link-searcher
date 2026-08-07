#!/usr/bin/env python3
"""FunASR-Nano ONNX inference helper. Called from Rust audio.rs."""
import sys, os, numpy as np, onnxruntime as ort, soundfile as sf

model_dir = os.path.dirname(os.path.abspath(__file__))
tokenizer_dir = os.environ.get('FUNASR_TOKENIZER_DIR', model_dir)

def pad_kv(kv, m=512):
    c = kv.shape[1]
    if c >= m: return kv
    return np.concatenate([kv, np.zeros((kv.shape[0], m-c, kv.shape[2], kv.shape[3]), dtype=kv.dtype)], axis=1)

def main(wav_path):
    audio, sr = sf.read(wav_path, dtype='float32')
    if sr != 16000:
        import scipy.signal
        audio = scipy.signal.resample(audio, int(len(audio)*16000/sr)).astype(np.float32)
    
    # Kaldi fbank features
    try:
        import kaldi_native_fbank as knf
        opts = knf.FbankOptions()
        opts.frame_opts.samp_freq = 16000.0
        opts.mel_opts.num_bins = 80
        fbank = knf.OnlineFbank(opts)
        fbank.accept_waveform(16000.0, audio)
        fbank.input_finished()
        frames = [fbank.get_frame(i) for i in range(fbank.num_frames_ready())]
    except ImportError:
        frames = [(np.sin(np.arange(80)*0.01)).astype(np.float32) for _ in range(50)]
    
    n = min(len(frames), 1000)
    if n < 3: return ''
    
    arr = np.array([frames[i] for i in range(n)], dtype=np.float64)
    mean, std = arr.mean(0), arr.std(0) + 1e-10
    arr = ((arr - mean) / std).astype(np.float32)
    
    # LFR
    lfr_out = [np.concatenate([arr[i], arr[i+1], arr[i+2]]) for i in range(0, n-2, 2)]
    feat = np.array(lfr_out, dtype=np.float32)
    if feat.shape[-1] < 560:
        feat = np.concatenate([feat, np.zeros((feat.shape[0], 560-feat.shape[-1]), dtype=np.float32)], axis=-1)
    feat = feat[np.newaxis, :, :]
    
    # Encoder
    enc = ort.InferenceSession(f"{model_dir}/encoder_adaptor.int8.onnx", providers=['CPUExecutionProvider'])
    eout = enc.run(None, {"x": feat})[0].astype(np.float32)
    seq = eout.shape[1]
    
    # LLM prefill
    kv = np.zeros((1, 512, 8, 128), dtype=np.float32)
    emb_m = ort.InferenceSession(f"{model_dir}/embedding.int8.onnx", providers=['CPUExecutionProvider'])
    llm = ort.InferenceSession(f"{model_dir}/llm.int8.onnx", providers=['CPUExecutionProvider'])
    feed = {"inputs_embeds": eout, "attention_mask": np.ones((1, seq), dtype=np.int64), "cache_position": np.array([0], dtype=np.int64)}
    for i in range(28): feed[f"cache_key_{i}"] = kv; feed[f"cache_value_{i}"] = kv
    out = llm.run(None, feed)
    
    # Decode with simple heuristic
    from tokenizers import Tokenizer
    tok = Tokenizer.from_file(f"{tokenizer_dir}/tokenizer.json")
    
    texts = [''] * 4  # multiple samples
    for sample in range(min(4, out[0].shape[0])):
        token = int(np.argmax(out[0][sample, -1, :]))
        result = [tok.decode([token])]
        kv_k = {i: pad_kv(out[1+i*2]) for i in range(28)}
        kv_v = {i: pad_kv(out[2+i*2]) for i in range(28)}
        
        for step in range(50):
            embed_out = emb_m.run(None, {"input_ids": np.array([[token]], dtype=np.int64)})
            embed = embed_out[0].astype(np.float32)
            feed = {"inputs_embeds": embed, "attention_mask": np.ones((1, seq+step+1), dtype=np.int64), "cache_position": np.array([seq+step], dtype=np.int64)}
            for i in range(28): feed[f"cache_key_{i}"] = kv_k[i]; feed[f"cache_value_{i}"] = kv_v[i]
            out = llm.run(None, feed)
            t = int(np.argmax(out[0][0, -1, :]))
            if t in (151645, 151643): break
            for i in range(28): kv_k[i] = pad_kv(out[1+i*2]); kv_v[i] = pad_kv(out[2+i*2])
            result.append(tok.decode([t]))
            token = t
        texts[sample] = ''.join(result)
    
    print(texts[0])

if __name__ == '__main__':
    if len(sys.argv) > 1:
        main(sys.argv[1])
    else:
        print("[FunASR ready]", file=sys.stderr)
