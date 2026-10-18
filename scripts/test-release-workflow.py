#!/usr/bin/env python3
"""Execute release publication and Homebrew scripts offline with hostile inputs."""

import base64
import hashlib
import json
import os
from pathlib import Path
import re
import self_test_hang
import socket
import subprocess
import tempfile
import textwrap
import threading
import time
import unittest

try:
    import yaml
except ImportError:
    yaml = None


ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github/workflows/release.yml"
MACOS_BUNDLE = "orbit-desktop-universal-apple-darwin.dmg"
LINUX_BUNDLES = (
    "orbit-desktop-x86_64-unknown-linux-gnu.deb",
    "orbit-desktop-x86_64-unknown-linux-gnu.AppImage",
    "orbit-desktop-aarch64-unknown-linux-gnu.deb",
    "orbit-desktop-aarch64-unknown-linux-gnu.AppImage",
)
DESKTOP_BUNDLES = (MACOS_BUNDLE, *LINUX_BUNDLES)
APPLE_SECRETS = ("APPLE_CERTIFICATE", "APPLE_CERTIFICATE_PASSWORD", "APPLE_SIGNING_IDENTITY",
                 "APPLE_ID", "APPLE_PASSWORD", "APPLE_TEAM_ID")
DESKTOP_VERSION = "orbit-desktop-version.txt"
DESKTOP_MANIFEST = "orbit-desktop-checksums.txt"
DESKTOP_SIGNATURE = DESKTOP_MANIFEST + ".sig"


