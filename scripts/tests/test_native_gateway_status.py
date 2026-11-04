"""Shutdown observation contract; no installed Gateway or service is touched."""

import importlib.util
from pathlib import Path
import unittest
from unittest.mock import Mock, call, patch

SOURCE = Path(__file__).resolve().parents[1] / "native_gateway_status.py"
spec = importlib.util.spec_from_file_location("native_gateway_status", SOURCE)
gateway = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gateway)

STOPPING = {"running": True, "desiredRunning": False}
STOPPED = {"running": False, "desiredRunning": False}


class ShutdownTests(unittest.TestCase):
    def test_completed_stop_needs_no_poll(self):
        oc_json = Mock(return_value=STOPPED)
        self.assertEqual(gateway.stop_gateway(oc_json, "fixture"), STOPPED)
        oc_json.assert_called_once_with("service stop", "service", "stop", "fixture")

    def test_acknowledged_stop_can_still_be_shutting_down(self):
        accepted = dict(STOPPING, warnings=["gateway is still shutting down"])
        oc_json = Mock(side_effect=[accepted, STOPPING, STOPPED])
        with patch.object(gateway.time, "monotonic", side_effect=[0, 1, 2]), \
                patch.object(gateway.time, "sleep"):
            self.assertEqual(gateway.stop_gateway(oc_json, "fixture"), STOPPED)
        self.assertEqual(oc_json.call_args_list, [
            call("service stop", "service", "stop", "fixture"),
            call("stopping status", "service", "status", "fixture", timeout=29),
            call("stopping status", "service", "status", "fixture", timeout=28),
        ])

    def test_running_intent_must_stay_cleared(self):
        running_intent = {"running": False, "desiredRunning": True}
        for responses in [[running_intent], [STOPPING, running_intent]]:
            with self.subTest(responses=responses), \
                    patch.object(gateway.time, "monotonic", return_value=0), \
                    self.assertRaisesRegex(AssertionError, "running intent"):
                gateway.stop_gateway(Mock(side_effect=responses), "fixture")

    def test_a_gateway_that_never_stops_fails_at_the_deadline(self):
        oc_json = Mock(return_value=STOPPING)
        with patch.object(gateway.time, "monotonic", side_effect=[0, 1, 31]), \
                patch.object(gateway.time, "sleep"), \
                self.assertRaisesRegex(AssertionError, "still running 30 seconds"):
            gateway.stop_gateway(oc_json, "fixture")
        self.assertEqual(oc_json.call_count, 2)

    def test_status_failures_propagate(self):
        oc_json = Mock(side_effect=[STOPPING, RuntimeError("status failed")])
        with self.assertRaisesRegex(RuntimeError, "status failed"):
            gateway.stop_gateway(oc_json, "fixture")


if __name__ == "__main__":
    unittest.main()
