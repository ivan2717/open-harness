"""Small standard-library transport for the opt-in Rust live evaluation.

The key is inherited from DEEPSEEK_API_KEY, never passed in argv or printed.
Only the official HTTPS endpoint is allowed; redirects and automatic retries
are disabled. This is a bounded evaluation transport, not a production client.
"""

import json
import os
import sys
import urllib.error
import urllib.request


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def main():
    key = os.environ.get("DEEPSEEK_API_KEY", "")
    if not key:
        raise ValueError("DEEPSEEK_API_KEY is required")
    envelope = json.load(sys.stdin)
    operation = envelope["operation"]
    if operation == "models":
        path, body = "/models", None
    elif operation == "chat":
        path, body = "/chat/completions", json.dumps(envelope["body"]).encode()
    else:
        raise ValueError("unknown operation")
    request = urllib.request.Request(
        "https://api.deepseek.com" + path,
        data=body,
        headers={"Authorization": "Bearer " + key, "Content-Type": "application/json"},
    )
    try:
        opener = urllib.request.build_opener(NoRedirect)
        with opener.open(request, timeout=180) as response:
            result = json.load(response)
        print(json.dumps(result))
    except urllib.error.HTTPError as error:
        detail = error.read().decode("utf-8", errors="replace").replace(key, "[REDACTED]")
        print(json.dumps({"transport_error": detail[:2000], "http_status": error.code}))
    except Exception as error:
        print(json.dumps({"transport_error": str(error).replace(key, "[REDACTED]")}))


if __name__ == "__main__":
    main()
