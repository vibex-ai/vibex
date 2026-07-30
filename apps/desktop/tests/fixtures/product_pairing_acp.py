#!/usr/bin/env python3
import json
import sys


def send(value):
    sys.stdout.write(json.dumps(value, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def session_state(session_id, model_id="fixture/model-1"):
    return {
        "sessionId": session_id,
        "models": {
            "availableModels": [
                {"modelId": "fixture/model-1"},
                {"modelId": "fixture/model-2"},
            ],
            "currentModelId": model_id,
        },
        "modes": {
            "availableModes": [
                {"id": "build", "label": "Build"},
                {"id": "review", "label": "Review"},
            ],
            "currentModeId": "build",
        },
        "configOptions": [
            {
                "id": "model",
                "category": "model",
                "label": "Model",
                "type": "select",
                "currentValue": model_id,
                "options": [
                    {"value": "fixture/model-1", "label": "Fixture Model 1"},
                    {"value": "fixture/model-2", "label": "Fixture Model 2"},
                ],
            }
        ],
    }


session_counter = 0

for raw_line in sys.stdin:
    try:
        message = json.loads(raw_line)
    except json.JSONDecodeError:
        continue
    method = message.get("method")
    request_id = message.get("id")
    params = message.get("params") or {}

    if method == "initialize":
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": {
                    "loadSession": True,
                    "listSessions": True,
                    "sessionCapabilities": {"resume": {}},
                    "sessionConfig": {
                        "setModel": {"supported": True, "encoding": "typed"},
                        "setMode": {"supported": True, "encoding": "typed"},
                        "setConfigOption": {
                            "supported": True,
                            "encoding": "versioned_raw",
                        },
                    },
                },
                "agentInfo": {"name": "vibex-e2e-acp", "version": "1.0.0"},
            },
        })
    elif method == "model/list":
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "data": [
                    {"id": "fixture/model-1"},
                    {"id": "fixture/model-2"},
                ],
                "nextCursor": None,
            },
        })
    elif method == "session/new":
        session_counter += 1
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": session_state(f"fixture-session-{session_counter}"),
        })
    elif method in ("session/load", "session/resume"):
        session_id = params.get("sessionId") or "fixture-restored-session"
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": session_state(session_id),
        })
    elif method == "session/list":
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"sessions": []},
        })
    elif method == "session/set_model":
        session_id = params.get("sessionId") or "fixture-session-1"
        model_id = params.get("modelId") or "fixture/model-1"
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": session_state(session_id, model_id),
        })
    elif method == "session/set_mode":
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "sessionId": params.get("sessionId") or "fixture-session-1",
                "modes": {
                    "availableModes": [
                        {"id": "build", "label": "Build"},
                        {"id": "review", "label": "Review"},
                    ],
                    "currentModeId": params.get("modeId") or "build",
                },
            },
        })
    elif method == "session/set_config_option":
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"configOptions": []},
        })
    elif method == "session/prompt":
        session_id = params.get("sessionId") or "fixture-session-1"
        send({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "agent_thought_chunk",
                    "content": {"type": "text", "text": "processing"},
                },
            },
        })
        send({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "OK"},
                },
            },
        })
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"stopReason": "end_turn"},
        })
    elif request_id is not None:
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": -32601, "message": "method not found"},
        })
