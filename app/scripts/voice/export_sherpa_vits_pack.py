"""把 coqui/jaywalnut310 结构的中文 VITS（如 zomehwh/vits-uma-genshin-honkai 的 804 角色模型）导出为使驾「sherpa-vits」语音包。

产物（--out 目录）：
  <id>.zip           主包：model.onnx + lexicon.txt + tokens.txt + dict/ + *.fst + voice.json（engine = "sherpa-vits"）
  <id>-<角色>.zip    可选角色小包：只有 voice.json（engine = "preset"，base = "pack:<id>"，换说话人 / 变调）
在 设置 → 全局助手 → 语音 → 角色语音包 → 导入。运行时复用语音运行环境里的 sherpa_onnx，不需要 torch。

依赖（开发机，CPU 即可）：pip install torch --index-url https://download.pytorch.org/whl/cpu ; pip install onnx
用法：
  python export_sherpa_vits_pack.py --src <模型仓库目录：models.py 等 + model/config.json> --ckpt <G_*.pth> \
      --frontend <sherpa-onnx vits-zh-hf-* 解压目录：lexicon.txt tokens.txt dict/ *.fst> \
      --id genshin-vits-zh --name "原神/崩坏 中文 VITS" --sid 127 --out dist [--voice "心海=127" --voice "绫华=87:-0.5"]
注意：symbols 必须与 frontend 的 tokens.txt 一致（zh_ja_mixture_cleaners 的 52 个符号），脚本会校验。
"""
import argparse
import json
import os
import shutil
import sys
import types
import zipfile


def export_onnx(src, cfg, ckpt, out):
    import torch
    import onnx

    sys.modules.setdefault("monotonic_align", types.ModuleType("monotonic_align"))  # 推理用不到（训练对齐用）
    sys.path.insert(0, src)
    import models

    d = cfg["data"]
    net = models.SynthesizerTrn(len(cfg["symbols"]), d["filter_length"] // 2 + 1, cfg["train"]["segment_size"] // d["hop_length"],
                                n_speakers=d["n_speakers"], **cfg["model"])
    sd = torch.load(ckpt, map_location="cpu", weights_only=False)
    sd = sd.get("model", sd)
    own = net.state_dict()
    missing = [k for k in own if k not in sd]
    if missing:
        raise SystemExit(f"checkpoint 缺少 {len(missing)} 个参数，例如 {missing[:3]}")
    net.load_state_dict({k: sd[k] for k in own})
    net.eval()
    for m in net.dec.ups:
        torch.nn.utils.remove_weight_norm(m)
    for r in net.dec.resblocks:
        r.remove_weight_norm()

    class W(torch.nn.Module):
        def __init__(self, n):
            super().__init__()
            self.n = n

        def forward(self, x, x_length, noise_scale, length_scale, noise_scale_w, sid):
            return self.n.infer(x=x, x_lengths=x_length, sid=sid, noise_scale=noise_scale, length_scale=length_scale, noise_scale_w=noise_scale_w)[0]

    x = torch.randint(1, len(cfg["symbols"]), (1, 50))
    args = (x, torch.tensor([50]), torch.tensor([0.667]), torch.tensor([1.0]), torch.tensor([0.8]), torch.tensor([0]))
    with torch.no_grad():
        torch.onnx.export(W(net), args, out, opset_version=13,
                          input_names=["x", "x_length", "noise_scale", "length_scale", "noise_scale_w", "sid"], output_names=["y"],
                          dynamic_axes={"x": {0: "N", 1: "L"}, "x_length": {0: "N"}, "y": {0: "N", 2: "L"}}, dynamo=False)
    mo = onnx.load(out)
    meta = {"model_type": "vits", "comment": "shidrive-sherpa-vits", "language": "Chinese", "add_blank": "1" if d.get("add_blank") else "0",
            "n_speakers": str(d["n_speakers"]), "sample_rate": str(d["sampling_rate"]), "punctuation": ", . ! ? - ~ …", "jieba": "1"}
    for k, v in meta.items():
        e = mo.metadata_props.add()
        e.key, e.value = k, v
    onnx.save(mo, out)


