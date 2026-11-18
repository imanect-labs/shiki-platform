import copy
from concurrent.futures import ThreadPoolExecutor
from threading import Event, Thread
import unittest

from bench import RESUME_FIELDS, validate_resume
from rustfs_layouts import upload_all


class MeasurementGuardTests(unittest.TestCase):
    def test_resume_rejects_backend_window_and_instrument_changes(self):
        original = dict.fromkeys(RESUME_FIELDS, "same")
        validate_resume(original, original)
        for key in ("backend", "dictionary_window_log", "instrument_sha256"):
            current = copy.deepcopy(original)
            current[key] = "changed"
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, key):
                validate_resume(original, current)

    def test_resume_rejects_untracked_old_conditions(self):
        with self.assertRaisesRegex(ValueError, "backend"):
            validate_resume({}, dict.fromkeys(RESUME_FIELDS, "same"))

    def test_failed_upload_waits_for_inflight_put_before_cleanup(self):
        started, release, cleaned = Event(), Event(), Event()

        def put(item):
            if item == 0:
                if not started.wait(2):
                    raise TimeoutError("second PUT did not start")
                raise RuntimeError("first PUT failed")
            started.set()
            if not release.wait(2):
                raise TimeoutError("test did not release PUT")

        def run():
            with ThreadPoolExecutor(max_workers=2) as pool:
                try:
                    upload_all(pool, put, [0, 1])
                except RuntimeError:
                    pass
                finally:
                    cleaned.set()

        thread = Thread(target=run)
        thread.start()
        self.assertTrue(started.wait(2))
        self.assertFalse(cleaned.wait(0.05))
        release.set()
        thread.join(2)
        self.assertFalse(thread.is_alive())
        self.assertTrue(cleaned.is_set())
