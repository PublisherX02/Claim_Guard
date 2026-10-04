import os
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import fuzz_strategies as fs
from hypothesis import given, settings


class FoundationTests(unittest.TestCase):
    def test_ci_profile_is_deterministic_and_has_no_deadline(self):
        s = settings.get_profile('ci')
        self.assertTrue(s.derandomize)
        self.assertIsNone(s.deadline)

    def test_default_profile_is_ci(self):
        if 'FUZZ_PROFILE' not in os.environ:
            self.assertEqual(settings().max_examples, settings.get_profile('ci').max_examples)

    @given(fs.hostile_text)
    def test_hostile_text_is_encodable(self, text):
        text.encode('utf-8')

    @given(fs.byte_edits(b'{"a": 1}\n'))
    def test_byte_edits_returns_bytes(self, data):
        self.assertIsInstance(data, bytes)


if __name__ == '__main__':
    unittest.main()
