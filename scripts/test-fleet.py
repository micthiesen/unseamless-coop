#!/usr/bin/env python3
"""Exercise fleet launch and migration guards without touching live sessions."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class FleetTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="unseamless-fleet-test-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.repo = self.root / "repo"
        self.home = self.root / "home"
        self.state = self.root / "fleet"
        self.bin = self.root / "bin"
        self.log = self.root / "calls.jsonl"
        self.bin.mkdir()
        self.state.mkdir()
        (self.home / "Code/.rifts/repo").mkdir(parents=True)
        source = Path(__file__).resolve().parent
        shutil.copytree(source / "fleet", self.repo / "scripts/fleet")
        shutil.copytree(source.parent / "docs/roles", self.repo / "docs/roles")
        # Redirect home-dependent paths in test copies, keeping the real HOME and
        # CODEX_HOME untouched. Trust writes are separately replaced with no-ops.
        for f in (self.repo / "scripts/fleet").iterdir():
            if f.is_file():
                f.write_text(f.read_text().replace("$HOME", "$FLEET_TEST_HOME"))
        (self.repo / "scripts/fleet/_codex").write_text(
            "fleet_codex_trust_add() { :; }\nfleet_codex_trust_remove() { :; }\n"
        )
        stub = '''#!/usr/bin/env python3
import json, os, pathlib, sys
name=pathlib.Path(sys.argv[0]).name
args=sys.argv[1:]
with open(os.environ["FLEET_TEST_LOG"],"a") as f:
    f.write(json.dumps([name,*args])+"\\n")
if name == "tmux":
    if args[0] == "has-session":
        sys.exit(0 if args[-1] in os.environ.get("FLEET_TEST_LIVE","").split(",") else 1)
    if args[0] == "load-buffer": sys.stdin.read()
elif name == "rift":
    base=pathlib.Path(os.environ["FLEET_TEST_HOME"])/"Code/.rifts/repo"
    if args[0] == "create":
        ws=base/args[-1]; (ws/"docs/roles").mkdir(parents=True)
        (ws/"docs/roles/worker.md").write_text("Worker role")
        (ws/"docs/roles/worker-solo.md").write_text("Solo role")
    elif args[0] == "list":
        for ws in sorted(base.iterdir()): print(ws)
elif name == "git":
    if "branch" in args: print("worker/test")
    elif "rev-list" in args: print("0")
'''
        for name in ["tmux", "rift", "git"]:
            f = self.bin / name
            f.write_text(stub)
            f.chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                        FLEET_TEST_HOME=str(self.home), FLEET_TEST_LOG=str(self.log),
                        UNSEAMLESS_FLEET_DIR=str(self.state), DISPLAY="", WAYLAND_DISPLAY="")

    def run_script(self, name, *args, ok=True, text=None):
        result = subprocess.run(["bash", str(self.repo / "scripts/fleet" / name), *args],
                                env=self.env, input=text, text=True, capture_output=True)
        if ok:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0)
        return result

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def marker(self, name, harness):
        (self.state / "assignments").mkdir(exist_ok=True)
        (self.state / "assignments" / (name + ".harness")).write_text(harness)
        (self.home / "Code/.rifts/repo" / name).mkdir(exist_ok=True)

    def test_codex_worker_seed_model_and_resume(self):
        self.run_script("worker-new", "--model", "requested-model", "lane", "-",
                        text="Preserve `literal` and $(literal) text.")
        spawn = next(c for c in self.calls() if c[:2] == ["tmux", "new-session"])
        self.assertIn("codex", spawn)
        self.assertIn("requested-model", spawn)
        self.assertIn("docs/roles/worker.md", spawn[-1])
        self.assertIn("assignment", spawn[-1])
        assignment = (self.state / "assignments/lane.md").read_text()
        self.assertIn("Preserve `literal` and $(literal) text.", assignment)
        self.run_script("worker-open", "lane")
        revive = [c for c in self.calls() if c[:2] == ["tmux", "new-session"]][-1]
        self.assertIn("resume", revive)
        self.assertIn("--last", revive)
        self.assertIn("requested-model", revive)

    def test_solo_worker_uses_default_model_and_solo_role(self):
        self.run_script("worker-new", "--solo", "solo")
        spawn = next(c for c in self.calls() if c[:2] == ["tmux", "new-session"])
        self.assertIn("docs/roles/worker-solo.md", spawn[-1])
        self.assertNotIn("-m", spawn)
        self.assertFalse((self.state / "assignments/solo.md").exists())
        self.run_script("worker-open", "solo")

    def test_retired_workers_do_not_hide_current_workers(self):
        self.marker("a-old", "claude")
        self.marker("b-new", "codex")
        self.env["FLEET_TEST_LIVE"] = "usc-worker-a-old,usc-worker-b-new"
        result = self.run_script("worker-ls")
        self.assertIn("a-old", result.stdout)
        self.assertIn("retired", result.stdout)
        self.assertIn("b-new", result.stdout)
        self.run_script("msg", "usc-worker-a-old", "test", ok=False)
        self.assertFalse(any(c[:2] == ["tmux", "paste-buffer"] for c in self.calls()))

    def test_retired_worker_cannot_resume(self):
        self.marker("old", "claude")
        self.run_script("worker-open", "old", ok=False)
        self.assertFalse(any(c[:2] == ["tmux", "new-session"] for c in self.calls()))

    def test_old_orchestrator_requires_restart(self):
        self.env["FLEET_TEST_LIVE"] = "usc-orch"
        self.run_script("orch-start", ok=False)
        self.run_script("msg", "usc-orch", "test", ok=False)
        self.assertFalse(any(c[:2] in [["tmux", "attach"], ["tmux", "paste-buffer"]]
                             for c in self.calls()))

    def test_new_orchestrator_resume_and_marker(self):
        self.run_script("orch-start", "--continue")
        self.assertEqual((self.state / "orchestrator.harness").read_text().strip(), "codex")
        launch = next(c for c in self.calls() if c[:2] == ["tmux", "new-session"])
        self.assertIn("resume", launch)
        self.assertIn("--last", launch)
        self.assertFalse(any("Boot: read" in part for part in launch))
        self.env["FLEET_TEST_LIVE"] = "usc-orch"
        self.run_script("orch-start")
        self.run_script("msg", "usc-orch", "test")
        self.assertTrue(any(c[:2] == ["tmux", "paste-buffer"] for c in self.calls()))

    def test_retired_spawn_flag_rejected_before_creation(self):
        self.run_script("worker-new", "--harness", "claude", "old", ok=False)
        self.assertFalse(self.log.exists())


if __name__ == "__main__":
    unittest.main()
