#!/usr/bin/env python3
"""rfcheck detection benchmark: injected faults in real rigged GLBs.

    python3 bench/faults.py [--tag NAME] [clean.glb ...]

Clean inputs default to the CC0 characters motionforge's bench writes
(standardized and animated MPFB2 bodies, ~/Games/_blender/motionforge/bench/
results/*/*/{std,anim}.glb, DEF- named) and the raw corpus (Unreal names,
so R_JOINT_PREFIX is expected there and listed apart). Each clean file is
checked as is (false alarms = any contract code on it), then once per
injected fault (detected = the expected code appears). Stdlib only.
Writes bench/results/<tag>/{summary.json,scorecard.md} + bench/history/.
"""

import argparse
import datetime
import glob
import json
import os
import shutil
import struct
import subprocess
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
CLI = os.path.join(ROOT, "target", "release", "rfcheck")


def read(path):
    b = open(path, "rb").read()
    jl = struct.unpack("<I", b[12:16])[0]
    j = json.loads(b[20:20 + jl])
    o = 20 + jl
    binary = bytearray(b[o + 8:o + 8 + struct.unpack("<I", b[o:o + 4])[0]]) if len(b) > o else bytearray()
    return j, binary


def write(path, j, binary):
    js = json.dumps(j, separators=(",", ":")).encode()
    js += b" " * (-len(js) % 4)
    binary = bytes(binary) + b"\0" * (-len(binary) % 4)
    if j.get("buffers"):
        j["buffers"][0]["byteLength"] = len(binary)
        js = json.dumps(j, separators=(",", ":")).encode()
        js += b" " * (-len(js) % 4)
    total = 12 + 8 + len(js) + (8 + len(binary) if binary else 0)
    with open(path, "wb") as fh:
        fh.write(struct.pack("<III", 0x46546C67, 2, total))
        fh.write(struct.pack("<II", len(js), 0x4E4F534A) + js)
        if binary:
            fh.write(struct.pack("<II", len(binary), 0x004E4942) + binary)


SIZE = {5126: 4, 5123: 2, 5121: 1, 5125: 4}
FMT = {5126: "f", 5123: "H", 5121: "B", 5125: "I"}
NC = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4, "MAT4": 16}


def elem(j, acc, i, k):
    a = j["accessors"][acc]
    bv = j["bufferViews"][a["bufferView"]]
    sz = SIZE[a["componentType"]]
    stride = bv.get("byteStride", 0) or sz * NC[a["type"]]
    return bv.get("byteOffset", 0) + a.get("byteOffset", 0) + i * stride + k * sz, FMT[a["componentType"]]


def skinned_prim(j):
    for m in j["meshes"]:
        for p in m["primitives"]:
            if "WEIGHTS_0" in p["attributes"]:
                return p
    return None


def f_unnormalized(j, b):
    p = skinned_prim(j)
    acc = p["attributes"]["WEIGHTS_0"]
    if j["accessors"][acc]["componentType"] != 5126:
        return False
    for i in range(min(50, j["accessors"][acc]["count"])):
        off, f = elem(j, acc, i, 0)
        struct.pack_into("<f", b, off, struct.unpack_from("<f", b, off)[0] * 0.6)
    return True


def f_joint_range(j, b):
    p = skinned_prim(j)
    acc = p["attributes"]["JOINTS_0"]
    off, f = elem(j, acc, 3, 0)
    struct.pack_into("<" + f, b, off, 250)
    return True


def f_over_influence(j, b):
    """A second influence set with 4 other joints (weights still sum to 1):
    8 distinct joints per vertex. On the largest skinned primitive (the
    body: its verts blend several joints)."""
    p = max((p for m in j["meshes"] for p in m["primitives"] if "WEIGHTS_0" in p["attributes"]),
            key=lambda p: j["accessors"][p["attributes"]["POSITION"]]["count"])
    ja, wa = p["attributes"]["JOINTS_0"], p["attributes"]["WEIGHTS_0"]
    if j["accessors"][wa]["componentType"] != 5126:
        return False
    n = j["accessors"][ja]["count"]
    nj = len(j["skins"][0]["joints"])
    if nj < 8:
        return False
    jn, wn = bytearray(), bytearray()
    for i in range(n):
        js = []
        for k in range(4):
            off, f = elem(j, ja, i, k)
            js.append(struct.unpack_from("<" + f, b, off)[0])
        for k in range(4):
            jn += struct.pack("<H", (js[k] + 4) % nj)
            off, _ = elem(j, wa, i, k)
            w = struct.unpack_from("<f", b, off)[0]
            struct.pack_into("<f", b, off, w * 0.75)
            wn += struct.pack("<f", w * 0.25)
    def add(data, ctype):
        b.extend(b"\0" * (-len(b) % 4))
        j["bufferViews"].append({"buffer": 0, "byteOffset": len(b), "byteLength": len(data)})
        b.extend(data)
        j["accessors"].append({"bufferView": len(j["bufferViews"]) - 1, "componentType": ctype, "count": n,
                               "type": "VEC4"})
        return len(j["accessors"]) - 1
    p["attributes"]["JOINTS_1"] = add(jn, 5123)
    p["attributes"]["WEIGHTS_1"] = add(wn, 5126)
    j["buffers"][0]["byteLength"] = len(b)
    return True


