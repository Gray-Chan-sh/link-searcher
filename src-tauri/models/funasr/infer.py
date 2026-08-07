#!/usr/bin/env python3
"""FunASR-Nano + CAM++ speaker diarization via ModelScope (no token needed)."""
import sys, os
os.environ['FUNASR_DISABLE_UPDATE'] = '1'

def main(wav_path):
    from funasr import AutoModel
    model = AutoModel(
        model="FunAudioLLM/Fun-ASR-Nano-2512",
        vad_model="fsmn-vad",
        vad_kwargs={"max_single_segment_time": 30000},
        spk_model="cam++",
        device="cpu",
        disable_pbar=True,
        disable_log=True,
    )
    res = model.generate(
        input=[wav_path], cache={}, batch_size=1,
        language="中文",
        hotwords=os.environ.get('FUNASR_HOTWORDS', ''),
    )
    if not res: return
    info = res[0].get("sentence_info", [])
    if info:
        for sent in info:
            spk = sent.get('spk', '?')
            text = sent.get('text', sent.get('sentence', ''))
            print(f"[Speaker {spk}] {text}")
    else:
        print(res[0].get("text", ""))

if __name__ == '__main__':
    if len(sys.argv) > 1: main(sys.argv[1])
