#!/usr/bin/env python3
"""Test a LinuxPlay Flatpak bundle on the VMs of the distro lab.

For each VM: revert to its clean snapshot, boot, install the bundle the way a
user would, then run a host and a client inside the guest's own desktop
session and check that video flows. On X11 sessions the real screen is
captured as well. Nothing here makes a sound, and no VM window opens on the
owner's desktop (the lab runs them headless).

  run.py --lab /path/to/distro-lab --bundle LinuxPlay.flatpak [--out DIR] VM [VM...]
  run.py --lab /path/to/distro-lab --bundle LinuxPlay-VERSION-x86_64.AppImage --all

--bundle takes either package; each is installed the way its README section says.

The lab's own tools (lab/vm.py, lab/guest.py) do the VM handling.
"""
import argparse, json, re, sys, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
GUEST_DIR = "linuxplay-test"


def parse_kv(text):
    """KEY=VALUE lines into a dict; a repeated key becomes a list."""
    out = {}
    for line in text.splitlines():
        m = re.match(r"^([A-Z][A-Z0-9_]*)=(.*)$", line)
        if not m:
            continue
        key, value = m.groups()
        if key in out:
            out[key] = (out[key] if isinstance(out[key], list) else [out[key]]) + [value]
        else:
            out[key] = value
    return out


def stream_ok(result):
    """Did frames reach the screen? The statistics line only carries a latency
    figure once frames have been received, decoded and presented."""
    stats = result.get("STATS", "")
    fps = re.search(r"· (\d+) fps", stats)
    return bool(fps and int(fps.group(1)) >= 5 and re.search(r"Latency \d", stats) and result.get("HOST_STREAM"))