def f_prefix(j, b):
    joints = j["skins"][0]["joints"]
    n = j["nodes"][joints[len(joints) // 2]]
    n["name"] = n.get("name", "bone").replace("DEF-", "") + "_x"
    return True


def f_joint_index(j, b):
    j["skins"][0]["joints"].append(len(j["nodes"]) + 5)
    return True


def f_no_skin(j, b):
    j.pop("skins", None)
    for n in j["nodes"]:
        n.pop("skin", None)
    return True


def f_cycle(j, b):
    joints = j["skins"][0]["joints"]
    root, leaf = joints[0], joints[-1]
    j["nodes"][leaf].setdefault("children", []).append(root)
    return True


def f_anim_nan(j, b):
    if not j.get("animations"):
        return False
    s = j["animations"][0]["samplers"][0]
    off, f = elem(j, s["output"], 1, 0)
    if f != "f":
        return False
    struct.pack_into("<f", b, off, float("nan"))
    return True


def f_clip_name(j, b):
    a = j.get("animations", [])
    if len(a) < 2:
        return False
    a[1]["name"] = a[0].get("name", "clip")
    return True


def f_bounds(j, b):
    p = skinned_prim(j)
    j["accessors"][p["attributes"]["POSITION"]]["count"] *= 50
    return True


def f_fan(j, b):
    skinned_prim(j)["mode"] = 6
    return True


FAULTS = [("W_UNNORMALIZED", f_unnormalized), ("X_JOINT_RANGE", f_joint_range), ("W_OVER_INFLUENCE", f_over_influence),
          ("R_JOINT_PREFIX", f_prefix), ("R_JOINT_INDEX", f_joint_index), ("R_NO_SKIN", f_no_skin),
          ("B_NODE_CYCLE", f_cycle), ("X_ANIM_NAN", f_anim_nan), ("B_CLIP_NAME", f_clip_name),
          ("X_ACCESSOR_BOUNDS", f_bounds), ("B_PRIM_MODE", f_fan)]


def check(path):
    t0 = time.time()
    p = subprocess.run([CLI, "--json", path], capture_output=True, text=True)
    dt = time.time() - t0
    codes = []
    try:
        r = json.loads(p.stdout)
        items = r if isinstance(r, list) else r.get("files", [r])
        for it in items:
            for f in it.get("diags", it.get("findings", [])):
                codes.append(f.get("code"))
    except Exception:
        codes = [line.split()[0] for line in p.stdout.splitlines() if line and line.split()[0][:2] in
                 ("D_", "R_", "W_", "X_", "B_", "L_", "P_")]
    return p.returncode, sorted(set(c for c in codes if c)), dt


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("files", nargs="*")
    ap.add_argument("--tag", default=datetime.date.today().isoformat())
    a = ap.parse_args()
    if not os.path.exists(CLI):
        subprocess.run(["cargo", "build", "--release", "-j2"], cwd=ROOT, check=True)
    files = a.files or sorted(glob.glob(os.path.expanduser("~/Games/_blender/motionforge/bench/results/baseline/*/*.glb")))
    out = os.path.join(HERE, "results", a.tag)
    os.makedirs(out, exist_ok=True)
    rows, times = [], []
    clean_alarms = []
    for f in files:
        rc, codes, dt = check(f)
        times.append(dt)
        bad = [c for c in codes if not c.startswith("P_")]
        if bad:
            clean_alarms.append((f, bad))
        for code, fn in FAULTS:
            j, b = read(f)
            if not fn(j, b):
                continue
            m = os.path.join(out, "mut.glb")
            write(m, j, b)
            rc2, codes2, dt2 = check(m)
            times.append(dt2)
            rows.append({"file": os.path.relpath(f, os.path.expanduser("~")), "fault": code, "detected": code in codes2,
                         "exit": rc2, "codes": codes2})
    det = {}
    for r in rows:
        d = det.setdefault(r["fault"], [0, 0])
        d[0] += r["detected"]
        d[1] += 1
    summary = {"tag": a.tag, "date": datetime.datetime.now().isoformat(timespec="seconds"), "clean_files": len(files),
               "clean_false_alarms": clean_alarms, "detection": det, "rows": rows,
               "median_ms": sorted(times)[len(times) // 2] * 1000 if times else None}
    json.dump(summary, open(os.path.join(out, "summary.json"), "w"), indent=1)
    L = [f"# rfcheck fault-injection bench `{a.tag}`", "",
         f"{len(files)} clean CC0 rigged GLBs (MPFB2 bodies standardized + animated by motionforge); "
         f"false alarms on them: {len(clean_alarms)}; median check {summary['median_ms']:.0f} ms.", "",
         "| injected fault | detected |", "|---|---|"]
    for k, (d, n) in det.items():
        L.append(f"| {k} | {d}/{n} |")
    open(os.path.join(out, "scorecard.md"), "w").write("\n".join(L) + "\n")
    hist = os.path.join(HERE, "history")
    os.makedirs(hist, exist_ok=True)
    shutil.copy(os.path.join(out, "summary.json"), os.path.join(hist, f"{a.tag}.json"))
    shutil.copy(os.path.join(out, "scorecard.md"), os.path.join(hist, f"{a.tag}.md"))
    print("\n".join(L))


if __name__ == "__main__":
    main()
