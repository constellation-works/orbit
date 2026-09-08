"""Deterministic evidence-classification tests; no kernel capability required."""

import errno
import json
import unittest

import live_read_probe as probe
from live_read_fixture import paired_controls


def record(*outcomes, exit_code=0, stderr=""):
    return {"exit_code": exit_code, "stdout": "\n".join(map(json.dumps, outcomes)),
            "stderr": stderr}


class ClassificationTests(unittest.TestCase):
    def test_only_permission_denial_passes(self):
        for number in [errno.EACCES, errno.EPERM, errno.ENOENT, errno.EIO]:
            with self.subTest(errno=number):
                result = probe.classify(record({"outcome": "denied", "errno": number}), "deny")
                self.assertEqual(result["status"],
                                 "pass" if number in [errno.EACCES, errno.EPERM] else "fail")

    def test_leak_survives_crash_or_malformed_output(self):
        for output in [probe.MARKER, json.dumps({"outcome": "read", "content": probe.MARKER})]:
            for code in [0, 1, -9]:
                with self.subTest(output=output, code=code):
                    result = probe.classify({"stdout": output, "stderr": "", "exit_code": code}, "deny")
                    self.assertEqual(result["status"], "fail")

    def test_timeout_after_leak_is_failure(self):
        result = probe.classify({"status": "unavailable", "reason": "timeout",
                                 "stdout": probe.MARKER}, "deny")
        self.assertEqual(result["status"], "fail")

    def test_paired_controls_require_an_executed_leaking_baseline(self):
        candidate = {"probes": {"outside": {"status": "pass"}}}
        self.assertEqual(paired_controls({}, candidate)["outside"], "unavailable")
        baseline = {"probes": {"outside": record({"outcome": "read", "content": probe.MARKER})}}
        self.assertEqual(paired_controls(baseline, candidate)["outside"], "pass")
        baseline["probes"]["outside"]["exit_code"] = 1
        self.assertEqual(paired_controls(baseline, candidate)["outside"], "unavailable")

    def test_stderr_leak_overrides_claimed_denial(self):
        result = probe.classify(record({"outcome": "denied", "errno": errno.EACCES},
                                       stderr=probe.MARKER), "deny")
        self.assertEqual(result["status"], "fail")

    def test_missing_or_malformed_body_is_unavailable(self):
        for output in ["", "null", "[]", "3", "garbage"]:
            with self.subTest(output=output):
                result = probe.classify({"stdout": output, "stderr": "", "exit_code": 0}, "deny")
                self.assertEqual(result["status"], "unavailable")

    def test_setup_failure_and_descendant_failure_are_not_passes(self):
        denied = {"outcome": "denied", "errno": errno.EACCES}
        for outcome in [{"outcome": "setup_failed", "errno": errno.EACCES},
                        {"outcome": "descendant_complete", "wait_status": 9}]:
            self.assertEqual(probe.classify(record(denied, outcome), "deny")["status"], "unavailable")
        self.assertEqual(probe.classify(record(denied, {"outcome": "descendant_complete",
                                                      "wait_status": 0}), "deny")["status"], "pass")

    def test_generated_content_is_required(self):
        for content in ["", probe.MARKER, probe.GENERATED]:
            self.assertEqual(probe.classify(record({"outcome": "read", "content": content}),
                                            "allow")["status"],
                             "pass" if content == probe.GENERATED else "fail")

    def test_unavailable_is_preserved_for_every_expectation(self):
        for expectation in ["deny", "allow", "positive", "observation"]:
            self.assertEqual(probe.classify({"status": "unavailable"}, expectation)["status"],
                             "unavailable")

    def test_observations_cannot_hide_stronger_contract_failures(self):
        observed = probe.classify(record({"outcome": "read", "content": probe.MARKER}), "observation")
        results = probe.contract_assessment({"rename_mmap": observed, "rename_cached": observed})
        self.assertEqual(results["later_access_revocation"], "fail")
        self.assertEqual(results["previously_acquired_bytes"], "fail")
        self.assertEqual(results["pathname_acquisition"], "unavailable")
        self.assertEqual(observed["status"], "observed")

    def test_unknown_expectation_is_rejected(self):
        with self.assertRaises(ValueError):
            probe.classify(record(), "typo")


if __name__ == "__main__":
    unittest.main()
