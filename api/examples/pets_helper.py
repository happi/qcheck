#!/usr/bin/env python3
"""A qcheck helper: the pet API called in-process, with no network.

    qcheck openapi api/examples/pets.yaml --exec 'python3 api/examples/pets_helper.py'

It reads one call per line on stdin and writes one reply per line on stdout.
This one has a single bug: a pet's age comes back as a string.
"""
import json
import sys


def handle(request):
    method, path = request["method"], request["path"]
    if (method, path) == ("GET", "/health"):
        return 200, {"status": "ok"}
    if (method, path) == ("GET", "/pets"):
        return 200, []
    if (method, path) == ("POST", "/pets"):
        pet = request["body"]["json"]
        return 201, {"id": 1, "name": pet["name"], "age": str(pet["age"])}
    return 404, {"error": "no such pet"}


for line in sys.stdin:
    call = json.loads(line)
    if call["operation"] != "http":
        reply = {"error": "unknown operation " + call["operation"]}
    else:
        status, body = handle(call["input"])
        reply = {"output": {
            "status": status,
            "headers": [["content-type", "application/json"]],
            "body": json.dumps(body),
        }}
    print(json.dumps(reply), flush=True)
