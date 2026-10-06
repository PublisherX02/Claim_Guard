"""The in-memory queue store passes the shared contract (the MongoDB store passes the same one, in test_wq_store_mongo.py)."""
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'src'))
sys.path.insert(0, str(ROOT / 'tests'))
from queue_store_contract import StoreContract
from workqueue.store import MemoryQueueStore


class MemoryQueueStoreContract(StoreContract, unittest.TestCase):
    def make_store(self):
        return MemoryQueueStore()


if __name__ == '__main__':
    unittest.main()
