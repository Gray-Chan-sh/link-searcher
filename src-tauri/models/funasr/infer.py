#!/usr/bin/env python3
"""FunASR inference via official funasr.AutoModel."""
import sys, os
os.environ['FUNASR_DISABLE_UPDATE'] = '1'

def main(wav_path):
    from funasr import AutoModel
    model = AutoModel(
        model="FunASRNano",
        hub="hf",
        device="cpu",
        disable_pbar=True,
        disable_log=True,
    )
    res = model.generate(input=[wav_path], language="中文", itn=True, batch_size_s=60)
    text = res[0]["text"] if res else ""
    print(text.strip())

if __name__ == '__main__':
    if len(sys.argv) > 1:
        main(sys.argv[1])
