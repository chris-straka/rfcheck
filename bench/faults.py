#!/usr/bin/env python3
"""rfcheck detection benchmark and gate: injected faults in clean GLBs.

    python3 bench/faults.py [--check] [--tag NAME] [clean.glb ...]

Clean set combines:
- built-in procedural clean fixtures (rigged humanoid, rigged CC0-style quadruped
  animal, and decimated character-style closed mesh with clean UVs & 2 clips) so
  `--check` runs deterministically anywhere without external files, PLUS
- any real clean GLBs passed on the command line or found in optional clean sets.

Each clean file is checked as is (false alarms = any non-P_* diagnostic or warning),
then once per injected fault (detected = the expected code appears in diags or warns).
Reference-compared faults (M_FOLD:ref, M_SILHOUETTE_DRIFT:nicked_chin) run with
`--reference <clean.glb>`.

With `--check`, exits 1 on any missed fault, zero-sample fault, or clean false alarm.
Stdlib only. Writes bench/results/<tag>/{summary.json,scorecard.md} + bench/history/.
"""

import argparse
import datetime
import glob
import json
import math
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
CLI = os.path.join(os.environ.get("CARGO_TARGET_DIR") or os.path.join(ROOT, "target"), "release", "rfcheck")


def read(path):
    b = open(path, "rb").read()
    jl = struct.unpack("<I", b[12:16])[0]
    j = json.loads(b[20 : 20 + jl])
    o = 20 + jl
    binary = bytearray(b[o + 8 : o + 8 + struct.unpack("<I", b[o : o + 4])[0]]) if len(b) > o else bytearray()
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


def append_accessor(j, b, raw_bytes, ctype, atype, count):
    b.extend(b"\0" * (-len(b) % 4))
    off = len(b)
    b.extend(raw_bytes)
    j["bufferViews"].append({"buffer": 0, "byteOffset": off, "byteLength": len(raw_bytes)})
    j["accessors"].append({
        "bufferView": len(j["bufferViews"]) - 1,
        "componentType": ctype,
        "count": count,
        "type": atype,
    })
    j["buffers"][0]["byteLength"] = len(b)
    return len(j["accessors"]) - 1


