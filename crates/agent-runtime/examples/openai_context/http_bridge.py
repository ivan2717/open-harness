"""Bounded standard-library transport for the opt-in OpenAI evaluation.

Credentials are inherited through OPENAI_API_KEY, never argv or report fields.
Only the official API origin is allowed; redirects/retries are disabled.
"""

import json
import os
import re
import sys
import urllib.error
import urllib.request


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def main():
    key = os.environ.get("OPENAI_API_KEY", "")
    if not key:
        raise ValueError("OPENAI_API_KEY is required")
    envelope = json.load(sys.stdin)
    if envelope["operation"] == "models":
        path, body = "/models", None
    elif envelope["operation"] == "respond":
        path, body = "/responses", json.dumps(envelope["body"]).encode()
    else:
        raise ValueError("unknown operation")
    request = urllib.request.Request(
        "https://api.openai.com/v1" + path,
        data=body,
        headers={"Authorization": "Bearer " + key, "Content-Type": "application/json"},
    )
    try:
        with urllib.request.build_opener(NoRedirect).open(request, timeout=180) as response:
            result = json.load(response)
        print(json.dumps(result))
    except urllib.error.HTTPError as error:
        # Return only typed error metadata; never provider-echoed prompt/key text.
        try:
            detail = json.loads(error.read()).get("error", {})
        except (ValueError, AttributeError):
            detail = {}
        safe = {}
        for field in ("type", "code", "param"):
            value = detail.get(field)
            if isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9_.\[\]-]{1,120}", value):
                safe[field] = value
        print(json.dumps({"transport_error": json.dumps({"error": safe}), "http_status": error.code}))
    except Exception as error:
        print(json.dumps({"transport_error": type(error).__name__}))


if __name__ == "__main__":
    main()
