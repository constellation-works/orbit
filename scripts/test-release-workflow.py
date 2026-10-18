#!/usr/bin/env python3
"""Execute release publication and Homebrew scripts offline with hostile inputs."""

import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import textwrap
import unittest

try:
    import yaml
except ImportError:
    yaml = None


ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github/workflows/release.yml"
DESKTOP_BUNDLES = (
    "orbit-desktop-universal-apple-darwin.dmg",
    "orbit-desktop-x86_64-unknown-linux-gnu.deb",
    "orbit-desktop-x86_64-unknown-linux-gnu.AppImage",
    "orbit-desktop-aarch64-unknown-linux-gnu.deb",
    "orbit-desktop-aarch64-unknown-linux-gnu.AppImage",
)
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
        self.jobs = yaml.safe_load(WORKFLOW.read_text())["jobs"]
        self.metadata = next(step for step in self.jobs["publish-release"]["steps"]
                             if step.get("id") == "metadata")
        self.tap = self.jobs["bump-homebrew-tap"]
        self.tap_runs = [step for step in self.tap["steps"] if "run" in step]
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

    def run_script(self, step, **values):
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
            else:
                self.fail(f"unsupported workflow env binding: {key}")
        env.update(values)
        return subprocess.run(["bash", "-e", "-o", "pipefail", "-c", step["run"]],
                              cwd=self.root, env=env, text=True, capture_output=True, timeout=10)

    def git(self, *arguments):
        result = subprocess.run(["git", *arguments], cwd=self.root, env=self.env,
                                text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip()

    def prepare_desktop(self):
        self.remote = self.root / "release"
        self.remote.mkdir()
        self.upload_log = self.root / "uploads.jsonl"
        self.signing_key = self.root / "signing-key.pem"
        public_key = self.root / "npm/release-signing.pub"
        public_key.parent.mkdir()
        for args in (("genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256",
                      "-out", str(self.signing_key)),
                     ("pkey", "-in", str(self.signing_key), "-pubout", "-out", str(public_key))):
            result = subprocess.run(["openssl", *args], capture_output=True, text=True, timeout=10)
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
            assert tag == "v1.2.3", args
            options = args[3:]
            def option(name):
                index = options.index(name)
                value = options[index + 1]
                del options[index:index + 2]
                return value
            assert option("--repo") == "offline/orbit", args
            if command == "view":
                assert option("--json") == "assets", args
                assert option("--jq") == ".assets[].name", args
                assert not options, args
                print("\\n".join(sorted(p.name for p in remote.iterdir())))
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
                        GITHUB_REF_NAME="v1.2.3", GITHUB_REPOSITORY="offline/orbit")

    def publish_desktop(self):
        for step in self.jobs["publish-desktop"]["steps"]:
            if "run" in step:
                result = self.run_script(step)
                if result.returncode:
                    return result
        return result

    def uploaded_names(self):
        return [json.loads(line) for line in self.upload_log.read_text().splitlines()] \
            if self.upload_log.exists() else []

    def assert_desktop_authenticated(self):
        result = subprocess.run(
            ["openssl", "dgst", "-sha256", "-verify", str(self.root / "npm/release-signing.pub"),
             "-signature", str(self.remote / DESKTOP_SIGNATURE), str(self.remote / DESKTOP_MANIFEST)],
            text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        digests = {name: digest for digest, name in
                   (line.split("  ") for line in
                    (self.remote / DESKTOP_MANIFEST).read_text().splitlines())}
        self.assertEqual(set(digests), {*DESKTOP_BUNDLES, DESKTOP_VERSION})
        for name, digest in digests.items():
            self.assertEqual(digest, hashlib.sha256((self.remote / name).read_bytes()).hexdigest(),
                             f"signed manifest must authenticate published {name}")
        self.assertEqual((self.remote / DESKTOP_VERSION).read_text(), "1.2.3\n")

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


if __name__ == "__main__":
    unittest.main()
