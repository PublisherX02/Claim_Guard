import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from access import store as s
from access_store_contract import StoreContract


class MemoryStoreContract(StoreContract, unittest.TestCase):
    def make_store(self):
        return s.MemoryStore()


if __name__ == '__main__':
    unittest.main()