def build_clean_rigged_glb(path, kind="humanoid"):
    """Build a clean, watertight, outward-wound rigged GLB (1.75m humanoid, quadruped
    animal, or decimated character-style mesh) with DEF- joints, normalized 4-bone
    weights, clean UVs, outward smooth normals, and 2 valid animation clips."""
    pos, nrm, uvs, idx = [], [], [], []
    segs = 16
    rings = 16
    if kind == "animal":
        # Quadruped torso ellipsoid (wider along Z, lower along Y)
        def profile(t):
            return max(0.05, 0.22 * math.sin(math.pi * t)), 0.15 + 0.70 * t
        rx_scale, rz_scale = 0.95, 1.45
        y0, y1 = 0.15, 0.85
    elif kind == "tripo_decimated":
        # Humanoid with smooth tapered neck & rounded head, 1.75m tall
        segs, rings = 12, 24
        def profile(t):
            base = math.sin(math.pi * max(0.05, min(0.95, t)))
            neck = 1.0 - 0.35 * math.exp(-((t - 0.79) / 0.07) ** 2)
            return max(0.055, 0.17 * base * neck), t * 1.75
        rx_scale, rz_scale = 1.0, 0.88
        y0, y1 = 0.0, 1.75
    else:
        # Clean humanoid 1.75m tall (glTF +Y up, facing +Z)
        segs, rings = 12, 24
        def profile(t):
            base = math.sin(math.pi * max(0.05, min(0.95, t)))
            neck = 1.0 - 0.38 * math.exp(-((t - 0.79) / 0.07) ** 2)
            return max(0.055, 0.18 * base * neck), t * 1.75
        rx_scale, rz_scale = 1.0, 0.90
        y0, y1 = 0.0, 1.75

    # Bottom cap center (separate pole vertices per triangle would avoid pole UV fan stretch,
    # or map caps into dedicated circular charts in [0.05..0.25] and body cylinder in [0.30..0.95])
    pos.append((0.0, y0, 0.0))
    uvs.append((0.15, 0.15))
    for r in range(1, rings):
        t = r / float(rings)
        rad, y = profile(t)
        for s in range(segs):
            ang = 2.0 * math.pi * s / float(segs)
            x = rad * rx_scale * math.cos(ang)
            z = rad * rz_scale * math.sin(ang)
            pos.append((x, y, z))
            uvs.append((0.30 + 0.65 * (s / float(segs)), 0.05 + 0.90 * t))
    top_i = len(pos)
    pos.append((0.0, y1, 0.0))
    uvs.append((0.15, 0.85))

    # Separate cap ring vertices with clean disk UVs so polar caps have zero UV stretch!
    bot_ring_uv = []
    for s in range(segs):
        vi = 1 + s
        ang = 2.0 * math.pi * s / float(segs)
        bot_ring_uv.append(len(pos))
        pos.append(pos[vi])
        uvs.append((0.15 + 0.08 * math.cos(ang), 0.15 + 0.08 * math.sin(ang)))
    top_ring_uv = []
    last_row = 1 + (rings - 2) * segs
    for s in range(segs):
        vi = last_row + s
        ang = 2.0 * math.pi * s / float(segs)
        top_ring_uv.append(len(pos))
        pos.append(pos[vi])
        uvs.append((0.15 + 0.08 * math.cos(ang), 0.85 + 0.08 * math.sin(ang)))

    # Bottom cap (outward normal = -Y)
    for s in range(segs):
        a = bot_ring_uv[s]
        b = bot_ring_uv[(s + 1) % segs]
        idx.extend([0, a, b])
    # Side quads (outward normal pointing radially outward)
    for r in range(rings - 2):
        row0 = 1 + r * segs
        row1 = 1 + (r + 1) * segs
        for s in range(segs):
            s1 = (s + 1) % segs
            a, b, c, d = row0 + s, row0 + s1, row1 + s1, row1 + s
            idx.extend([a, c, b, a, d, c])
    # Top cap (outward normal = +Y)
    for s in range(segs):
        a = top_ring_uv[s]
        b = top_ring_uv[(s + 1) % segs]
        idx.extend([a, top_i, b])

    # Recompute exact area-weighted outward smooth vertex normals by welded position.
    n_by_pos = {}
    def pk(p):
        return (round(p[0] * 10000), round(p[1] * 10000), round(p[2] * 10000))
    for k in range(0, len(idx), 3):
        ia, ib, ic = idx[k], idx[k + 1], idx[k + 2]
        pa, pb, pc = pos[ia], pos[ib], pos[ic]
        ab = (pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2])
        ac = (pc[0] - pa[0], pc[1] - pa[1], pc[2] - pa[2])
        fn = (ab[1] * ac[2] - ab[2] * ac[1], ab[2] * ac[0] - ab[0] * ac[2], ab[0] * ac[1] - ab[1] * ac[0])
        for p in (pa, pb, pc):
            key = pk(p)
            acc = n_by_pos.setdefault(key, [0.0, 0.0, 0.0])
            acc[0] += fn[0]
            acc[1] += fn[1]
            acc[2] += fn[2]
    for p in pos:
        v = n_by_pos[pk(p)]
        l = math.sqrt(v[0] * v[0] + v[1] * v[1] + v[2] * v[2]) or 1.0
        nrm.append((v[0] / l, v[1] / l, v[2] / l))

    # Fix cylinder seam triangles by duplicating the s=0 column at u=0.95
    seam_map = {}
    for r in range(rings - 1):
        vi = 1 + r * segs
        seam_map[vi] = len(pos)
        pos.append(pos[vi])
        nrm.append(nrm[vi])
        uvs.append((0.95, uvs[vi][1]))
    for k in range(0, len(idx), 3):
        tri = [idx[k], idx[k + 1], idx[k + 2]]
        us = [uvs[v][0] for v in tri]
        if max(us) - min(us) > 0.45:
            for m in range(3):
                if tri[m] in seam_map:
                    idx[k + m] = seam_map[tri[m]]

    # 8 DEF- joints so W_OVER_INFLUENCE (needs >= 8 joints) and all rig checks apply.
    n_joints = 8
    nv = len(pos)
    pos_b = b"".join(struct.pack("<fff", *p) for p in pos)
    nrm_b = b"".join(struct.pack("<fff", *n) for n in nrm)
    uv_b = b"".join(struct.pack("<ff", *u) for u in uvs)
    jnt_b = b"".join(struct.pack("<HHHH", 0, 1, 2, 3) for _ in range(nv))
    wgt_b = b"".join(struct.pack("<ffff", 0.5, 0.25, 0.15, 0.10) for _ in range(nv))
    idx_b = b"".join(struct.pack("<H", i) for i in idx)
    # 2 keyframes for animations
    t_b = struct.pack("<ff", 0.0, 1.0)
    tr_b = struct.pack("<ffffff", 0.0, 0.0, 0.0, 0.0, 0.05, 0.0)
    # Inverse bind matrices (8 identity MAT4)
    ident = struct.pack("<16f", 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1)
    ibm_b = ident * n_joints

    bin_buf = bytearray()
    bvs, accs = [], []

    def add(raw, ctype, atype, count, extra=None):
        bin_buf.extend(b"\0" * (-len(bin_buf) % 4))
        off = len(bin_buf)
        bin_buf.extend(raw)
        bvs.append({"buffer": 0, "byteOffset": off, "byteLength": len(raw)})
        a = {"bufferView": len(bvs) - 1, "componentType": ctype, "count": count, "type": atype}
        if extra:
            a.update(extra)
        accs.append(a)
        return len(accs) - 1

    pmin = [min(p[k] for p in pos) for k in range(3)]
    pmax = [max(p[k] for p in pos) for k in range(3)]
    a_pos = add(pos_b, 5126, "VEC3", nv, {"min": pmin, "max": pmax})
    a_nrm = add(nrm_b, 5126, "VEC3", nv)
    a_uv = add(uv_b, 5126, "VEC2", nv)
    a_jnt = add(jnt_b, 5123, "VEC4", nv)
    a_wgt = add(wgt_b, 5126, "VEC4", nv)
    a_idx = add(idx_b, 5123, "SCALAR", len(idx))
    a_t = add(t_b, 5126, "SCALAR", 2, {"min": [0.0], "max": [1.0]})
    a_tr = add(tr_b, 5126, "VEC3", 2)
    a_ibm = add(ibm_b, 5126, "MAT4", n_joints)

    nodes = [{"name": f"Clean_{kind}", "mesh": 0, "skin": 0}]
    joint_nodes = []
    for ji in range(n_joints):
        ni = len(nodes)
        joint_nodes.append(ni)
        node = {"name": f"DEF-joint_{ji}"}
        if ji + 1 < n_joints:
            node["children"] = [ni + 1]
        nodes.append(node)

    j = {
        "asset": {"version": "2.0"},
        "scenes": [{"nodes": [0, joint_nodes[0]]}],
        "scene": 0,
        "nodes": nodes,
        "skins": [{"joints": joint_nodes, "inverseBindMatrices": a_ibm}],
        "meshes": [{
            "name": f"Mesh_{kind}",
            "primitives": [{
                "attributes": {
                    "POSITION": a_pos,
                    "NORMAL": a_nrm,
                    "TEXCOORD_0": a_uv,
                    "JOINTS_0": a_jnt,
                    "WEIGHTS_0": a_wgt,
                },
                "indices": a_idx,
            }],
        }],
        "animations": [
            {
                "name": "idle",
                "samplers": [{"input": a_t, "output": a_tr}],
                "channels": [{"sampler": 0, "target": {"node": joint_nodes[0], "path": "translation"}}],
            },
            {
                "name": "walk",
                "samplers": [{"input": a_t, "output": a_tr}],
                "channels": [{"sampler": 0, "target": {"node": joint_nodes[1], "path": "translation"}}],
            },
        ],
        "buffers": [{"byteLength": len(bin_buf)}],
        "bufferViews": bvs,
        "accessors": accs,
    }
    write(path, j, bin_buf)


