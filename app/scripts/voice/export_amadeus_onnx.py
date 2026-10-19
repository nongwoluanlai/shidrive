"""把 Hugging Face 上的 mio/amadeus（ESPnet2 VITS，JSUT 微调，CC-BY-4.0）导出为使驾「角色语音包」。

产物：<out>/amadeus-kurisu-voice.zip（model.onnx + tokens.json + voice.json + README.md），
在 设置 → 全局助手 → 语音 → 角色语音包 → 导入 即可使用。只在开发机上运行一次，使驾运行时不需要 torch。

依赖（开发机，CPU 即可）：
  pip install torch --index-url https://download.pytorch.org/whl/cpu
  pip install espnet --no-deps
  pip install onnx onnxruntime pyyaml typeguard humanfriendly configargparse scipy librosa torch_complex
用法：
  python export_amadeus_onnx.py --config config.yaml --model model.pth --out dist
  （config.yaml / model.pth 来自 https://huggingface.co/mio/amadeus/tree/main/exp/tts_amadeus_vits_finetune_from_jsut_32_sentence ，
   model.pth 即 train.total_count.ave_10best.pth）
"""
import argparse
import json
import os
import zipfile

import numpy as np
import onnx
import torch
import yaml
from onnx import TensorProto, helper, numpy_helper


def build_generator(cfg_path, pth_path):
    from espnet2.gan_tts.vits.generator import VITSGenerator

    cfg = yaml.safe_load(open(cfg_path, encoding="utf-8"))
    g = VITSGenerator(**cfg["tts_conf"]["generator_params"])
    sd = torch.load(pth_path, map_location="cpu")
    prefix = "tts.generator."
    g.load_state_dict({k[len(prefix):]: v for k, v in sd.items() if k.startswith(prefix)}, strict=True)
    g.eval()
    if hasattr(g, "remove_weight_norm"):
        g.remove_weight_norm()
    return g, cfg["token_list"]


class Infer(torch.nn.Module):
    def __init__(self, g):
        super().__init__()
        self.g = g

    def forward(self, text, text_lengths, noise_scale, noise_scale_dur, alpha):
        wav, _, _ = self.g.inference(text, text_lengths, noise_scale=noise_scale, noise_scale_dur=noise_scale_dur, alpha=alpha)
        return wav


def weights_to_fp16(src, dst):
    """权重以 fp16 存储、计算仍为 fp32（插入 Cast），体积减半，音质无可闻差异。"""
    m = onnx.load(src)
    g = m.graph
    casts, inits = [], []
    for init in list(g.initializer):
        if init.data_type == TensorProto.FLOAT and int(np.prod(init.dims)) >= 256:
            a = numpy_helper.to_array(init)
            g.initializer.remove(init)
            inits.append(numpy_helper.from_array(a.astype(np.float16), init.name + "__h"))
            casts.append(helper.make_node("Cast", [init.name + "__h"], [init.name], to=TensorProto.FLOAT, name="castw_%d" % len(casts)))
    g.initializer.extend(inits)
    for i, n in enumerate(casts):
        g.node.insert(i, n)
    onnx.save(m, dst)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--config", required=True)
    ap.add_argument("--model", required=True)
    ap.add_argument("--out", default="dist")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    g, tokens = build_generator(a.config, a.model)
    x = torch.randint(2, len(tokens) - 1, (1, 40))
    args = (x, torch.tensor([40]), torch.tensor(0.667), torch.tensor(0.8), torch.tensor(1.0))
    raw = os.path.join(a.out, "model.fp32.onnx")
    torch.onnx.export(
        Infer(g), args, raw,
        input_names=["text", "text_lengths", "noise_scale", "noise_scale_dur", "alpha"], output_names=["wav"],
        dynamic_axes={"text": {1: "T"}, "wav": {1: "L"}}, opset_version=17, dynamo=False,
    )
    model = os.path.join(a.out, "model.onnx")
    weights_to_fp16(raw, model)
    os.remove(raw)
    meta = {
        "id": "amadeus-kurisu",
        "name": "牧濑红莉栖 · Amadeus（日语）",
        "lang": "ja",
        "engine": "espnet-vits-onnx",
        "model": "model.onnx",
        "tokens": "tokens.json",
        "g2p": "pyopenjtalk_accent_with_pause",
        "sample_rate": 22050,
        "noise_scale": 0.667,
        "noise_scale_dur": 0.8,
        "skins": ["kurisu-dynamic", "steins-gate"],
        "characters": ["牧濑红莉栖", "牧瀬紅莉栖", "Makise Kurisu"],
        "license": "CC-BY-4.0",
        "source": "https://huggingface.co/mio/amadeus",
        "credits": "mio/amadeus (ESPnet2 VITS, fine-tuned from JSUT). Exported to ONNX for ShiDrive.",
    }
    readme = (
        "# 牧濑红莉栖 · Amadeus 日语语音包\n\n"
        "- 来源：https://huggingface.co/mio/amadeus （ESPnet2 VITS，基于 JSUT 微调），许可 CC-BY-4.0，署名 mio。\n"
        "- 本包仅做格式转换（PyTorch → ONNX，权重 fp16 存储），未改动模型。\n"
        "- 文本前端：pyopenjtalk（pyopenjtalk_accent_with_pause），输入日语文本。\n"
        "- 角色与声音版权归原作品权利人所有，请仅用于个人非商业用途。\n"
    )
    zpath = os.path.join(a.out, "amadeus-kurisu-voice.zip")
    with zipfile.ZipFile(zpath, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        z.write(model, "model.onnx")
        z.writestr("tokens.json", json.dumps(tokens, ensure_ascii=False))
        z.writestr("voice.json", json.dumps(meta, ensure_ascii=False, indent=2))
        z.writestr("README.md", readme)
    os.remove(model)
    print("written", zpath, os.path.getsize(zpath))


if __name__ == "__main__":
    main()
