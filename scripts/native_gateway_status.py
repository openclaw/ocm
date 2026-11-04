"""Observe asynchronous shutdown in disposable native Gateway proofs."""

import time


def stop_gateway(oc_json, env_name):
    status = oc_json("service stop", "service", "stop", env_name)
    deadline = time.monotonic() + 30
    while True:
        assert not status["desiredRunning"], "stop did not clear running intent"
        if not status["running"]:
            return status
        remaining = deadline - time.monotonic()
        assert remaining > 0, "Gateway still running 30 seconds after stop"
        status = oc_json(
            "stopping status", "service", "status", env_name, timeout=remaining,
        )
        if status["running"]:
            time.sleep(0.1)