def zip_dir(d, zp, root):
    with zipfile.ZipFile(zp, "w", zipfile.ZIP_DEFLATED) as z:
        for base, _, files in os.walk(d):
            for f in sorted(files):
                full = os.path.join(base, f)
                z.write(full, os.path.join(root, os.path.relpath(full, d)).replace("\\", "/"))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", required=True)
    ap.add_argument("--ckpt", required=True)
    ap.add_argument("--config", default="")
    ap.add_argument("--frontend", required=True)
    ap.add_argument("--id", required=True)
    ap.add_argument("--name", required=True)
    ap.add_argument("--sid", type=int, default=0)
    ap.add_argument("--pitch", type=float, default=0.0)
    ap.add_argument("--speed", type=float, default=1.0)
    ap.add_argument("--noise-scale", type=float, default=0.5)
    ap.add_argument("--noise-scale-w", type=float, default=0.6)
    ap.add_argument("--skin", action="append", default=[])
    ap.add_argument("--voice", action="append", default=[], help="角色小包：名称=sid[:变调半音]")
    ap.add_argument("--source", default="")
    ap.add_argument("--credits", default="")
    ap.add_argument("--license", default="")
    ap.add_argument("--onnx", default="", help="已导出的 onnx（跳过导出）")
    ap.add_argument("--out", default="dist")
    a = ap.parse_args()

    cfg = json.load(open(a.config or os.path.join(a.src, "model", "config.json"), encoding="utf-8"))
    tokens = [l.rstrip("\n").rsplit(" ", 1)[0] for l in open(os.path.join(a.frontend, "tokens.txt"), encoding="utf-8")]
    if tokens != list(cfg["symbols"]):
        raise SystemExit("模型 symbols 与 frontend tokens.txt 不一致，不能直接复用该 lexicon")
    speakers = cfg.get("speakers") or []
    stage = os.path.join(a.out, a.id)
    shutil.rmtree(stage, ignore_errors=True)
    os.makedirs(stage)
    model = os.path.join(stage, "model.onnx")
    if a.onnx:
        shutil.copy(a.onnx, model)
    else:
        export_onnx(a.src, cfg, a.ckpt, model)
    for f in ["lexicon.txt", "tokens.txt", "phone.fst", "date.fst", "number.fst", "new_heteronym.fst"]:
        if os.path.exists(os.path.join(a.frontend, f)):
            shutil.copy(os.path.join(a.frontend, f), stage)
    shutil.copytree(os.path.join(a.frontend, "dict"), os.path.join(stage, "dict"))
    rules = [f for f in ["phone.fst", "date.fst", "number.fst", "new_heteronym.fst"] if os.path.exists(os.path.join(stage, f))]
    common = {"lang": "zh", "skins": a.skin, "license": a.license, "source": a.source, "credits": a.credits}
    sp = lambda i: speakers[i] if i < len(speakers) else str(i)
    main_meta = {"id": a.id, "name": a.name, "engine": "sherpa-vits", **common, "characters": [sp(a.sid)],
                 "model": "model.onnx", "lexicon": "lexicon.txt", "tokens": "tokens.txt", "dict": "dict", "rule_fsts": rules,
                 "sample_rate": cfg["data"]["sampling_rate"], "sid": a.sid, "pitch": a.pitch, "speed": a.speed,
                 "noise_scale": a.noise_scale, "noise_scale_w": a.noise_scale_w, "num_speakers": cfg["data"]["n_speakers"]}
    json.dump(main_meta, open(os.path.join(stage, "voice.json"), "w", encoding="utf-8"), ensure_ascii=False, indent=2)
    zip_dir(stage, os.path.join(a.out, a.id + ".zip"), a.id)
    for v in a.voice:
        name, rest = v.split("=", 1)
        sid, _, pitch = rest.partition(":")
        vid = f"{a.id}-{sid}"
        meta = {"id": vid, "name": f"{a.name} · {name}", "engine": "preset", "base": "pack:" + a.id, **common, "skins": [],
                "characters": [sp(int(sid))], "sid": int(sid), "pitch": float(pitch or 0), "speed": a.speed}
        with zipfile.ZipFile(os.path.join(a.out, f"{vid}.zip"), "w", zipfile.ZIP_DEFLATED) as z:
            z.writestr("voice.json", json.dumps(meta, ensure_ascii=False, indent=2))
    print("ok", os.path.join(a.out, a.id + ".zip"))


if __name__ == "__main__":
    main()
