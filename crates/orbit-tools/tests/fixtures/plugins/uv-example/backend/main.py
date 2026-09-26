"""uv-example backend (`backend.type: exec`, envelope v1).

Imports a third-party package that `uv run` installed from uv.lock, and
reports where that environment lives so a conformance run can check it stayed
under the plugin's state directory.
"""

import json
import os
import sys

import orbit_uv_fixture_dep


def inside(path: str, root: str) -> bool:
    return os.path.realpath(path).startswith(os.path.realpath(root) + os.sep)


def status(_payload: dict) -> dict:
    state = os.environ["ORBIT_PLUGIN_STATE"]
    return {
        "dependency_version": orbit_uv_fixture_dep.__version__,
        "environment_in_plugin_state": inside(sys.prefix, state)
        and inside(orbit_uv_fixture_dep.__file__, state),
        "cache_in_plugin_state": os.path.isdir(os.path.join(state, "uv-cache")),
    }


def main() -> int:
    request = json.load(sys.stdin)
    verb = str(request.get("tool", "")).rsplit(".", 1)[-1]
    if verb != "status":
        response = {
            "ok": False,
            "error": {"code": "unknown_tool", "message": f"no tool '{verb}'"},
        }
    else:
        response = {"ok": True, "output": status(request.get("input") or {})}
    json.dump(response, sys.stdout)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
