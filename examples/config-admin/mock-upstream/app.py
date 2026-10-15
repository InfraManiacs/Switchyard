# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""An OpenAI-compatible endpoint that answers without calling a model.

The stack needs somewhere for routed requests to land. Using this instead of a
real provider keeps the stack offline and free to run, and makes the reply name
the model that was selected, which is what you want when checking that a
configuration change moved traffic.
"""

import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = 8000


class Handler(BaseHTTPRequestHandler):
    def do_POST(self) -> None:
        length = int(self.headers.get("content-length", 0))
        try:
            request = json.loads(self.rfile.read(length) or b"{}")
        except json.JSONDecodeError:
            request = {}

        model = request.get("model", "unknown")
        # Echo the prompt rather than naming the model again: the caller already
        # learns the model from the `model` field, and an echo shows the whole
        # round trip arrived intact.
        messages = request.get("messages") or []
        prompt = ""
        if messages:
            content = messages[-1].get("content", "")
            prompt = content if isinstance(content, str) else str(content)
        body = json.dumps(
            {
                "id": "mock-completion",
                "object": "chat.completion",
                "created": 0,
                "model": model,
                "choices": [
                    {
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": f"Received: {prompt}",
                        },
                        "finish_reason": "stop",
                    }
                ],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "total_tokens": 2,
                },
            }
        ).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:
        self.send_response(200)
        self.send_header("content-length", "0")
        self.end_headers()

    def log_message(self, *_args: object) -> None:
        """Keeps routed requests out of the compose log."""


if __name__ == "__main__":
    ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