class Run:
    def __init__(self, V, G, vm, out):
        self.V, self.G, self.vm, self.out = V, G, vm, out
        self.result = {"vm": vm.name, "title": vm.cfg.get("title", ""), "checks": {}, "extra_steps": [], "errors": []}

    def guest(self, *args, timeout=900):
        quoted = " ".join("'" + str(a).replace("'", "'\\''") + "'" for a in args)
        r = self.V.ssh_run(self.vm, f"bash ~/{GUEST_DIR}/guest.sh {quoted}", timeout=timeout)
        with open(self.out / "harness.log", "a") as log:
            log.write(f"\n===== guest.sh {' '.join(map(str, args))} (rc {r.returncode}) =====\n{r.stdout}\n{r.stderr}\n")
        kv = parse_kv(r.stdout)
        steps = kv.get("EXTRA_STEP", [])
        for step in steps if isinstance(steps, list) else [steps]:
            if step not in self.result["extra_steps"]:
                self.result["extra_steps"].append(step)
        if kv.get("ERROR"):
            self.result["errors"].append(f"{args[0]}: {kv['ERROR']}")
        return r.returncode, kv

    def check(self, name, ok, detail=""):
        self.result["checks"][name] = {"ok": bool(ok), "detail": detail}
        print(f"  {'ok  ' if ok else 'FAIL'} {name}: {detail}", flush=True)

    def shot(self, name):
        try:
            self.V.screendump(self.vm, self.out / f"{name}.png")
        except Exception as e:  # a screenshot is evidence, not a check
            self.result["errors"].append(f"screenshot {name}: {e}")

    def stream(self, capture):
        """Run host + client in the guest; screenshot while the picture is up."""
        import threading
        shot = threading.Timer(15, self.shot, args=[f"stream-{capture}"])
        shot.start()
        rc, kv = self.guest("stream", capture, timeout=120)
        shot.join()
        self.result[f"stream_{capture}"] = kv
        self.check(f"stream_{capture}", stream_ok(kv),
                   f"{kv.get('VIDEO_DRIVER', '?')} window; {kv.get('STATS', 'no statistics')[:150]}"
                   + (f" | client: {kv['CLIENT_PROBLEMS']}" if kv.get("CLIENT_PROBLEMS") else "")
                   + (f" | host: {kv['HOST_PROBLEMS']}" if kv.get("HOST_PROBLEMS") else ""))
        self.V.pull(self.vm, f"{GUEST_DIR}/*.log", self.out)

    def run(self, bundle, snapshot):
        V, G, vm, R = self.V, self.G, self.vm, self.result
        t0 = time.time()
        if vm.pid():
            V.stop_vm(vm)
        V.revert(vm, snapshot)
        V.start(vm)
        V.wait_ssh(vm, timeout=900)
        G.push_harness(vm)
        V.push(vm, [HERE / "guest.sh", bundle], GUEST_DIR)
        G.wait_session(vm, timeout=600)
        G.guest(vm, "prepare", timeout=120)
        _, info, _ = G.guest(vm, "info", timeout=120)
        R["system"] = {k: info.get(k, "") for k in
                       ("OS_PRETTY", "KERNEL", "DESKTOP", "DE_NAME", "DE_VERSION", "WM_NAME", "WM_VERSION", "SESSION_TYPE", "GLIBC")}
        print(f"  {info.get('OS_PRETTY')} · {info.get('DE_NAME') or info.get('DESKTOP')} {info.get('DE_VERSION', '')} · "
              f"{info.get('SESSION_TYPE')}", flush=True)
        if "GNOME" in info.get("DESKTOP", ""):
            V.send_keys(vm, "esc")  # GNOME starts in the Activities overview
            time.sleep(2)

        rc, kv = self.guest("install", bundle.name, timeout=1500)
        R["install"] = kv
        self.check("install", kv.get("INSTALL_OK") == "yes",
                   f"{kv.get('INSTALL_SECONDS', '?')} s, {kv.get('FLATPAK') or kv.get('RUNTIME', '?')}"
                   + (f"; extra steps: {'; '.join(R['extra_steps'])}" if R["extra_steps"] else ""))
        if kv.get("INSTALL_OK") != "yes":
            return
        _, facts = self.guest("facts", timeout=180)
        R["facts"] = facts
        self.check("commands", facts.get("HELP_CLIENT", "0") != "0" and facts.get("HELP_HOST", "0") != "0", "both programs answer --help")
        self.check("encoders", bool(facts.get("ENCODERS")), facts.get("ENCODERS", "none"))
        self.stream("test")
        if info.get("SESSION_TYPE") == "x11":
            self.check("x11_capture_plugin", facts.get("X11_CAPTURE") == "yes", "ximagesrc in the package")
            self.stream("x11")
        R["uinput"] = facts.get("UINPUT", "")
        R["seconds"] = round(time.time() - t0)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--lab", required=True, type=Path, help="the distro lab's folder")
    ap.add_argument("--bundle", required=True, type=Path)
    ap.add_argument("--out", type=Path, help="where results go (default: <lab>/results-linuxplay)")
    ap.add_argument("--snapshot", default="clean")
    ap.add_argument("--all", action="store_true", help="every VM that has the snapshot")
    ap.add_argument("--keep-running", action="store_true")
    ap.add_argument("vms", nargs="*")
    a = ap.parse_args()

    sys.path.insert(0, str(a.lab / "lab"))
    import vm as V, guest as G  # the lab's own modules

    if any(V.VM(n).cfg.get("disk_bus") == "nvme" for n in a.vms):
        # SteamOS's disk sits on NVMe, which only its recipe knows how to
        # attach: importing it wraps the lab's QEMU arguments.
        sys.path.insert(0, str(a.lab / "lab" / "recipes"))
        import steamos  # noqa: F401
    names = a.vms or (sorted(p.name for p in (a.lab / "vms").iterdir() if (p / "vm.json").exists()) if a.all else [])
    if not names:
        ap.error("name VMs, or pass --all")
    out_root = a.out or a.lab / "results-linuxplay"
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    summary = []
    for name in names:
        vm = V.VM(name)
        if a.snapshot not in V.snapshots(vm):
            print(f"{name}: no snapshot {a.snapshot!r}, skipped", flush=True)
            continue
        out = out_root / name / stamp
        out.mkdir(parents=True)
        run = Run(V, G, vm, out)
        print(f"{name}:", flush=True)
        try:
            run.run(a.bundle.resolve(), a.snapshot)
        except Exception as e:
            run.result["errors"].append(f"{type(e).__name__}: {e}")
            print(f"  ERROR {type(e).__name__}: {e}", flush=True)
        finally:
            if not a.keep_running and vm.pid():
                try:
                    run.guest("stop", timeout=30)
                except Exception:
                    pass
                V.stop_vm(vm)
        R = run.result
        R["package"] = "AppImage" if a.bundle.suffix == ".AppImage" else "Flatpak"
        R["passed"] = bool(R["checks"]) and all(c["ok"] for c in R["checks"].values()) and not R["errors"]
        (out / "result.json").write_text(json.dumps(R, indent=2) + "\n")
        summary.append(R)
        print(f"  => {'PASSED' if R['passed'] else 'FAILED'} in {R.get('seconds', '?')} s ({out})", flush=True)
    (out_root / f"summary-{stamp}.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(f"\n{sum(r['passed'] for r in summary)} of {len(summary)} passed")


if __name__ == "__main__":
    main()