@unittest.skipIf(yaml is None, "PyYAML not installed; the workflow guard skips locally without it")
class ReleaseWorkflowTests(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch, prefix="release-workflow-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.env = dict(PATH=os.environ["PATH"], LANG="C", TMPDIR=str(self.root),
                        GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
        document = yaml.safe_load(WORKFLOW.read_text())
        self.workflow = document
        self.workflow_name = document["name"]
        self.jobs = document["jobs"]
        self.metadata = next(step for step in self.jobs["publish-release"]["steps"]
                             if step.get("id") == "metadata")
        self.tap = self.jobs["bump-homebrew-tap"]
        self.tap_runs = [step for step in self.tap["steps"] if "run" in step]
        self.preflight = self.jobs["desktop-preflight"]["steps"][0]
        # Values of the non-secret expressions desktop steps bind to env.
        self.bindings = {
            "${{ github.event_name }}": "push",
            "${{ inputs.tag }}": "",
            "${{ vars.ORBIT_DESKTOP_RELEASE }}": "",
            "${{ needs.desktop-preflight.outputs.tag }}": "v1.2.3",
            "${{ needs.desktop-preflight.outputs.macos }}": "true",
        }
        self.apple_secrets = {name: f"offline-{name.lower()}" for name in APPLE_SECRETS}
        self.outputs = dict(version="1.2.3-rc.1",
                            darwin_arm_asset="orbit-aarch64-apple-darwin.tar.gz",
                            darwin_arm_sha="a" * 64,
                            darwin_intel_asset="orbit-x86_64-apple-darwin.tar.gz",
                            darwin_intel_sha="b" * 64)
        dist = self.root / "dist"
        dist.mkdir()
        (dist / "orbit-checksums.txt").write_text(
            f"{'a' * 64}  {self.outputs['darwin_arm_asset']}\n"
            f"{'b' * 64}  {self.outputs['darwin_intel_asset']}\n")
        self.output = self.root / "output"

    def run_script(self, step, cwd=None, timeout=None, **values):
        # Resolve the actual step env bindings, as Actions does, without ever
        # substituting expressions into Bash source.
        env = dict(self.env)
        for key, value in step.get("env", {}).items():
            output = re.fullmatch(r"\$\{\{ needs\.publish-release\.outputs\.(\w+) \}\}", value)
            if output:
                env[key] = self.outputs[output[1]]
            elif value in ("${{ secrets.TAP_GITHUB_TOKEN }}", "${{ github.token }}"):
                env[key] = "offline-test-token"
            elif value == "${{ secrets.ORBIT_RELEASE_SIGNING_KEY_PEM }}":
                env[key] = self.signing_key.read_text()
            elif value in self.bindings:
                env[key] = self.bindings[value]
            elif (secret := re.fullmatch(r"\$\{\{ secrets\.(APPLE_\w+) \}\}", value)):
                env[key] = self.apple_secrets.get(secret[1], "")
            else:
                self.fail(f"unsupported workflow env binding: {key}")
        env.update(values)
        return self_test_hang.run(
            ["bash", "-e", "-o", "pipefail", "-c", step["run"]],
            step=str(step.get("id") or step.get("name") or "workflow step"),
            cwd=cwd or self.root, env=env, text=True, capture_output=True,
            timeout=timeout)

    def git(self, *arguments):
        result = self_test_hang.run(
            ["git", *arguments], step="git", cwd=self.root, env=self.env,
            text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip()

    def prepare_desktop(self):
        if hasattr(self, "remote"):
            return
        self.remote = self.root / "release"
        self.remote.mkdir()
        self.upload_log = self.root / "uploads.jsonl"
        self.signing_key = self.root / "signing-key.pem"
        public_key = self.root / "npm/release-signing.pub"
        public_key.parent.mkdir()
        for args in (("genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256",
                      "-out", str(self.signing_key)),
                     ("pkey", "-in", str(self.signing_key), "-pubout", "-out", str(public_key))):
            result = self_test_hang.run(
                ["openssl", *args], step="openssl", capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
        for name in DESKTOP_BUNDLES:
            (self.root / "dist" / name).write_bytes(f"rebuilt {name}".encode())

        # A file-backed release API at the public gh command boundary. In
        # particular, upload checks ALL names before writing any file, as gh
        # does without --clobber. No network or real release credentials.
        binary_dir = self.root / "bin"
        binary_dir.mkdir()
        gh = binary_dir / "gh"
        gh.write_text(textwrap.dedent('''\
            #!/usr/bin/env python3
            import json
            import os
            from pathlib import Path
            import shutil
            import sys

            args = sys.argv[1:]
            remote = Path(os.environ["FAKE_RELEASE"])
            assert args[0] == "release", args
            command, tag = args[1:3]
            accepted = os.environ.get("FAKE_ACCEPT_TAGS", "v1.2.3").split(",")
            if tag not in accepted:
                sys.exit("release not found")
            options = args[3:]
            def option(name):
                index = options.index(name)
                value = options[index + 1]
                del options[index:index + 2]
                return value
            assert option("--repo") == "offline/orbit", args
            if command == "view":
                # Optional same-process gate so two publishers can prove they
                # both reached gh before either uploads. Unset in every other test.
                barrier = os.environ.get("FAKE_GH_BARRIER")
                if barrier:
                    import socket
                    host, port = barrier.rsplit(":", 1)
                    with socket.create_connection((host, int(port)), timeout=15) as gate:
                        label = os.environ.get("FAKE_GH_BARRIER_LABEL", "publish")
                        gate.sendall(label.encode() + b"\\n")
                        assert gate.recv(1) == b"+"
                fields = option("--json")
                if fields == "tagName":
                    assert option("--jq") == ".tagName", args
                    print(tag)
                else:
                    assert fields == "assets", args
                    assert option("--jq") == ".assets[].name", args
                    print("\\n".join(sorted(p.name for p in remote.iterdir())))
                assert not options, args
            elif command == "download":
                name = option("--pattern")
                destination = Path(option("--dir")) / name
                assert options == ["--clobber"], args
                shutil.copyfile(remote / name, destination)
            elif command == "upload":
                paths = [Path(value) for value in options]
                assert paths and not any(value.startswith("--") for value in options), args
                if any((remote / path.name).exists() for path in paths):
                    sys.exit("asset under the same name already exists")
                for path in paths:
                    log = Path(os.environ["FAKE_UPLOAD_LOG"])
                    count = len(log.read_text().splitlines()) if log.exists() else 0
                    if count == int(os.environ.get("FAIL_AFTER_UPLOADS", "-1")):
                        sys.exit("injected upload interruption")
                    shutil.copyfile(path, remote / path.name)
                    with log.open("a") as output:
                        output.write(json.dumps(path.name) + "\\n")
            else:
                raise AssertionError(args)
            '''))
        gh.chmod(0o755)
        self.env.update(PATH=str(binary_dir) + os.pathsep + self.env["PATH"],
                        FAKE_RELEASE=str(self.remote), FAKE_UPLOAD_LOG=str(self.upload_log),
                        GITHUB_REPOSITORY="offline/orbit")

    def plan_desktop(self, **values):
        """Run desktop-preflight; return its outputs, or None when it fails."""
        self.prepare_desktop()
        self.output.write_text("")
        summary = self.root / "summary.md"
        summary.write_text("")
        values.setdefault("GITHUB_REF_NAME", "v1.2.3")
        result = self.run_script(self.preflight, GITHUB_OUTPUT=str(self.output),
                                 GITHUB_STEP_SUMMARY=str(summary), **values)
        self.plan_log = result.stdout + result.stderr + summary.read_text()
        if result.returncode:
            self.assertEqual(self.output.read_text(), "", "a failed plan must publish no outputs")
            return None
        return dict(line.split("=", 1) for line in self.output.read_text().splitlines())

    def publish_desktop(self, cwd=None, timeout=None, **values):
        for step in self.jobs["publish-desktop"]["steps"]:
            if "run" in step:
                result = self.run_script(step, cwd=cwd, timeout=timeout, **values)
                if result.returncode:
                    return result
        return result

    def require_desktop_publish_serialization(self):
        """The job concurrency group is the cross-run lock GitHub holds for the whole job."""
        self.assertIsNone(self.workflow.get("concurrency"),
                          "only publish-desktop waits; other release jobs keep running")
        for name, job in self.jobs.items():
            if name == "publish-desktop":
                continue
            self.assertNotIn("concurrency", job,
                             f"{name} stays outside desktop publication serialization")
        concurrency = self.jobs["publish-desktop"].get("concurrency")
        self.assertIsInstance(concurrency, dict,
                              "tag pushes, dispatches and reruns share one publish-desktop group")
        self.assertIs(concurrency.get("cancel-in-progress"), False,
                      "an active publish-desktop uploader must finish; a later attempt must not cancel it")
        self.assertEqual(concurrency.get("queue"), "max",
                         "every waiting publisher for one tag stays queued behind the active uploader")
        group = concurrency.get("group")
        self.assertIsInstance(group, str)
        expressions = {" ".join(expr.split())
                       for expr in re.findall(r"\$\{\{\s*([^}]+?)\s*\}\}", group)}
        self.assertEqual(expressions, {"github.workflow", "needs.desktop-preflight.outputs.tag"},
                         "the group follows the preflight tag inside this workflow, for every trigger")
        return group

    def desktop_publish_group(self, tag):
        group = self.require_desktop_publish_serialization()
        rendered = re.sub(r"\$\{\{\s*github\.workflow\s*\}\}", str(self.workflow_name), group)
        rendered = re.sub(r"\$\{\{\s*needs\.desktop-preflight\.outputs\.tag\s*\}\}", tag, rendered)
        self.assertNotIn("${{", rendered, rendered)
        self.assertIn(str(self.workflow_name), rendered)
        self.assertIn(tag, rendered)
        return rendered

    def stage_release_checkout(self, root: Path) -> Path:
        """A second tag's tree: local bundles, the release public key, and an empty release."""
        dist = root / "dist"
        dist.mkdir(parents=True)
        for name in DESKTOP_BUNDLES:
            (dist / name).write_bytes(f"rebuilt {name}".encode())
        public_key = root / "npm" / "release-signing.pub"
        public_key.parent.mkdir(parents=True)
        public_key.write_bytes((self.root / "npm" / "release-signing.pub").read_bytes())
        remote = root / "release"
        remote.mkdir()
        return remote

    def uploaded_names(self):
        return [json.loads(line) for line in self.upload_log.read_text().splitlines()] \
            if self.upload_log.exists() else []

    def assert_desktop_authenticated(self, bundles=DESKTOP_BUNDLES, root=None, remote=None,
                                     version="1.2.3"):
        root = self.root if root is None else root
        remote = self.remote if remote is None else remote
        result = self_test_hang.run(
            ["openssl", "dgst", "-sha256", "-verify", str(root / "npm/release-signing.pub"),
             "-signature", str(remote / DESKTOP_SIGNATURE), str(remote / DESKTOP_MANIFEST)],
            step="openssl dgst", text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        digests = {name: digest for digest, name in
                   (line.split("  ") for line in
                    (remote / DESKTOP_MANIFEST).read_text().splitlines())}
        self.assertEqual(set(digests), {*bundles, DESKTOP_VERSION})
        for name, digest in digests.items():
            self.assertEqual(digest, hashlib.sha256((remote / name).read_bytes()).hexdigest(),
                             f"signed manifest must authenticate published {name}")
        self.assertEqual((remote / DESKTOP_VERSION).read_text(), f"{version}\n")

    def test_desktop_fresh_and_complete_publication(self):
        self.prepare_desktop()
        # Fresh release: all bundles precede the authenticated metadata.
        result = self.publish_desktop()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(set(self.uploaded_names()),
                         {*DESKTOP_BUNDLES, DESKTOP_VERSION, DESKTOP_MANIFEST, DESKTOP_SIGNATURE})
        self.assertEqual(self.uploaded_names()[-2:], [DESKTOP_MANIFEST, DESKTOP_SIGNATURE])
        self.assert_desktop_authenticated()
        published = {path.name: path.read_bytes() for path in self.remote.iterdir()}

        # Complete retry with different build bytes must preserve every asset.
        for name in DESKTOP_BUNDLES:
            (self.root / "dist" / name).write_bytes(b"another rebuild")
        result = self.publish_desktop()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.uploaded_names()), 8)
        self.assertEqual({path.name: path.read_bytes() for path in self.remote.iterdir()}, published)
        self.assert_desktop_authenticated()

    def test_desktop_partial_retry_hashes_published_bundle_bytes(self):
        self.prepare_desktop()
        dmg = DESKTOP_BUNDLES[0]
        (self.remote / dmg).write_bytes(b"previously published DMG; differs from local rebuild")
        published = (self.remote / dmg).read_bytes()
        result = self.publish_desktop()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(set(self.uploaded_names()),
                         {*DESKTOP_BUNDLES[1:], DESKTOP_VERSION, DESKTOP_MANIFEST, DESKTOP_SIGNATURE})
        self.assertEqual((self.remote / dmg).read_bytes(), published)
        self.assert_desktop_authenticated()

    def test_desktop_recovers_upload_interruptions_including_unsigned_manifest(self):
        self.prepare_desktop()
        for count in (1, 6, 7):
            with self.subTest(uploaded_before_failure=count):
                for path in self.remote.iterdir():
                    path.unlink()
                self.upload_log.write_text("")
                self.env["FAIL_AFTER_UPLOADS"] = str(count)
                result = self.publish_desktop()
                self.assertNotEqual(result.returncode, 0, result.stderr)
                self.assertEqual(len(self.uploaded_names()), count)
                published = {path.name: path.read_bytes() for path in self.remote.iterdir()}
                self.env.pop("FAIL_AFTER_UPLOADS")
                for name in DESKTOP_BUNDLES:
                    (self.root / "dist" / name).write_bytes(f"rebuild after {count}".encode())
                result = self.publish_desktop()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(len(self.uploaded_names()), 8)
                for name, data in published.items():
                    self.assertEqual((self.remote / name).read_bytes(), data)
                self.assert_desktop_authenticated()

    def test_desktop_legacy_metadata_must_match_before_any_new_upload(self):
        self.prepare_desktop()
        result = self.publish_desktop()
        self.assertEqual(result.returncode, 0, result.stderr)
        original = {path.name: path.read_bytes() for path in self.remote.iterdir()}
        # The old glob upload could publish metadata before all bundles.
        # Recover if the missing bundle is still available with the same bytes.
        dmg = DESKTOP_BUNDLES[0]
        (self.remote / dmg).unlink()
        self.upload_log.write_text("")
        result = self.publish_desktop()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.uploaded_names(), [dmg])
        self.assert_desktop_authenticated()

        # Changed rebuilt bytes cannot satisfy an immutable published manifest.
        (self.remote / dmg).unlink()
        (self.root / "dist" / dmg).write_bytes(b"different build")
        self.upload_log.write_text("")
        result = self.publish_desktop()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.uploaded_names(), [])
        self.assertEqual({path.name: path.read_bytes() for path in self.remote.iterdir()},
                         {name: data for name, data in original.items() if name != dmg})

        # An orphan signature from an old partial upload is also authoritative:
        # reject mismatched candidate bytes even when its manifest is absent.
        (self.remote / DESKTOP_MANIFEST).unlink()
        result = self.publish_desktop()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.uploaded_names(), [])
        (self.root / "dist" / dmg).write_bytes(original[dmg])
        result = self.publish_desktop()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.uploaded_names(), [dmg, DESKTOP_MANIFEST])
        self.assertEqual({path.name: path.read_bytes() for path in self.remote.iterdir()}, original)
        self.assert_desktop_authenticated()

    def test_desktop_plan_builds_linux_always_and_macos_only_with_every_apple_secret(self):
        # Each matrix key a build-desktop step reads must come from the plan.
        used = set(re.findall(r"matrix\.(\w+)", json.dumps(self.jobs["build-desktop"])))
        for missing in ((), ("APPLE_TEAM_ID",), APPLE_SECRETS):
            for opt_in in ("", "true"):
                with self.subTest(missing=missing, ORBIT_DESKTOP_RELEASE=opt_in):
                    self.bindings["${{ vars.ORBIT_DESKTOP_RELEASE }}"] = opt_in
                    self.apple_secrets = {name: "" if name in missing else "set"
                                          for name in APPLE_SECRETS}
                    plan = self.plan_desktop()
                    self.assertIsNotNone(plan, self.plan_log)
                    self.assertEqual(plan["enabled"], "true",
                                     "an unset opt-in must not suppress the Linux bundles")
                    self.assertEqual(plan["tag"], "v1.2.3")
                    self.assertEqual(plan["macos"], "false" if missing else "true")
                    legs = json.loads(plan["matrix"])["include"]
                    for leg in legs:
                        self.assertEqual(set(leg), used)
                    # The legs package exactly the bundles publish-desktop signs.
                    built = {f"orbit-desktop-{leg['target']}.{suffix}" for leg in legs
                             for suffix in (("dmg",) if leg["runs_on"].startswith("macos")
                                            else ("deb", "AppImage"))}
                    self.assertEqual(built, set(LINUX_BUNDLES) if missing
                                     else set(DESKTOP_BUNDLES))
                    for name in missing:
                        self.assertIn(name, self.plan_log,
                                      "a skipped macOS leg must name the missing secret")

    def test_desktop_opt_out_turns_off_tag_pushes_but_not_a_backfill(self):
        self.bindings["${{ vars.ORBIT_DESKTOP_RELEASE }}"] = "false"
        plan = self.plan_desktop()
        self.assertEqual(plan and plan["enabled"], "false", self.plan_log)
        self.bindings.update({"${{ github.event_name }}": "workflow_dispatch",
                              "${{ inputs.tag }}": "v1.2.3"})
        plan = self.plan_desktop(GITHUB_REF_NAME="agent-main")
        self.assertEqual(plan and (plan["enabled"], plan["tag"]), ("true", "v1.2.3"),
                         self.plan_log)

    def test_desktop_backfill_needs_an_existing_release_and_a_plain_tag(self):
        self.bindings["${{ github.event_name }}"] = "workflow_dispatch"
        for tag in ("v1.2.4", "", "agent-main", "v1.2.3\nmacos=true", 'v1.2.3$(touch sentinel)',
                    "v1.2.3\n::warning::injected"):
            with self.subTest(tag=tag):
                self.bindings["${{ inputs.tag }}"] = tag
                self.assertIsNone(self.plan_desktop(GITHUB_REF_NAME="v1.2.3"), self.plan_log)
                self.assertNotIn("::warning::injected", self.plan_log)
                self.assertFalse((self.root / "sentinel").exists())

    def test_desktop_linux_only_publication_and_later_macos_conflict(self):
        self.prepare_desktop()
        self.bindings["${{ needs.desktop-preflight.outputs.macos }}"] = "false"
        (self.root / "dist" / MACOS_BUNDLE).unlink()
        result = self.publish_desktop()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(set(self.uploaded_names()),
                         {*LINUX_BUNDLES, DESKTOP_VERSION, DESKTOP_MANIFEST, DESKTOP_SIGNATURE})
        self.assert_desktop_authenticated(LINUX_BUNDLES)
        published = {path.name: path.read_bytes() for path in self.remote.iterdir()}

        # A Linux-only retry verifies the release without uploading again.
        self.upload_log.write_text("")
        self.assertEqual(self.publish_desktop().returncode, 0)
        self.assertEqual(self.uploaded_names(), [])

        # A macOS bundle built later cannot join the published manifest.
        self.bindings["${{ needs.desktop-preflight.outputs.macos }}"] = "true"
        (self.root / "dist" / MACOS_BUNDLE).write_bytes(b"signed later")
        result = self.publish_desktop()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.uploaded_names(), [])
        self.assertEqual({path.name: path.read_bytes() for path in self.remote.iterdir()},
                         published)

    def test_desktop_linux_only_run_refuses_a_published_macos_bundle(self):
        # A Linux-only manifest would leave the published DMG unauthenticated.
        self.prepare_desktop()
        (self.remote / MACOS_BUNDLE).write_bytes(b"orphan DMG from a partial upload")
        self.bindings["${{ needs.desktop-preflight.outputs.macos }}"] = "false"
        result = self.publish_desktop()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.uploaded_names(), [])
        self.assertEqual({path.name for path in self.remote.iterdir()}, {MACOS_BUNDLE})

    def test_overlapping_same_tag_publishers_wait_and_refuse_an_unlisted_dmg(self):
        # GitHub holds the concurrency group for the whole job. This lock is
        # that admission: the full plan cannot enter the publish script while
        # the Linux-only plan holds the tag, then it refuses before uploading.
        self.prepare_desktop()
        group = self.desktop_publish_group("v1.2.3")
        self.assertNotEqual(group, self.desktop_publish_group("v9.9.9"))
        self.bindings["${{ needs.desktop-preflight.outputs.macos }}"] = "false"
        (self.root / "dist" / MACOS_BUNDLE).unlink()
        lock = threading.Lock()
        holding = threading.Event()
        excluded = threading.Event()
        proceed = threading.Event()
        results = {}

        def run_linux():
            owns = False
            try:
                lock.acquire()
                owns = True
                holding.set()
                if not proceed.wait(self_test_hang.HANG_CEILING_SECONDS):
                    raise TimeoutError("linux publisher was not released to upload")
                results["linux"] = self.publish_desktop()
            except Exception as exc:
                results["linux_error"] = exc
            finally:
                holding.set()
                if owns:
                    lock.release()

        def run_full():
            owns = False
            try:
                if not holding.wait(self_test_hang.HANG_CEILING_SECONDS):
                    raise TimeoutError("linux publisher never held the tag group")
                acquired = lock.acquire(blocking=False)
                results["entered_during_linux"] = acquired
                if acquired:
                    lock.release()
                    return
                excluded.set()
                lock.acquire()
                owns = True
                self.bindings["${{ needs.desktop-preflight.outputs.macos }}"] = "true"
                (self.root / "dist" / MACOS_BUNDLE).write_bytes(b"signed later")
                results["full"] = self.publish_desktop()
            except Exception as exc:
                results["full_error"] = exc
            finally:
                excluded.set()
                if owns:
                    lock.release()

        linux = threading.Thread(target=run_linux)
        full = threading.Thread(target=run_full)
        linux.start()
        full.start()
        try:
            self.assertTrue(excluded.wait(self_test_hang.HANG_CEILING_SECONDS), results)
            self.assertFalse(results.get("entered_during_linux"),
                             "full publisher entered while linux held the tag group")
        finally:
            proceed.set()
            linux.join(self_test_hang.SUBPROCESS_JOIN_SECONDS)
            full.join(self_test_hang.SUBPROCESS_JOIN_SECONDS)
        self.assertFalse(linux.is_alive() or full.is_alive(), "publisher thread did not finish")
        self.assertNotIn("linux_error", results, results.get("linux_error"))
        self.assertNotIn("full_error", results, results.get("full_error"))
        self.assertEqual(results["linux"].returncode, 0, results["linux"].stderr)
        self.assertNotEqual(results["full"].returncode, 0, results["full"].stdout)
        self.assertNotIn(MACOS_BUNDLE, self.uploaded_names(),
                         "the waiting full plan must refuse before uploading the DMG")
        self.assertNotIn(MACOS_BUNDLE, {path.name for path in self.remote.iterdir()})
        self.assert_desktop_authenticated(LINUX_BUNDLES)
        published = {path.name: path.read_bytes() for path in self.remote.iterdir()}
        # A complete Linux-only retry still uploads nothing and keeps those bytes.
        self.upload_log.write_text("")
        self.bindings["${{ needs.desktop-preflight.outputs.macos }}"] = "false"
        self.assertEqual(self.publish_desktop().returncode, 0)
        self.assertEqual(self.uploaded_names(), [])
        self.assertEqual({path.name: path.read_bytes() for path in self.remote.iterdir()}, published)

    def test_distinct_release_tags_publish_desktop_side_by_side(self):
        # Different groups must not take the same lock: both publishers reach
        # gh release view before either is released, then each signs its own tag.
        self.prepare_desktop()
        self.assertNotEqual(self.desktop_publish_group("v1.2.3"),
                            self.desktop_publish_group("v9.9.9"))
        other = self.root / "other-tag"
        other_remote = self.stage_release_checkout(other)
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.bind(("127.0.0.1", 0))
        listener.listen(2)
        # Accept is a wall-clock wait for publishers that run at idle priority.
        # The connect timeout inside gh starts only after that child is running.
        listener.settimeout(self_test_hang.HANG_CEILING_SECONDS)
        barrier_host, barrier_port = listener.getsockname()
        barrier_address = f"{barrier_host}:{barrier_port}"
        labels = []
        errors = {}

        def serve():
            connections = []
            try:
                for _ in range(2):
                    connection, _ = listener.accept()
                    connections.append(connection)
                    label = bytearray()
                    while not label.endswith(b"\n"):
                        chunk = connection.recv(64)
                        if not chunk:
                            break
                        label.extend(chunk)
                    labels.append(label.rstrip(b"\n").decode())
                for connection in connections:
                    connection.sendall(b"+")
            except Exception as exc:
                errors["barrier"] = exc
            finally:
                for connection in connections:
                    connection.close()
                listener.close()

        def publish(tag, cwd, remote, accept):
            try:
                errors[tag] = self.publish_desktop(
                    cwd=cwd,
                    FAKE_GH_BARRIER=barrier_address,
                    FAKE_GH_BARRIER_LABEL=tag,
                    FAKE_ACCEPT_TAGS=accept,
                    FAKE_RELEASE=str(remote),
                    FAKE_UPLOAD_LOG=str(cwd / "uploads.jsonl"),
                    RELEASE_TAG=tag,
                )
            except Exception as exc:
                errors[tag] = exc

        server = threading.Thread(target=serve)
        server.start()
        first = threading.Thread(target=publish, args=("v1.2.3", self.root, self.remote, "v1.2.3"))
        second = threading.Thread(target=publish, args=("v9.9.9", other, other_remote, "v9.9.9"))
        first.start()
        second.start()
        first.join(self_test_hang.SUBPROCESS_JOIN_SECONDS)
        second.join(self_test_hang.SUBPROCESS_JOIN_SECONDS)
        server.join(self_test_hang.SUBPROCESS_JOIN_SECONDS)
        self.assertFalse(first.is_alive() or second.is_alive() or server.is_alive())
        self.assertNotIn("barrier", errors, errors.get("barrier"))
        self.assertEqual(set(labels), {"v1.2.3", "v9.9.9"},
                         "both tags must be inside gh before either upload continues")
        for tag in ("v1.2.3", "v9.9.9"):
            result = errors[tag]
            self.assertIsInstance(result, subprocess.CompletedProcess, result)
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_desktop_authenticated()
        self.assert_desktop_authenticated(root=other, remote=other_remote, version="9.9.9")

    def test_metadata_accepts_release_and_prerelease_tags(self):
        for tag in ("v0.0.0", "v12.34.56", "v1.2.3-rc.1", "v1.2.3-alpha-beta.2"):
            with self.subTest(tag=tag):
                self.output.write_text("")
                result = self.run_script(self.metadata, GITHUB_REF_NAME=tag,
                                         GITHUB_OUTPUT=str(self.output))
                self.assertEqual(result.returncode, 0, result.stderr)
                outputs = dict(line.split("=", 1) for line in self.output.read_text().splitlines())
                self.assertEqual(outputs, dict(self.outputs, version=tag[1:],
                                               prerelease="true" if "-" in tag else "false"))

    def test_prerelease_release_flag_and_homebrew_formula_gate(self):
        self.output.write_text("")
        result = self.run_script(self.metadata, GITHUB_REF_NAME="v1.2.3-rc.1",
                                 GITHUB_OUTPUT=str(self.output))
        self.assertEqual(result.returncode, 0, result.stderr)
        outputs = dict(line.split("=", 1) for line in self.output.read_text().splitlines())
        self.assertEqual(outputs["prerelease"], "true")

        release = next(step for step in self.jobs["publish-release"]["steps"]
                       if step.get("name") == "Create GitHub Release")
        self.assertEqual(self.jobs["publish-release"]["outputs"].get("prerelease"),
                         "${{ steps.metadata.outputs.prerelease }}")
        # The release step runs inside publish-release, where the job's own
        # `needs` context is unavailable and would resolve to an empty flag.
        self.assertEqual(release["with"].get("prerelease"),
                         "${{ steps.metadata.outputs.prerelease }}",
                         "prerelease tags must create a GitHub Release flagged prerelease")
        self.assertEqual(self.tap.get("if"),
                         "${{ needs.publish-release.outputs.prerelease == 'false' }}",
                         "Homebrew formula must not be rewritten for prerelease tags")

    def test_metadata_rejects_hostile_and_nonrelease_tags_before_outputs(self):
        for tag in ("1.2.3", "v1.2", "v1.2.3.4", "v1.2.3-", "v1.2.3-rc..1",
                    "v1.2.3+build", "v1.2.3\nversion=evil", 'v1.2.3$(touch sentinel)',
                    'v1.2.3`touch sentinel`', 'v1.2.3";touch sentinel;#',
                    'v1.2.3$(touch${IFS}sentinel)', 'v1.2.3`touch${IFS}sentinel`'):
            with self.subTest(tag=tag):
                if "${IFS}" in tag:
                    self.git("check-ref-format", f"refs/tags/{tag}")
                self.output.write_text("")
                result = self.run_script(self.metadata, GITHUB_REF_NAME=tag,
                                         GITHUB_OUTPUT=str(self.output))
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.output.read_text(), "",
                                 "invalid tags must not publish metadata to downstream jobs")
                self.assertFalse((self.root / "sentinel").exists(),
                                 "tag data must never execute as shell source")

    def test_formula_uses_metadata_and_rejects_ruby_and_shell_payloads(self):
        result = self.run_script(self.tap_runs[0])
        self.assertEqual(result.returncode, 0, result.stderr)
        formula = self.root / "tap/Formula/orbit.rb"
        generated = formula.read_text()
        self.assertIn(f'version "{self.outputs["version"]}"', generated)
        for arch in ("arm", "intel"):
            asset = self.outputs[f"darwin_{arch}_asset"]
            self.assertIn(f'/v{self.outputs["version"]}/{asset}"', generated)
            self.assertIn(f'sha256 "{self.outputs[f"darwin_{arch}_sha"]}"', generated)

        for field in self.outputs:
            for payload in ('$(touch sentinel)', '`touch sentinel`', '";system("touch sentinel");#',
                            '#{system("touch sentinel")}', "value\ninjected"):
                with self.subTest(field=field, payload=payload):
                    original = self.outputs[field]
                    self.outputs[field] = payload
                    try:
                        result = self.run_script(self.tap_runs[0])
                    finally:
                        self.outputs[field] = original
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(formula.read_text(), generated,
                                     "unsafe metadata must not overwrite the Ruby formula")
                    self.assertFalse((self.root / "sentinel").exists())

    def test_tap_push_authentication_is_ephemeral_and_commit_message_is_data(self):
        checkout = next(step for step in self.tap["steps"] if "uses" in step)
        self.assertIs(checkout["with"].get("persist-credentials"), False,
                      "tap checkout must not persist its token for formula-generation steps")
        self.git("init", "--initial-branch=main", "tap")
        self.git("init", "--bare", "remote.git")
        self.git("-C", "tap", "remote", "add", "origin", str(self.root / "remote.git"))
        self.git("-C", "tap", "config", "push.default", "current")
        result = self.run_script(self.tap_runs[0])
        self.assertEqual(result.returncode, 0, result.stderr)
        # A real pre-push hook observes the one-command Git config; the
        # remote is local so no credential or network access is required.
        hook = self.root / "tap/.git/hooks/pre-push"
        hook.write_text('#!/bin/sh\nset -eu\n'
                        'git config --get http.https://github.com/.extraheader > ../push-header\n')
        hook.chmod(0o755)
        hostile_version = '1.2.3$(touch sentinel)`touch sentinel`"'
        self.outputs["version"] = hostile_version
        result = self.run_script(self.tap_runs[1])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git("--git-dir=remote.git", "log", "main", "-1", "--format=%s"),
                         f"orbit v{hostile_version}")
        self.assertFalse((self.root / "tap/sentinel").exists(),
                         "commit messages must treat shell metacharacters as literal data")
        authorization = base64.b64encode(b"x-access-token:offline-test-token").decode()
        self.assertEqual((self.root / "push-header").read_text().strip(),
                         f"AUTHORIZATION: basic {authorization}")
        config = self.git("-C", "tap", "config", "--local", "--list")
        self.assertNotIn(authorization, config,
                         "push credentials must not persist in tap/.git/config")
        self.assertNotIn("offline-test-token", config)
        self.assertNotIn("extraheader", config)
        before = self.git("-C", "tap", "rev-parse", "HEAD")
        result = self.run_script(self.tap_runs[1])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git("-C", "tap", "rev-parse", "HEAD"), before)

    def test_slow_step_past_the_removed_cap_completes(self):
        # Sleep stands in for a correct step that loses the CPU under idle
        # scheduling. 25s is past the removed 10s and 20s caps.
        step = {"id": "slow-release-step", "run": f"sleep {self_test_hang.SLOW_FIXTURE_SECONDS}"}
        started = time.monotonic()
        result = self.run_script(step)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertGreater(time.monotonic() - started, 20)

    def test_hung_step_fails_and_names_the_step(self):
        step = {"id": "hung-release-step", "run": "sleep 30"}
        with self.assertRaises(AssertionError) as caught:
            self.run_script(step, timeout=self_test_hang.HANG_PROBE_SECONDS)
        self.assertIn(
            "hung step: hung-release-step "
            f"(command bash exceeded {self_test_hang.HANG_PROBE_SECONDS:g}s)",
            str(caught.exception))


if __name__ == "__main__":
    unittest.main()
