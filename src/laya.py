import contextlib
import json
import sys

with contextlib.redirect_stdout(sys.stderr):
    import laya

    router = laya.Router()

questions = {
    "complexity": {
        "type": "choice",
        "instructions": "Choose the coding plan's complexity based on the request and project context.",
        "criteria": {
            "simple": "A focused, clear change in one component with no architectural decisions.",
            "complex": "Changes across components, dependencies, migrations or unclear requirements.",
            "demanding": "Major architecture changes, security-critical design or difficult distributed systems.",
        },
    }
}

for line in sys.stdin:
    try:
        request = json.loads(line)
        with contextlib.redirect_stdout(sys.stderr):
            result = router.predict(
                request["state"], questions, model="multilingual", max_len=8192
            )
        print(json.dumps(result["answers"]["complexity"]), flush=True)
    except Exception as error:
        print(json.dumps({"error": str(error)}), flush=True)
