#!/usr/bin/env python3
"""Positive and negative fixtures for the CodeQL extension schema gate."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent
REPO_ROOT = SCRIPTS.parent
CHECKER_PATH = SCRIPTS / "check-codeql-extension-schema.py"

_SPEC = importlib.util.spec_from_file_location("check_codeql_extension_schema", CHECKER_PATH)
if _SPEC is None or _SPEC.loader is None:
    raise RuntimeError(f"unable to load {CHECKER_PATH}")
CHECK = importlib.util.module_from_spec(_SPEC)
sys.modules["check_codeql_extension_schema"] = CHECK
_SPEC.loader.exec_module(CHECK)


PACK_YML = """name: constellation-works/orbit-rust-path-validation
version: 0.0.1
library: true

extensionTargets:
  codeql/rust-all: "*"

dataExtensions:
  - path-validation.yml
"""

TWO_VALID_ROWS = """extensions:
  - addsTo:
      pack: codeql/rust-all
      extensible: barrierModel
    data:
      - [
          "orbit_store::driver::file::friction_store::validated_friction_root",
          "ReturnValue.Field[core::result::Result::Ok(0)]",
          "path-injection",
          "manual",
        ]
      - [
          "orbit_cmd::diagnostics::validated_diagnostics_file_path",
          "ReturnValue.Field[core::result::Result::Ok(0)]",
          "path-injection",
          "manual",
        ]
"""

FIVE_FIELD_MERGED_ROW = """extensions:
  - addsTo:
      pack: codeql/rust-all
      extensible: barrierModel
    data:
      - [
          "orbit_store::driver::file::friction_store::validated_friction_root",
          "orbit_cmd::diagnostics::validated_diagnostics_file_path",
          "ReturnValue.Field[core::result::Result::Ok(0)]",
          "path-injection",
          "manual",
        ]
"""

MALFORMED_YAML = """extensions:
  - addsTo:
      pack: codeql/rust-all
      extensible: barrierModel
    data:
      - [
          "crate::sanitize",
"""

UNSUPPORTED_PREDICATE = """extensions:
  - addsTo:
      pack: codeql/rust-all
      extensible: notAModel
    data:
      - [
          "crate::sanitize",
          "ReturnValue",
          "path-injection",
          "manual",
        ]
"""

UNSUPPORTED_PACK = """extensions:
  - addsTo:
      pack: codeql/java-all
      extensible: barrierModel
    data:
      - [
          "crate::sanitize",
          "ReturnValue",
          "path-injection",
          "manual",
        ]
"""

INVALID_ROW_TYPES = """extensions:
  - addsTo:
      pack: codeql/rust-all
      extensible: barrierModel
    data:
      - [
          "crate::sanitize",
          "ReturnValue",
          true,
          "manual",
        ]
"""


class CodeqlExtensionSchemaTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="orbit-codeql-schema-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def write_pack(self, data_yml, pack_yml=None):
        pack_dir = self.root / ".github/codeql/extensions/orbit-rust-path-validation"
        pack_dir.mkdir(parents=True)
        (pack_dir / "codeql-pack.yml").write_text(pack_yml or PACK_YML, encoding="utf-8")
        (pack_dir / "path-validation.yml").write_text(data_yml, encoding="utf-8")
        return pack_dir / "path-validation.yml"

    def errors(self):
        return CHECK.validate_root(self.root)

    def test_two_valid_four_field_rows_pass(self):
        self.write_pack(TWO_VALID_ROWS)
        self.assertEqual(self.errors(), [])

    def test_five_field_merged_barrier_reports_file_predicate_and_row(self):
        data_file = self.write_pack(FIVE_FIELD_MERGED_ROW)
        errors = self.errors()
        self.assertEqual(len(errors), 1, errors)
        message = errors[0]
        self.assertIn(data_file.relative_to(self.root).as_posix(), message)
        self.assertIn("barrierModel", message)
        self.assertIn("row 1", message)
        self.assertIn("expected 4 fields", message)
        self.assertIn("found 5", message)

    def test_malformed_yaml_fails_clearly(self):
        data_file = self.write_pack(MALFORMED_YAML)
        errors = self.errors()
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("malformed YAML", errors[0])
        self.assertIn(data_file.relative_to(self.root).as_posix(), errors[0])

    def test_unsupported_predicate_fails_clearly(self):
        self.write_pack(UNSUPPORTED_PREDICATE)
        errors = self.errors()
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("unsupported extensible predicate 'notAModel'", errors[0])
        self.assertIn("codeql/rust-all", errors[0])

    def test_unsupported_pack_fails_clearly(self):
        self.write_pack(UNSUPPORTED_PACK)
        errors = self.errors()
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("unsupported extension pack 'codeql/java-all'", errors[0])

    def test_invalid_row_value_types_fail_clearly(self):
        self.write_pack(INVALID_ROW_TYPES)
        errors = self.errors()
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("barrierModel row 1", errors[0])
        self.assertIn("kind", errors[0])
        self.assertIn("expected a non-empty string", errors[0])
        self.assertIn("boolean", errors[0])

    def test_current_extension_files_pass(self):
        self.assertEqual(CHECK.validate_root(REPO_ROOT), [])


if __name__ == "__main__":
    unittest.main()
