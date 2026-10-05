# SPDX-License-Identifier: MIT
"""Import GLBs in headless Blender and check the skinning survived.

    Blender --background --factory-startup --python blender_import.py -- \
        [--export-dir DIR] a.glb b.glb ...

Per file: one JSON line with mesh/armature counts, vertex-group stats,
max influences per vertex, and weighted vertices whose weights do not sum
to 1 (tolerance 1e-3, as rfcheck). With --export-dir, also re-exports the
imported scene as GLB (Blender's own writer) for the reverse direction.
Exit 1 if any import fails or a skinned mesh comes in with no weights.
"""

import json
import os
import sys

import bpy

TOL = 1e-3


def args():
    argv = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else []
    export_dir = None
    if argv[:1] == ["--export-dir"]:
        export_dir, argv = argv[1], argv[2:]
    return export_dir, argv


def check(path, export_dir):
    bpy.ops.wm.read_factory_settings(use_empty=True)
    bpy.ops.import_scene.gltf(filepath=path)
    meshes = [o for o in bpy.data.objects if o.type == "MESH"]
    arms = [o for o in bpy.data.objects if o.type == "ARMATURE"]
    out = {"file": os.path.basename(path), "meshes": len(meshes), "armatures": len(arms),
           "bones": sum(len(a.data.bones) for a in arms), "verts": 0, "skinned_meshes": 0,
           "unweighted_verts": 0, "max_influences": 0, "bad_sums": 0}
    problems = []
    for o in meshes:
        skinned = any(m.type == "ARMATURE" for m in o.modifiers) or o.parent_type == "ARMATURE"
        out["verts"] += len(o.data.vertices)
        if not skinned:
            continue
        out["skinned_meshes"] += 1
        weighted = 0
        for v in o.data.vertices:
            ws = [g.weight for g in v.groups if g.weight > 0.0]
            if not ws:
                out["unweighted_verts"] += 1
                continue
            weighted += 1
            out["max_influences"] = max(out["max_influences"], len(ws))
            if abs(sum(ws) - 1.0) > TOL:
                out["bad_sums"] += 1
        if weighted == 0:
            problems.append(f"{o.name}: skinned but no vertex weights")
    if export_dir:
        dst = os.path.join(export_dir, os.path.basename(path))
        bpy.ops.export_scene.gltf(filepath=dst, export_format="GLB")
        out["exported"] = dst
    out["ok"] = not problems
    if problems:
        out["problems"] = problems
    return out


def main():
    export_dir, files = args()
    failed = 0
    for f in files:
        try:
            r = check(os.path.abspath(f), export_dir)
        except Exception as e:  # import errors are the result, not a crash
            r = {"file": os.path.basename(f), "ok": False, "problems": [f"import failed: {e}"]}
        failed += not r["ok"]
        print("GLBKIT " + json.dumps(r), flush=True)
    print(f"GLBKIT {len(files) - failed} of {len(files)} imported cleanly in Blender {bpy.app.version_string}")
    sys.exit(1 if failed else 0)


main()