def skinned_prim(j):
    for m in j["meshes"]:
        for p in m["primitives"]:
            if "WEIGHTS_0" in p["attributes"]:
                return p
    return None


def largest_indexed_prim(j):
    cands = [
        p for m in j.get("meshes", []) for p in m.get("primitives", [])
        if p.get("mode", 4) == 4 and "indices" in p and "POSITION" in p.get("attributes", {})
    ]
    if not cands:
        return None
    return max(cands, key=lambda pr: j["accessors"][pr["attributes"]["POSITION"]]["count"])


def f_unnormalized(j, b):
    p = skinned_prim(j)
    if not p:
        return False
    acc = p["attributes"]["WEIGHTS_0"]
    if j["accessors"][acc]["componentType"] != 5126:
        return False
    for i in range(min(50, j["accessors"][acc]["count"])):
        off, _ = elem(j, acc, i, 0)
        struct.pack_into("<f", b, off, struct.unpack_from("<f", b, off)[0] * 0.6)
    return True


def f_joint_range(j, b):
    p = skinned_prim(j)
    if not p:
        return False
    acc = p["attributes"]["JOINTS_0"]
    off, f = elem(j, acc, 3, 0)
    struct.pack_into("<" + f, b, off, 250)
    return True


def f_over_influence(j, b):
    cands = [p for m in j["meshes"] for p in m["primitives"] if "WEIGHTS_0" in p["attributes"]]
    if not cands:
        return False
    p = max(cands, key=lambda p: j["accessors"][p["attributes"]["POSITION"]]["count"])
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
    p["attributes"]["JOINTS_1"] = append_accessor(j, b, jn, 5123, "VEC4", n)
    p["attributes"]["WEIGHTS_1"] = append_accessor(j, b, wn, 5126, "VEC4", n)
    return True


