import sqlite3
import json
import subprocess
import sys
import tempfile
import unittest
from contextlib import closing
from pathlib import Path
from unittest.mock import patch

sys.dont_write_bytecode = True
import materials


class MaterialsAdapterTests(unittest.TestCase):
    def test_portable_material_paths_fail_closed_and_keep_account_cache_isolated(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict(materials.os.environ, {"COMMUNITYHERO_RUNTIME_MODE": "portable"}, clear=True):
            with self.assertRaisesRegex(materials.AdapterError, "not configured"):
                materials._conveyor_root(materials.os.environ)
            with self.assertRaisesRegex(materials.AdapterError, "not configured"):
                materials._own_database("likeavto")
            with self.assertRaisesRegex(materials.AdapterError, "not configured"):
                materials._old_connection("likeavto")
            with patch.dict(materials.os.environ, {"COMMUNITYHERO_MATERIALS_DATA_DIR": directory}):
                self.assertEqual(materials._own_database("likeavto"), Path(directory) / "materials-likeavto.sqlite3")
                self.assertEqual(materials._own_database("baw-russia"), Path(directory) / "materials-baw-russia.sqlite3")


    def test_conveyor_root_aliases_reject_conflicts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            self.assertEqual(materials._conveyor_root({"COMMUNITYHERO_CONVEYOR_REPO": str(root)}), root)
            self.assertEqual(materials._conveyor_root({"COMMUNITYHERO_CONVEYOR_ROOT": str(root), "COMMUNITYHERO_CONVEYOR_REPO": str(root)}), root)
            with self.assertRaisesRegex(materials.AdapterError, "aliases conflict"):
                materials._conveyor_root({"COMMUNITYHERO_CONVEYOR_ROOT": str(root), "COMMUNITYHERO_CONVEYOR_REPO": str(root / "other")})

    def test_custom_card_override_is_bound_by_content_without_fallback(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            card, common = root / "custom-card.json", root / "common.json"
            card.write_text(json.dumps({"account": "baw-russia", "tone": "BAW tone"}), encoding="utf-8")
            common.write_text("{}", encoding="utf-8")
            with patch.dict(materials.os.environ, {"COMMUNITYHERO_ACCOUNT_CARD": str(card)}), patch.object(materials, "COMMON", common):
                result = materials._safe_card_materials("baw-russia")
                self.assertEqual(result[0]["account"], "BAW Russia")
                self.assertEqual(result[0]["text"], "BAW tone")
                with self.assertRaisesRegex(materials.AdapterError, "does not match"):
                    materials._safe_card_materials("likeavto")
                card.unlink()
                with self.assertRaisesRegex(materials.AdapterError, "unavailable"):
                    materials._safe_card_materials("baw-russia")


    def test_import_reads_each_account_materials_without_write_access_or_scope_leak(self):
        for account, display in materials.ACCOUNT_NAMES.items():
            with self.subTest(account=account), closing(materials._old_connection(account)) as db:
                self.assertEqual(db.execute("PRAGMA query_only").fetchone()[0], 1)
                with self.assertRaises(sqlite3.OperationalError):
                    db.execute("CREATE TABLE forbidden_materials_test (id INTEGER)")
            result = materials.materials({"account": account, "accountObjectIds": ["test"]})["materials"]
            self.assertTrue(any(item["kind"] == "transcript" for item in result))
            self.assertTrue(any(item["kind"] == "knowledge" for item in result))
            self.assertTrue(any(item["kind"] == "knowledge" and item.get("sourceUrl") for item in result))
            self.assertEqual(len(result), len({item["id"] for item in result}))
            self.assertTrue(all(item["kind"] in {"knowledge", "transcript", "ocr"} for item in result))
            self.assertTrue(all(item["account"] == display for item in result))

    def test_existing_transcript_is_reused_without_transcribing(self):
        with closing(materials._old_connection("likeavto")) as db:
            row = db.execute("""SELECT post_key FROM post_media WHERE account=?
                                AND state='ready' AND transcript IS NOT NULL LIMIT 1""",
                             ("likeavto",)).fetchone()
        self.assertIsNotNone(row)
        object_id = row["post_key"].split(":", 1)[0]
        result = materials.media({"account": "likeavto", "accountObjectIds": [object_id],
                                  "postId": "post-" + row["post_key"]})
        self.assertTrue(result["reused"])
        self.assertEqual(result["materials"][0]["kind"], "transcript")
        self.assertEqual(result["materials"][0]["postKey"], row["post_key"])

    def test_youtube_watch_keeps_only_valid_video_identity(self):
        self.assertEqual(
            materials._public_source("https://www.youtube.com/watch?v=AbCdEf12345&si=tracking"),
            "https://www.youtube.com/watch?v=AbCdEf12345")
        self.assertEqual(materials._public_source("https://youtube.com/watch?si=tracking"), "")
        self.assertEqual(materials._public_source("https://youtube.com/watch?v=bad!id"), "")
        self.assertEqual(materials._public_reference("https://www.suzuki.co.jp/page?token=hidden"),
                         "https://www.suzuki.co.jp/page")
        self.assertEqual(materials._public_reference("https://127.0.0.1/private"), "")

    def test_video_attachment_precedes_parent_post(self):
        key = "11391:unit-test-attachment"
        found = materials._post_source({
            "account": "likeavto", "accountObjectIds": ["11391"],
            "postId": "post-" + key,
            "post": {"id": "post-" + key, "postKey": key, "objectId": "11391",
                     "sourceUrl": "https://vk.com/wall-135891342_47001",
                     "attachments": [{"type": "video", "source_url":
                                      "https://vk.com/clip-135891342_456246880"}]},
        })
        self.assertEqual(found[1], "https://vk.com/clip-135891342_456246880")

    def test_scope_and_private_source_rejected(self):
        with self.assertRaisesRegex(materials.AdapterError, "Account is not configured"):
            materials.materials({"account": "unknown", "accountObjectIds": ["11391"]})
        with self.assertRaisesRegex(materials.AdapterError, "allowlist"):
            materials.materials({"account": "baw-russia"})
        with self.assertRaisesRegex(materials.AdapterError, "postKey is invalid"):
            materials._post_source({"account":"likeavto","accountObjectIds":["11391"],"postId": "post-foreign:1", "post": {
                "id": "post-foreign:1", "postKey": "foreign:1",
                "sourceUrl": "https://youtube.com/watch?v=AbCdEf12345"}})
        with self.assertRaisesRegex(materials.AdapterError, "postKey is invalid"):
            materials._post_source({"account":"likeavto","accountObjectIds":["11391"],"postId": "post-11391:unit-test", "post": {
                "id": "post-11391:unit-test", "postKey": "11391:unit-test", "objectId": "11390",
                "sourceUrl": "https://youtube.com/watch?v=AbCdEf12345"}})
        with self.assertRaisesRegex(materials.AdapterError, "No supported public video source"):
            materials._post_source({"account":"likeavto","accountObjectIds":["11391"],"postId": "post-11391:unit-test", "post": {
                "id": "post-11391:unit-test", "postKey": "11391:unit-test",
                "sourceUrl": "http://127.0.0.1/video"}})

    def test_local_media_cache_is_namespaced_by_account(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(materials, "MVP", Path(directory)):
            for account, transcript in (("likeavto", "LikeAvto words"), ("baw-russia", "BAW words")):
                with closing(materials._own_connection(account)) as db:
                    db.execute("INSERT INTO media VALUES(?,?,?,?,?,?,?)",
                               ("shared:post", "https://youtu.be/AbCdEf12345", "digest", "Title",
                                transcript, "", "2026-09-22T00:00:00Z"))
                    db.commit()
            like = materials._own_materials("likeavto")
            baw = materials._own_materials("baw-russia")
            self.assertEqual(like[0]["text"], "LikeAvto words")
            self.assertEqual(baw[0]["text"], "BAW words")
            self.assertEqual(like[0]["account"], "LikeAvto")
            self.assertEqual(baw[0]["account"], "BAW Russia")

    def test_baw_card_ids_cannot_fall_through_likeavto_import_manifest(self):
        manifest = json.loads((materials.MVP / "server/src/knowledge-import-manifest.json").read_text(encoding="utf-8"))
        reviewed = set(manifest["entries"])
        like_ids = {f'import-{item["id"]}' for item in materials._safe_card_materials("likeavto")}
        baw_ids = {f'import-{item["id"]}' for item in materials._safe_card_materials("baw-russia")}
        self.assertTrue(like_ids & reviewed)
        self.assertFalse(baw_ids & reviewed)
        self.assertTrue(like_ids.isdisjoint(baw_ids))

    def test_youtube_download_uses_one_bounded_cli_process(self):
        class PipelineError(RuntimeError):
            pass

        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)

            def completed(command, **kwargs):
                (work / "source.webm").write_bytes(b"media")
                return subprocess.CompletedProcess(command, 0)

            with patch.object(materials.subprocess, "run", side_effect=completed) as run:
                result = materials._download_youtube_cli(
                    "https://www.youtube.com/watch?v=hBSRrjkCRI8",
                    work,
                    video=False,
                    timeout_seconds=17,
                    error_type=PipelineError,
                )

            self.assertEqual(result, work / "source.webm")
            run.assert_called_once()
            command = run.call_args.args[0]
            self.assertEqual(command[:3], [sys.executable, "-m", "yt_dlp"])
            self.assertIn("--ignore-config", command)
            self.assertIn("--no-playlist", command)
            self.assertIn("bestaudio/best", command)
            self.assertEqual(command[-1], "https://www.youtube.com/watch?v=hBSRrjkCRI8")
            self.assertEqual(run.call_args.kwargs["timeout"], 17)
            self.assertFalse(run.call_args.kwargs["shell"])

    def test_youtube_download_timeout_removes_partial_file(self):
        class PipelineError(RuntimeError):
            pass

        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)

            def timeout(command, **kwargs):
                (work / "source.webm.part").write_bytes(b"partial")
                raise subprocess.TimeoutExpired(command, kwargs["timeout"])

            with patch.object(materials.subprocess, "run", side_effect=timeout):
                with self.assertRaisesRegex(PipelineError, "source_download_timeout"):
                    materials._download_youtube_cli(
                        "https://youtu.be/hBSRrjkCRI8",
                        work,
                        video=False,
                        timeout_seconds=0.25,
                        error_type=PipelineError,
                    )

            self.assertEqual(list(work.iterdir()), [])

    def test_youtube_download_classifies_sanitized_failures_and_cleans_partials(self):
        class PipelineError(RuntimeError):
            pass

        cases = (
            ("ERROR: Sign in to confirm you are not a bot", "source_download_failed_auth"),
            ("ERROR: HTTP Error 429: Too Many Requests", "source_download_failed_rate_limited"),
            ("ERROR: Video unavailable", "source_download_failed_unavailable"),
            ("ERROR: connection timed out", "source_download_timeout"),
            ("ERROR: extractor failed for https://example.invalid/private", "source_download_failed"),
        )
        for stderr, expected in cases:
            with self.subTest(expected=expected), tempfile.TemporaryDirectory() as directory:
                work = Path(directory)
                (work / "source.webm.part").write_bytes(b"partial")
                failed = subprocess.CompletedProcess([], 1, stderr=stderr)
                with patch.object(materials.subprocess, "run", return_value=failed):
                    with self.assertRaisesRegex(PipelineError, f"^{expected}$"):
                        materials._download_youtube_cli(
                            "https://www.youtube.com/watch?v=hBSRrjkCRI8",
                            work,
                            video=False,
                            timeout_seconds=17,
                            error_type=PipelineError,
                        )
                self.assertEqual(list(work.iterdir()), [])

    def test_youtube_download_success_without_output_is_source_file_missing(self):
        class PipelineError(RuntimeError):
            pass

        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            completed = subprocess.CompletedProcess([], 0, stderr="")
            with patch.object(materials.subprocess, "run", return_value=completed):
                with self.assertRaisesRegex(PipelineError, "^source_file_missing$"):
                    materials._download_youtube_cli(
                        "https://www.youtube.com/watch?v=hBSRrjkCRI8",
                        work,
                        video=False,
                        timeout_seconds=17,
                        error_type=PipelineError,
                    )

    def test_adapter_routes_only_youtube_around_legacy_worker(self):
        calls = []

        class PipelineError(RuntimeError):
            pass

        class LegacyTranscriber:
            download_timeout_seconds = 9

            def _download(self, source_url, work, *, video=False):
                calls.append(("legacy", source_url, video))
                return work / "legacy.webm"

        adapted = materials._adapter_transcriber_class(LegacyTranscriber, PipelineError)()
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            with patch.object(materials, "_download_youtube_cli",
                              return_value=work / "youtube.webm") as download:
                youtube = adapted._download(
                    "https://www.youtube.com/watch?v=hBSRrjkCRI8", work)
                vk = adapted._download("https://vk.com/clip-1_2", work, video=True)

        self.assertEqual(youtube.name, "youtube.webm")
        self.assertEqual(vk.name, "legacy.webm")
        download.assert_called_once_with(
            "https://www.youtube.com/watch?v=hBSRrjkCRI8",
            work,
            video=False,
            timeout_seconds=9,
            error_type=PipelineError,
        )
        self.assertEqual(calls, [("legacy", "https://vk.com/clip-1_2", True)])


if __name__ == "__main__":
    unittest.main()
