#!/usr/bin/env python3
"""Minimal JSON-RPC app-server driven by a Rust-generated JSON fixture."""

import json
import os
import sys


with open(os.environ["CODEX_FIXTURE"], encoding="utf-8") as fixture_file:
    fixture = json.load(fixture_file)


def send(message):
    print(json.dumps(message), flush=True)


def notify_turn(status="completed"):
    send(
        {
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {
                "threadId": fixture["threadId"],
                "turn": {
                    "id": fixture["turnId"],
                    "items": [],
                    "status": status,
                    "error": None,
                },
            },
        }
    )


def notify_reply(text):
    send(
        {
            "jsonrpc": "2.0",
            "method": "item/agentMessage/delta",
            "params": {
                "threadId": fixture["threadId"],
                "turnId": fixture["turnId"],
                "itemId": "item-message",
                "delta": text,
            },
        }
    )


for line in sys.stdin:
    request = json.loads(line)

    if request.get("id") == 99 and "result" in request:
        assert request["result"]["decision"] == "accept"
        notify_reply(fixture["reply"])
        notify_turn()
        continue

    method = request.get("method")
    if method == "initialize":
        result = {
            "userAgent": "fixture",
            "codexHome": "/tmp",
            "platformFamily": "unix",
            "platformOs": "linux",
        }
    elif method == "thread/list":
        result = fixture["threadList"]
    elif method == "thread/resume":
        assert request["params"]["cwd"] == fixture["expectedCwd"]
        result = fixture["threadResume"]
    elif method == "thread/start":
        result = fixture["threadStart"]
    elif method == "turn/start":
        assert request["params"]["threadId"] == fixture["threadId"]
        assert request["params"]["input"] == fixture["expectedInput"], (
            request["params"]["input"],
            fixture["expectedInput"],
        )
        send({"jsonrpc": "2.0", "id": request["id"], "result": fixture["turnStart"]})

        behavior = fixture.get("turnBehavior")
        if behavior == "waitForInterrupt":
            send(
                {
                    "jsonrpc": "2.0",
                    "method": "item/agentMessage/delta",
                    "params": {
                        "threadId": fixture["threadId"],
                        "turnId": fixture["turnId"],
                        "itemId": "item-message",
                        "delta": "ready-to-cancel",
                    },
                }
            )
            continue
        if behavior == "requestApproval":
            send(
                {
                    "jsonrpc": "2.0",
                    "id": 99,
                    "method": "item/commandExecution/requestApproval",
                    "params": {
                        "threadId": fixture["threadId"],
                        "turnId": fixture["turnId"],
                        "itemId": "approval-item",
                        "startedAtMs": 1,
                        "command": "echo fixture",
                        "cwd": "/tmp",
                        "availableDecisions": ["accept", "decline"],
                    },
                }
            )
            continue

        notify_reply(fixture["reply"])
        notify_turn()
        continue
    elif method == "turn/interrupt":
        send({"jsonrpc": "2.0", "id": request["id"], "result": {}})
        notify_turn(status="interrupted")
        continue
    else:
        continue

    send({"jsonrpc": "2.0", "id": request["id"], "result": result})
