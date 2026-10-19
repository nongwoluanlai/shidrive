# ShiDrive 语音服务：由使驾以「语音运行环境」里的 Python 启动，常驻内存（模型只加载一次）。
# 协议：stdin / stdout 每行一个 JSON。请求 {"id", "op", ...}；响应 {"id", "ok", ...} 或 {"id", "ok": false, "error"}。
# 父进程退出（stdin 关闭）后自动结束。不联网、不写文件；日志写 stderr。
import base64
import json
import os
import re
import sys
import threading
import traceback

_OUT = sys.stdout
sys.stdout = sys.stderr  # 第三方库的 print 不得污染协议通道

_lock = threading.Lock()
_cache = {}


def log(*a):
    print("[voice]", *a, file=sys.stderr, flush=True)


def threads():
    try:
        n = int(os.environ.get("SHIDRIVE_VOICE_THREADS", "0"))
    except ValueError:
        n = 0
    if n <= 0:
        n = max(1, min(4, (os.cpu_count() or 2) // 2))
    return n


def pcm16_b64(samples, rate):
    import numpy as np

    a = np.asarray(samples, dtype=np.float32)
    peak = float(np.abs(a).max()) if a.size else 0.0
    if peak > 1.0:
        a = a / peak
    b = (np.clip(a, -1.0, 1.0) * 32767.0).astype("<i2").tobytes()
    return {"rate": int(rate), "pcm": base64.b64encode(b).decode("ascii"), "seconds": round(a.size / float(rate), 3)}


def p(d, *names):
    return os.path.join(d, *names)


# ---------------------------------------------------------------- ASR (SenseVoice)

def asr_model(d):
    key = ("asr", d)
    if key not in _cache:
        import sherpa_onnx

        model = p(d, "model.int8.onnx")
        if not os.path.exists(model):
            model = p(d, "model.onnx")
        _cache[key] = sherpa_onnx.OfflineRecognizer.from_sense_voice(
            model=model, tokens=p(d, "tokens.txt"), use_itn=True, num_threads=threads(), language="auto"
        )
        log("asr loaded", d)
    return _cache[key]


def op_asr(req):
    import numpy as np

    rec = asr_model(req["dir"])
    raw = base64.b64decode(req["pcm"])
    a = np.frombuffer(raw, dtype="<i2").astype(np.float32) / 32768.0
    s = rec.create_stream()
    s.accept_waveform(int(req.get("rate", 16000)), a)
    rec.decode_stream(s)
    text = re.sub(r"<\|[^|]*\|>", "", s.result.text or "").strip()
    return {"text": text, "lang": (getattr(s.result, "lang", "") or "").strip("<|>")}


# ---------------------------------------------------------------- TTS (sherpa-onnx：Melo / Kokoro)

def tts_sherpa(kind, d):
    key = ("tts", kind, d)
    if key not in _cache:
        import sherpa_onnx

        fsts = ",".join(x for x in [p(d, n) for n in ("date.fst", "number.fst", "phone.fst", "date-zh.fst", "number-zh.fst", "phone-zh.fst")] if os.path.exists(x))
        if kind == "kokoro":
            model = p(d, "model.int8.onnx") if os.path.exists(p(d, "model.int8.onnx")) else p(d, "model.onnx")
            lex = ",".join(x for x in [p(d, "lexicon-us-en.txt"), p(d, "lexicon-zh.txt")] if os.path.exists(x))
            mc = sherpa_onnx.OfflineTtsModelConfig(
                kokoro=sherpa_onnx.OfflineTtsKokoroModelConfig(
                    model=model, voices=p(d, "voices.bin"), tokens=p(d, "tokens.txt"),
                    data_dir=p(d, "espeak-ng-data"), dict_dir=p(d, "dict"), lexicon=lex,
                ),
                num_threads=threads(),
            )
        else:
            mc = sherpa_onnx.OfflineTtsModelConfig(
                vits=sherpa_onnx.OfflineTtsVitsModelConfig(
                    model=p(d, "model.onnx"), lexicon=p(d, "lexicon.txt"), tokens=p(d, "tokens.txt"), dict_dir=p(d, "dict"),
                ),
                num_threads=threads(),
            )
        cfg = sherpa_onnx.OfflineTtsConfig(model=mc, rule_fsts=fsts, max_num_sentences=1)
        _cache[key] = sherpa_onnx.OfflineTts(cfg)
        log("tts loaded", kind, d)
    return _cache[key]


# ---------------------------------------------------------------- TTS (角色语音包：ESPnet VITS → ONNX，日语 pyopenjtalk)

_JA_KEEP = re.compile(r"[^\u3040-\u30ff\u3400-\u9fff\uf900-\ufaff\uff10-\uff19\uff21-\uff3a\uff41-\uff5a0-9A-Za-z、。！？!?,.，…ー～〜「」『』（）()\s]")


def g2p_accent_with_pause(text):
    import pyopenjtalk

    try:
        labels = pyopenjtalk.extract_fullcontext(text)
    except Exception:  # 个别读音修正需要额外词典时退回原版 OpenJTalk
        labels = pyopenjtalk.extract_fullcontext(text, use_vanilla=True)
    phones = []
    for lab in labels:
        if lab.split("-")[1].split("+")[0] == "pau":
            phones.append("pau")
            continue
        m = re.findall(r"\-(.*?)\+.*?\/A:([0-9\-]+).*?\/F:.*?_([0-9]+)", lab)
        if len(m) == 1:
            phones += [m[0][0], m[0][2], m[0][1]]
    return phones


def pack(d):
    key = ("pack", d)
    if key not in _cache:
        import onnxruntime as ort

        meta = json.load(open(p(d, "voice.json"), encoding="utf-8"))
        tok = meta.get("tokens", "tokens.json")
        if tok.endswith(".json"):
            tokens = json.load(open(p(d, tok), encoding="utf-8"))
        else:
            tokens = [l.rstrip("\n").split(" ")[0] for l in open(p(d, tok), encoding="utf-8")]
        so = ort.SessionOptions()
        so.intra_op_num_threads = threads()
        sess = ort.InferenceSession(p(d, meta.get("model", "model.onnx")), sess_options=so, providers=["CPUExecutionProvider"])
        _cache[key] = (meta, {t: i for i, t in enumerate(tokens)}, sess)
        log("voice pack loaded", d)
    return _cache[key]


def tts_pack(d, text, speed):
    import numpy as np

    meta, idx, sess = pack(d)
    if meta.get("engine") != "espnet-vits-onnx":
        raise RuntimeError("不支持的语音包引擎：%s" % meta.get("engine"))
    try:
        import jaconv

        text = jaconv.normalize(text)
    except Exception:
        pass
    text = _JA_KEEP.sub("", text).strip()
    if not text:
        return pcm16_b64([], meta.get("sample_rate", 22050))
    unk = idx.get("<unk>", 1)
    ids = [idx.get(ph, unk) for ph in g2p_accent_with_pause(text)]
    if not ids:
        return pcm16_b64([], meta.get("sample_rate", 22050))
    x = np.array([ids], dtype=np.int64)
    feeds = {
        "text": x,
        "text_lengths": np.array([x.shape[1]], dtype=np.int64),
        "noise_scale": np.array(float(meta.get("noise_scale", 0.667)), dtype=np.float32),
        "noise_scale_dur": np.array(float(meta.get("noise_scale_dur", 0.8)), dtype=np.float32),
        "alpha": np.array(1.0 / max(0.5, min(2.0, float(speed or 1.0))), dtype=np.float32),
    }
    wav = sess.run(None, feeds)[0].reshape(-1)
    return pcm16_b64(wav, meta.get("sample_rate", 22050))


def op_tts(req):
    engine = req.get("engine", "")
    d = req["dir"]
    text = (req.get("text") or "").strip()
    speed = float(req.get("speed") or 1.0)
    if engine == "pack":
        return tts_pack(d, text, speed)
    if engine not in ("melo", "kokoro"):
        raise RuntimeError("未知的语音引擎：%s" % engine)
    tts = tts_sherpa(engine, d)
    if not text:
        return pcm16_b64([], tts.sample_rate)
    sid = int(req.get("sid") or 0)
    if sid < 0 or sid >= max(1, tts.num_speakers):
        sid = 0
    audio = tts.generate(text, sid=sid, speed=speed)
    return pcm16_b64(audio.samples, audio.sample_rate)


def op_ping(_req):
    out = {"python": sys.version.split()[0]}
    for mod in ("numpy", "onnxruntime", "sherpa_onnx", "pyopenjtalk"):
        try:
            m = __import__(mod)
            out[mod] = getattr(m, "__version__", "ok")
        except Exception as e:  # 报告缺失的组件
            out[mod] = "ERROR: %s" % e
    return out


def op_unload(_req):
    _cache.clear()
    return {}


OPS = {"ping": op_ping, "asr": op_asr, "tts": op_tts, "unload": op_unload}


def reply(obj):
    _OUT.write(json.dumps(obj, ensure_ascii=False) + "\n")
    _OUT.flush()


def main():
    log("ready", sys.version.split()[0], "threads", threads())
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        rid = None
        try:
            req = json.loads(line)
            rid = req.get("id")
            fn = OPS.get(req.get("op"))
            if fn is None:
                raise RuntimeError("unknown op")
            with _lock:
                res = fn(req)
            res["id"] = rid
            res["ok"] = True
            reply(res)
        except Exception as e:
            log(traceback.format_exc())
            reply({"id": rid, "ok": False, "error": "%s: %s" % (type(e).__name__, e)})


if __name__ == "__main__":
    main()