def f_prefix(j, b):
    if not j.get("skins"):
        return False
    joints = j["skins"][0]["joints"]
    n = j["nodes"][joints[len(joints) // 2]]
    n["name"] = n.get("name", "bone").replace("DEF-", "") + "_x"
    return True


def f_joint_index(j, b):
    if not j.get("skins"):
        return False
    j["skins"][0]["joints"].append(len(j["nodes"]) + 5)
    return True


def f_no_skin(j, b):
    if not j.get("skins"):
        return False
    j.pop("skins", None)
    for n in j["nodes"]:
        n.pop("skin", None)
    return True


def f_cycle(j, b):
    if not j.get("skins"):
        return False
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
    p = skinned_prim(j) or largest_indexed_prim(j)
    if not p:
        return False
    j["accessors"][p["attributes"]["POSITION"]]["count"] *= 50
    return True


def f_fan(j, b):
    p = skinned_prim(j) or largest_indexed_prim(j)
    if not p:
        return False
    p["mode"] = 6
    return True


def f_flipped_normal(j, b):
    cands = [
        p for m in j.get("meshes", []) for p in m.get("primitives", [])
        if p.get("mode", 4) == 4 and "POSITION" in p.get("attributes", {}) and "NORMAL" in p.get("attributes", {})
    ]
    if not cands:
        return False
    p = max(cands, key=lambda pr: j["accessors"][pr["attributes"]["POSITION"]]["count"])
    if "indices" in p:
        acc = p["indices"]
        n_tris = j["accessors"][acc]["count"] // 3
        if n_tris < 3:
            return False
        flip_n = max(3, min(n_tris, (n_tris * 5 + 999) // 1000))
        for t in range(flip_n):
            off1, f1 = elem(j, acc, 3 * t + 1, 0)
            off2, f2 = elem(j, acc, 3 * t + 2, 0)
            v1 = struct.unpack_from("<" + f1, b, off1)[0]
            v2 = struct.unpack_from("<" + f2, b, off2)[0]
            struct.pack_into("<" + f1, b, off1, v2)
            struct.pack_into("<" + f2, b, off2, v1)
        return True
    return False


def f_inside_out_shell(j, b):
    cands = [
        m for m in j.get("meshes", [])
        if any(
            p.get("mode", 4) == 4 and "indices" in p and "POSITION" in p.get("attributes", {})
            for p in m.get("primitives", [])
        )
    ]
    if not cands:
        return False
    mesh = max(
        cands,
        key=lambda m: sum(
            j["accessors"][p["attributes"]["POSITION"]]["count"]
            for p in m.get("primitives", [])
            if p.get("mode", 4) == 4 and "indices" in p and "POSITION" in p.get("attributes", {})
        ),
    )
    flipped_tris = 0
    seen_nrms = set()
    for p in mesh.get("primitives", []):
        if p.get("mode", 4) != 4 or "indices" not in p or "POSITION" not in p.get("attributes", {}):
            continue
        acc = p["indices"]
        n_tris = j["accessors"][acc]["count"] // 3
        for t in range(n_tris):
            off1, f1 = elem(j, acc, 3 * t + 1, 0)
            off2, f2 = elem(j, acc, 3 * t + 2, 0)
            v1 = struct.unpack_from("<" + f1, b, off1)[0]
            v2 = struct.unpack_from("<" + f2, b, off2)[0]
            struct.pack_into("<" + f1, b, off1, v2)
            struct.pack_into("<" + f2, b, off2, v1)
        flipped_tris += n_tris
        nrm_acc = p["attributes"].get("NORMAL")
        if nrm_acc is not None and nrm_acc not in seen_nrms and j["accessors"][nrm_acc]["componentType"] == 5126:
            seen_nrms.add(nrm_acc)
            for i in range(j["accessors"][nrm_acc]["count"]):
                for k in range(3):
                    off, _ = elem(j, nrm_acc, i, k)
                    struct.pack_into("<f", b, off, -struct.unpack_from("<f", b, off)[0])
    return flipped_tris >= 4


# --- rfcheck-3 / mesh-hygiene + character-conversion fault injections ---

def _read_indices(j, b, acc):
    cnt = j["accessors"][acc]["count"]
    out = []
    for k in range(cnt):
        off, f = elem(j, acc, k, 0)
        out.append(struct.unpack_from("<" + f, b, off)[0])
    return out


def _read_pos(j, b, acc):
    cnt = j["accessors"][acc]["count"]
    out = []
    for i in range(cnt):
        off, _ = elem(j, acc, i, 0)
        out.append(struct.unpack_from("<fff", b, off))
    return out


def f_open_edge(j, b):
    """Drop 2 triangles from a closed body primitive to open a hole."""
    p = largest_indexed_prim(j)
    if not p:
        return False
    acc = p["indices"]
    if j["accessors"][acc]["count"] < 60:
        return False
    j["accessors"][acc]["count"] -= 6
    return True


def f_non_manifold(j, b):
    """Attach a 3rd fin triangle onto an existing interior edge (edge count = 3)."""
    p = largest_indexed_prim(j)
    if not p:
        return False
    inds = _read_indices(j, b, p["indices"])
    if len(inds) < 9:
        return False
    a, c0, b0 = inds[0], inds[1], inds[2]
    # Pick a distinct 3rd vertex from another triangle so fin has non-zero area and shares edge (a, c0)
    d = next((v for v in inds[3:] if v not in (a, c0, b0)), None)
    if d is None:
        return False
    inds.extend([a, c0, d])
    raw = b"".join(struct.pack("<I", v) for v in inds)
    p["indices"] = append_accessor(j, b, raw, 5125, "SCALAR", len(inds))
    return True


def f_degenerate_tri(j, b):
    """Append a collapsed zero-area duplicate-vertex triangle."""
    p = largest_indexed_prim(j)
    if not p:
        return False
    inds = _read_indices(j, b, p["indices"])
    if len(inds) < 6:
        return False
    inds.extend([inds[0], inds[0], inds[1]])
    raw = b"".join(struct.pack("<I", v) for v in inds)
    p["indices"] = append_accessor(j, b, raw, 5125, "SCALAR", len(inds))
    return True


def f_inconsistent_winding(j, b):
    """Reverse winding of a single triangle so its shared edges disagree with neighbors."""
    p = largest_indexed_prim(j)
    if not p:
        return False
    acc = p["indices"]
    if j["accessors"][acc]["count"] < 9:
        return False
    off1, f1 = elem(j, acc, 1, 0)
    off2, f2 = elem(j, acc, 2, 0)
    v1 = struct.unpack_from("<" + f1, b, off1)[0]
    v2 = struct.unpack_from("<" + f2, b, off2)[0]
    struct.pack_into("<" + f1, b, off1, v2)
    struct.pack_into("<" + f2, b, off2, v1)
    return True


def f_speck(j, b):
    """Add a tiny detached 4-tri floating speck tetrahedron (< 0.5% surface area)."""
    p = largest_indexed_prim(j)
    if not p:
        return False
    pos_acc = p["attributes"]["POSITION"]
    pos = _read_pos(j, b, pos_acc)
    # Build a separate mesh primitive or extend POSITION/attributes cleanly with a 2nd unskinned mesh
    speck_v = [(3.0, 3.0, 3.0), (3.004, 3.0, 3.0), (3.0, 3.004, 3.0), (3.0, 3.0, 3.004)]
    speck_i = [0, 2, 1, 0, 1, 3, 1, 2, 3, 2, 0, 3]
    a_sp = append_accessor(j, b, b"".join(struct.pack("<fff", *v) for v in speck_v), 5126, "VEC3", 4)
    a_si = append_accessor(j, b, b"".join(struct.pack("<H", i) for i in speck_i), 5123, "SCALAR", 12)
    # Attach as a second primitive on mesh 0 so check_mesh_hygiene sees 2 connected components on mesh 0
    j["meshes"][0]["primitives"].append({"attributes": {"POSITION": a_sp}, "indices": a_si})
    return True


def f_sliver(j, b):
    """Add a fan of 30 ultra-thin < 4 deg needle triangles to mesh 0."""
    pts = [(2.0, 0.5, 0.0)]
    for k in range(31):
        pts.append((3.0, 0.5 + 0.008 * k, 0.0))
    tris = []
    for k in range(30):
        tris.extend([0, 1 + k, 2 + k])
    a_p = append_accessor(j, b, b"".join(struct.pack("<fff", *v) for v in pts), 5126, "VEC3", len(pts))
    a_i = append_accessor(j, b, b"".join(struct.pack("<H", i) for i in tris), 5123, "SCALAR", len(tris))
    j["meshes"][0]["primitives"].append({"attributes": {"POSITION": a_p}, "indices": a_i})
    return True


def f_self_fold(j, b):
    """Add 20 acute fold-back triangle pairs (dihedral < 20 deg) onto mesh 0."""
    pts, tris = [], []
    for k in range(20):
        x0 = 2.0 + 0.2 * k
        base = len(pts)
        pts.extend([
            (x0, 0.0, 0.0),
            (x0 + 0.15, 0.0, 0.0),
            (x0 + 0.075, 0.4, 0.0),
            (x0 + 0.075, 0.39, 0.02),
        ])
        tris.extend([base, base + 1, base + 2, base + 1, base, base + 3])
    a_p = append_accessor(j, b, b"".join(struct.pack("<fff", *v) for v in pts), 5126, "VEC3", len(pts))
    a_i = append_accessor(j, b, b"".join(struct.pack("<H", i) for i in tris), 5123, "SCALAR", len(tris))
    j["meshes"][0]["primitives"].append({"attributes": {"POSITION": a_p}, "indices": a_i})
    return True


def f_neck_crack(j, b):
    """Delete triangles in the neck band (72%..87% H) of a humanoid-proportioned mesh."""
    p = largest_indexed_prim(j)
    if not p:
        return False
    pos = _read_pos(j, b, p["attributes"]["POSITION"])
    ys = [v[1] for v in pos]
    xs = [v[0] for v in pos]
    zs = [v[2] for v in pos]
    h = max(ys) - min(ys)
    if h <= 0.3 or h <= 1.2 * (max(zs) - min(zs)):
        return False
    lo_y = min(ys) + 0.74 * h
    hi_y = min(ys) + 0.85 * h
    inds = _read_indices(j, b, p["indices"])
    kept = []
    dropped = 0
    for k in range(0, len(inds), 3):
        a, c0, d0 = inds[k], inds[k + 1], inds[k + 2]
        cy = (pos[a][1] + pos[c0][1] + pos[d0][1]) / 3.0
        if lo_y <= cy <= hi_y and dropped < 6:
            dropped += 1
            continue
        kept.extend([a, c0, d0])
    if dropped < 2:
        return False
    raw = b"".join(struct.pack("<I", v) for v in kept)
    p["indices"] = append_accessor(j, b, raw, 5125, "SCALAR", len(kept))
    return True


def f_uv_shred(j, b):
    """Stretch and shred UVs (>15x anisotropy) across a patch of vertices."""
    p = largest_indexed_prim(j)
    if not p or "TEXCOORD_0" not in p.get("attributes", {}):
        return False
    uv_acc = p["attributes"]["TEXCOORD_0"]
    if j["accessors"][uv_acc]["componentType"] != 5126:
        return False
    cnt = j["accessors"][uv_acc]["count"]
    if cnt < 20:
        return False
    for i in range(0, cnt, 3):
        off_u, _ = elem(j, uv_acc, i, 0)
        off_v, _ = elem(j, uv_acc, i, 1)
        u = struct.unpack_from("<f", b, off_u)[0]
        v = struct.unpack_from("<f", b, off_v)[0]
        struct.pack_into("<f", b, off_u, u * 0.02 + (0.6 if i % 2 == 0 else 0.0))
        struct.pack_into("<f", b, off_v, v * 0.9)
    return True


def f_ref_fold(j, b):
    """Plant a >45 deg pleated surface fold patch on the mesh (tested with --reference)."""
    p = largest_indexed_prim(j)
    if not p:
        return False
    pos_acc = p["attributes"]["POSITION"]
    pos = _read_pos(j, b, pos_acc)
    ys = [v[1] for v in pos]
    h = max(ys) - min(ys)
    mid_y = 0.5 * (min(ys) + max(ys))
    changed = 0
    for i, (x, y, z) in enumerate(pos):
        if abs(y - mid_y) < 0.25 * h and z > 0.0:
            dz = (0.045 * h) if (i % 2 == 0) else (-0.045 * h)
            off, _ = elem(j, pos_acc, i, 2)
            struct.pack_into("<f", b, off, z + dz)
            changed += 1
    return changed >= 8


def f_nicked_chin(j, b):
    """Plant a localized chin/jaw silhouette nick (>4 px drift, tested with --reference)."""
    p = largest_indexed_prim(j)
    if not p:
        return False
    pos_acc = p["attributes"]["POSITION"]
    pos = _read_pos(j, b, pos_acc)
    ys = [v[1] for v in pos]
    h = max(ys) - min(ys)
    ymin = min(ys)
    # Push an unmistakable notch inward on the lower-head/chin silhouette zone
    changed = 0
    for i, (x, y, z) in enumerate(pos):
        t = (y - ymin) / max(h, 1e-6)
        if 0.80 <= t <= 0.92 and abs(x) < 0.12 * h:
            off_z, _ = elem(j, pos_acc, i, 2)
            off_x, _ = elem(j, pos_acc, i, 0)
            struct.pack_into("<f", b, off_z, z * 0.45)
            struct.pack_into("<f", b, off_x, x * 0.45)
            changed += 1
    return changed >= 4


FAULTS = [
    ("W_UNNORMALIZED", f_unnormalized, False),
    ("X_JOINT_RANGE", f_joint_range, False),
    ("W_OVER_INFLUENCE", f_over_influence, False),
    ("R_JOINT_PREFIX", f_prefix, False),
    ("R_JOINT_INDEX", f_joint_index, False),
    ("R_NO_SKIN", f_no_skin, False),
    ("B_NODE_CYCLE", f_cycle, False),
    ("X_ANIM_NAN", f_anim_nan, False),
    ("B_CLIP_NAME", f_clip_name, False),
    ("X_ACCESSOR_BOUNDS", f_bounds, False),
    ("B_PRIM_MODE", f_fan, False),
    ("M_FLIPPED_NORMAL", f_flipped_normal, False),
    ("M_FLIPPED_NORMAL:inside_out", f_inside_out_shell, False),
    # rfcheck-3 / rfcheck-4 mesh-hygiene faults
    ("M_OPEN_EDGE", f_open_edge, False),
    ("M_NON_MANIFOLD", f_non_manifold, False),
    ("M_DEGENERATE_TRI", f_degenerate_tri, False),
    ("M_INCONSISTENT_WINDING", f_inconsistent_winding, False),
    ("M_SPECK", f_speck, False),
    # Character-conversion defect codes
    ("M_SLIVER", f_sliver, False),
    ("M_FOLD:self", f_self_fold, False),
    ("M_NECK_CRACK", f_neck_crack, False),
    ("M_UV_SHRED", f_uv_shred, False),
    # Reference-compared character-conversion faults
    ("M_FOLD:ref", f_ref_fold, True),
    ("M_SILHOUETTE_DRIFT:nicked_chin", f_nicked_chin, True),
]


def check(path, reference=None):
    t0 = time.time()
    cmd = [CLI, "--json"]
    if reference is not None:
        cmd.extend(["--reference", reference])
    cmd.append(path)
    p = subprocess.run(cmd, capture_output=True, text=True)
    dt = time.time() - t0
    codes = []
    try:
        r = json.loads(p.stdout)
        items = r if isinstance(r, list) else r.get("files", [r])
        for it in items:
            for key in ("diags", "findings", "warns"):
                for f in it.get(key, []):
                    codes.append(f.get("code"))
    except Exception:
        codes = [
            line.split()[0] for line in p.stdout.splitlines()
            if line and line.split()[0][:2] in ("D_", "R_", "W_", "X_", "M_", "B_", "L_", "P_")
        ]
    return p.returncode, sorted(set(c for c in codes if c)), dt


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("files", nargs="*")
    ap.add_argument("--tag", default=datetime.date.today().isoformat())
    ap.add_argument("--check", action="store_true", help="Exit 1 on any missed fault or clean false alarm")
    a = ap.parse_args()
    if not os.path.exists(CLI):
        subprocess.run(["cargo", "build", "--release", "-j2"], cwd=ROOT, check=True)

    with tempfile.TemporaryDirectory() as synth_dir:
        synth_files = []
        for kind in ("humanoid", "animal", "tripo_decimated"):
            sp = os.path.join(synth_dir, f"clean_{kind}.glb")
            build_clean_rigged_glb(sp, kind=kind)
            synth_files.append(sp)

        if a.files:
            files = list(a.files)
        else:
            extra = sorted(glob.glob(os.path.expanduser("~/Games/_blender/motion/bench/results/baseline/*/*.glb")))
            files = synth_files + extra

        out = os.path.join(HERE, "results", a.tag)
        os.makedirs(out, exist_ok=True)
        rows, times = [], []
        clean_alarms = []
        for f in files:
            rc, codes, dt = check(f, reference=f)
            times.append(dt)
            bad = [c for c in codes if not c.startswith("P_")]
            if bad:
                clean_alarms.append((os.path.basename(f), bad))
            for code, fn, needs_ref in FAULTS:
                j, b = read(f)
                if not fn(j, b):
                    continue
                m = os.path.join(out, "mut.glb")
                write(m, j, b)
                rc2, codes2, dt2 = check(m, reference=f if needs_ref else None)
                times.append(dt2)
                expected = code.split(":")[0]
                rows.append({
                    "file": os.path.basename(f),
                    "fault": code,
                    "detected": expected in codes2,
                    "exit": rc2,
                    "codes": codes2,
                })

    det = {}
    for r in rows:
        d = det.setdefault(r["fault"], [0, 0])
        d[0] += int(r["detected"])
        d[1] += 1
    summary = {
        "tag": a.tag,
        "date": datetime.datetime.now().isoformat(timespec="seconds"),
        "clean_files": len(files),
        "clean_false_alarms": clean_alarms,
        "detection": det,
        "rows": rows,
        "median_ms": sorted(times)[len(times) // 2] * 1000 if times else None,
    }
    with open(os.path.join(out, "summary.json"), "w") as fh:
        json.dump(summary, fh, indent=1)
    L = [
        f"# rfcheck fault-injection bench `{a.tag}`",
        "",
        f"{len(files)} clean GLBs (humanoid, animal, decimated-character + optional corpus); "
        f"false alarms on them: {len(clean_alarms)}; median check {summary['median_ms']:.0f} ms.",
        "",
        "| injected fault | detected |",
        "|---|---|",
    ]
    for code, _, _ in FAULTS:
        d, n = det.get(code, [0, 0])
        L.append(f"| {code} | {d}/{n} |")
    with open(os.path.join(out, "scorecard.md"), "w") as fh:
        fh.write("\n".join(L) + "\n")
    hist = os.path.join(HERE, "history")
    os.makedirs(hist, exist_ok=True)
    shutil.copy(os.path.join(out, "summary.json"), os.path.join(hist, f"{a.tag}.json"))
    shutil.copy(os.path.join(out, "scorecard.md"), os.path.join(hist, f"{a.tag}.md"))
    print("\n".join(L))

    if a.check:
        errs = []
        if not files:
            errs.append("no clean files checked")
        if clean_alarms:
            errs.append(f"clean false alarms: {clean_alarms}")
        for code, _, _ in FAULTS:
            d, n = det.get(code, [0, 0])
            if n == 0:
                errs.append(f"fault {code} ran on 0 files")
            elif d < n:
                errs.append(f"fault {code} missed: {d}/{n}")
        if errs:
            print("FAIL:", "; ".join(errs), file=sys.stderr)
            sys.exit(1)
        print("FAULT BENCH CHECK PASS")


if __name__ == "__main__":
    main()
