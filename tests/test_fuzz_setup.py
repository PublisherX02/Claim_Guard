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

    def test_the_loaded_profile_is_the_one_selected(self):
        self.assertEqual(settings.get_profile(fs.ACTIVE).max_examples, settings().max_examples)

    @given(fs.hostile_text)
    def test_hostile_text_is_encodable(self, text):
        text.encode('utf-8')

    @given(fs.byte_edits(b'{"a": 1}\n'))
    def test_byte_edits_returns_bytes(self, data):
        self.assertIsInstance(data, bytes)


if __name__ == '__main__':
    unittest.main()
