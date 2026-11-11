import base64
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import tarfile
import unittest
import zipfile
from unittest.mock import patch
from urllib.error import HTTPError


spec = importlib.util.spec_from_file_location("release", Path(__file__).with_name("release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)

CHECKSUMS = {
    "bonsai-darwin-arm64.tar.gz": "a" * 64,
    "bonsai-darwin-amd64.tar.gz": "b" * 64,
    "bonsai-linux-arm64.tar.gz": "c" * 64,
    "bonsai-linux-amd64.tar.gz": "d" * 64,
    "bonsai-windows-amd64.zip": "e" * 64,
}


def http_error(status):
    return HTTPError("https://example.test", status, "failed", {}, None)


def crates_response(name, version, repository):
    body = json.dumps({"version": {"crate": name, "num": version, "repository": repository}})
    return io.BytesIO(body.encode())


class HomebrewTests(unittest.TestCase):
    def test_checksums_accept_binary_markers_and_blank_lines(self):
        self.assertEqual(
            release.parse_checksums(f"{'a' * 64}  bonsai.tar.gz\n\n{'b' * 64} *bonsai.zip\n"),
            {"bonsai.tar.gz": "a" * 64, "bonsai.zip": "b" * 64},
        )

    def test_formula_installs_the_prebuilt_binary_for_each_platform(self):
        formula = release.homebrew_formula("v0.7.0", CHECKSUMS)
        self.assertIn('version "0.7.0"', formula)
        for archive, digest in CHECKSUMS.items():
            if archive.endswith(".zip"):
                self.assertNotIn(archive, formula)
                continue
            self.assertIn(f"{release.REPOSITORY_URL}/releases/download/v0.7.0/{archive}", formula)
            self.assertIn(f'sha256 "{digest}"', formula)
        self.assertIn('generate_completions_from_executable(bin/"bonsai", "completions")', formula)
        self.assertIn('shell_output("#{bin}/bonsai --version")', formula)
        self.assertIn('eval "$(bonsai init zsh)"', formula)

    def test_formula_needs_every_archive(self):
        partial = dict(CHECKSUMS)
        del partial["bonsai-linux-arm64.tar.gz"]
        with self.assertRaisesRegex(ValueError, "bonsai-linux-arm64.tar.gz"):
            release.homebrew_formula("v0.7.0", partial)

    def test_homebrew_update_is_idempotent(self):
        formula = release.homebrew_formula("v0.7.0", CHECKSUMS)
        current = {"content": base64.b64encode(formula.encode()).decode(), "sha": "abc"}
        with patch.dict(release.os.environ, {"GH_TOKEN": "token"}), patch.object(
            release, "release_checksums", return_value=CHECKSUMS
        ), patch.object(release, "api", return_value=current) as api:
            release.update_homebrew("v0.7.0", "owner/homebrew-tap")
        api.assert_called_once_with(
            "GET", "/repos/owner/homebrew-tap/contents/Formula/bonsai.rb", allow_missing=True
        )

    def test_homebrew_update_replaces_an_older_formula(self):
        older = {"content": base64.b64encode(b"old").decode(), "sha": "abc"}
        with patch.dict(release.os.environ, {"GH_TOKEN": "token"}), patch.object(
            release, "release_checksums", return_value=CHECKSUMS
        ), patch.object(release, "api", side_effect=[older, None]) as api:
            release.update_homebrew("v0.7.0", "owner/homebrew-tap")
        method, path, body = api.call_args.args
        self.assertEqual((method, path), ("PUT", "/repos/owner/homebrew-tap/contents/Formula/bonsai.rb"))
        self.assertEqual(body["sha"], "abc")
        self.assertEqual(body["message"], "chore: update bonsai to 0.7.0")
        self.assertIn(b'version "0.7.0"', base64.b64decode(body["content"]))

    def test_homebrew_update_needs_a_token(self):
        with patch.dict(release.os.environ, {"GH_TOKEN": ""}):
            with self.assertRaisesRegex(SystemExit, "HOMEBREW_TAP_TOKEN"):
                release.update_homebrew("v0.7.0", "owner/homebrew-tap")


class BuildTests(unittest.TestCase):
    def test_github_release_checksums_every_archive_and_excludes_other_files(self):
        with tempfile.TemporaryDirectory() as directory:
            dist = Path(directory)
            archives = [f"bonsai-{platform}-{arch}{'.zip' if platform == 'windows' else '.tar.gz'}"
                        for platform in ("linux", "darwin", "windows") for arch in ("amd64", "arm64")]
            for name in archives:
                (dist / name).write_bytes(name.encode())
            (dist / "SHA256SUMS").write_text("old checksums")
            (dist / "notes.txt").write_text("not a release asset")
            with patch.dict(release.os.environ, {"GITHUB_REPOSITORY": "owner/bonsai", "GITHUB_SHA": "abc"}), patch.object(
                release.subprocess, "run"
            ) as run:
                release.publish_github("0.8.0", dist)
            self.assertEqual(release.parse_checksums((dist / "SHA256SUMS").read_text()), {
                name: hashlib.sha256(name.encode()).hexdigest() for name in archives
            })
            self.assertEqual(run.call_args.args[0], [
                "gh", "release", "create", "v0.8.0", "--repo", "owner/bonsai", "--target", "abc",
                "--generate-notes", *(str(dist / name) for name in sorted(archives)), str(dist / "SHA256SUMS"),
            ])

    def test_github_release_requires_all_six_platforms(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "six release archives"):
                release.publish_github("0.8.0", Path(directory))

    def test_version_follows_conventional_commits(self):
        cases = [
            ("fix: repair cleanup", "0.7.4"),
            ("feat(hq): add search", "0.8.0"),
            ("feat!: replace config", "1.0.0"),
            ("fix: update API\n\nBREAKING CHANGE: remove aliases", "1.0.0"),
        ]
        for log, expected in cases:
            with self.subTest(log=log):
                self.assertEqual(release.next_version("v0.7.3", log), expected)

    def test_archives_contain_only_the_verified_binary(self):
        for platform, binary_name, suffix in [
            ("linux-amd64", "bonsai", ".tar.gz"),
            ("windows-arm64", "bonsai.exe", ".zip"),
        ]:
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                binary = root / "target/test-target/release" / binary_name
                binary.parent.mkdir(parents=True)
                binary.write_bytes(b"binary")
                with patch.object(release.subprocess, "check_output", return_value="bonsai 0.8.0\n"):
                    archive = release.archive_binary("0.8.0", "test-target", f"bonsai-{platform}", root)
                self.assertEqual(archive.suffixes, [".tar", ".gz"] if suffix == ".tar.gz" else [".zip"])
                if suffix == ".zip":
                    with zipfile.ZipFile(archive) as contents:
                        self.assertEqual(contents.namelist(), [binary_name])
                else:
                    with tarfile.open(archive) as contents:
                        self.assertEqual(contents.getnames(), [binary_name])

    def test_archive_rejects_wrong_version(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(
            release.subprocess, "check_output", return_value="bonsai 0.1.1\n"
        ):
            with self.assertRaisesRegex(ValueError, "expected bonsai 0.8.0"):
                release.archive_binary("0.8.0", "test-target", "bonsai-darwin-arm64", Path(directory))


class CrateTests(unittest.TestCase):
    def manifests(self, directory, requirement='package = "bonsai-lib", path = "bonsai-lib", version = "0.1.1"'):
        root = Path(directory)
        (root / "bonsai-lib").mkdir()
        (root / "Cargo.toml").write_text(
            '[package]\nname = "bonsai-cli"\nversion = "0.1.1"\n'
            f"[dependencies]\nanyhow = {{ version = \"1.0.104\" }}\nbonsai = {{ {requirement} }}\n"
        )
        (root / "bonsai-lib/Cargo.toml").write_text(
            '[package]\nname = "bonsai-lib"\nversion = "0.1.1"\n'
            '[dependencies]\nserde = { version = "1.0.229" }\n'
        )
        return root

    def test_stamp_sets_both_versions_and_the_library_requirement(self):
        with tempfile.TemporaryDirectory() as directory:
            root = self.manifests(directory)
            release.stamp("0.7.0", root)
            cli = (root / "Cargo.toml").read_text()
            library = (root / "bonsai-lib/Cargo.toml").read_text()
        self.assertIn('version = "0.7.0"\n[dependencies]', cli)
        self.assertIn('path = "bonsai-lib", version = "0.7.0"', cli)
        self.assertIn('anyhow = { version = "1.0.104" }', cli)
        self.assertIn('name = "bonsai-lib"\nversion = "0.7.0"', library)
        self.assertIn('serde = { version = "1.0.229" }', library)

    def test_stamp_rejects_prereleases_and_unexpected_manifests(self):
        with tempfile.TemporaryDirectory() as directory:
            root = self.manifests(directory)
            with self.assertRaisesRegex(ValueError, "major.minor.patch"):
                release.stamp("0.7.0-rc.1", root)
        with tempfile.TemporaryDirectory() as directory:
            root = self.manifests(directory, requirement='package = "bonsai-lib", version = "0.1.1"')
            with self.assertRaisesRegex(ValueError, "by path and version"):
                release.stamp("0.7.0", root)

    def test_registry_lookup_tells_missing_ours_and_foreign_apart(self):
        with patch.object(release, "urlopen", side_effect=http_error(404)):
            self.assertFalse(release.crate_published("bonsai-lib", "0.7.0"))
        with patch.object(release, "urlopen", side_effect=http_error(500)):
            with self.assertRaises(HTTPError):
                release.crate_published("bonsai-lib", "0.7.0")
        ours = crates_response("bonsai-lib", "0.7.0", release.REPOSITORY_URL)
        with patch.object(release, "urlopen", return_value=ours):
            self.assertTrue(release.crate_published("bonsai-lib", "0.7.0"))
        foreign = crates_response("bonsai-lib", "0.7.0", "https://github.com/someone/else")
        with patch.object(release, "urlopen", return_value=foreign):
            with self.assertRaisesRegex(ValueError, "another repository"):
                release.crate_published("bonsai-lib", "0.7.0")

    def test_publishing_skips_a_published_version(self):
        with patch.object(release, "crate_published", return_value=True):
            commands = []
            release.publish_crate("0.7.0", run=lambda *args, **kwargs: commands.append(args))
        self.assertEqual(commands, [])

    def test_publishing_needs_a_token(self):
        with patch.object(release, "crate_published", return_value=False), patch.dict(
            release.os.environ, {"CARGO_REGISTRY_TOKEN": ""}
        ):
            with self.assertRaisesRegex(SystemExit, "CARGO_REGISTRY_TOKEN"):
                release.publish_crate("0.7.0", run=lambda *args, **kwargs: None)

    def test_publishing_stamps_then_publishes_only_the_library(self):
        commands = []
        with patch.object(release, "crate_published", return_value=False), patch.dict(
            release.os.environ, {"CARGO_REGISTRY_TOKEN": "token"}
        ), patch.object(release, "stamp") as stamp:
            release.publish_crate("0.7.0", run=lambda command, **kwargs: commands.append(command))
        stamp.assert_called_once_with("0.7.0")
        self.assertEqual(commands, [
            ["cargo", "update", "--workspace"],
            ["cargo", "publish", "--locked", "--allow-dirty", "--package", "bonsai-lib"],
        ])


if __name__ == "__main__":
    unittest.main()
