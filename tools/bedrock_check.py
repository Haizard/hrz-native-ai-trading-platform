"""Check that the configured Bedrock model is real and can do tool use.

Phase 5 (the AI agent) depends on three properties of the LLM provider, and none
of them is safe to assume:

  1. the model id in `.env` exists in this region (Bedrock often needs a full id
     or an inference-profile ARN rather than a bare name);
  2. the model supports the Converse API's tool use -- the agent design is built
     on tool calling, not on prompting the model to emit JSON;
  3. a `toolResult` can be fed back and consumed, because that is the shape of
     the loop the agent actually runs.

Run it before touching the provider wiring, and after any model id change.

    # boto3 is not a repo dependency; use the managed venv
    ~/.workbuddy-ai/binaries/python/envs/default/Scripts/python.exe \
        tools/bedrock_check.py

Credentials are read from the repo's `.env` and never printed.
"""

import json
import os
import pathlib
import sys

import boto3
from botocore.config import Config

ENV = pathlib.Path(__file__).resolve().parent.parent / ".env"

TOOL = {
    "toolSpec": {
        "name": "get_volume_profile",
        "description": (
            "Return the volume profile for a symbol: point of control (POC), value "
            "area high (VAH) and value area low (VAL), in price terms."
        ),
        "inputSchema": {
            "json": {
                "type": "object",
                "properties": {
                    "symbol": {"type": "string", "description": "e.g. BTCUSDT"},
                    "timeframe": {"type": "string", "description": "e.g. 5m, 1h"},
                },
                "required": ["symbol", "timeframe"],
            }
        },
    }
}

ASK = (
    "I want the volume profile for BTCUSDT on the 5m timeframe. "
    "Use your tool to get the real numbers, do not guess."
)

# A plausible Analytics Core result, to see what the model does with real numbers.
TOOL_RESULT = {
    "symbol": "BTCUSDT",
    "timeframe": "5m",
    "poc": 103250.5,
    "vah": 103980.0,
    "val": 102410.25,
}


def load_env():
    if not ENV.exists():
        raise SystemExit(f"no {ENV} -- copy .env.example and fill it in")
    for raw in ENV.read_text(encoding="utf-8").splitlines():
        raw = raw.strip()
        if not raw or raw.startswith("#") or "=" not in raw:
            continue
        key, value = raw.split("=", 1)
        os.environ.setdefault(key.strip(), value.strip())


def check_models(bedrock, configured):
    print("--- 1. is the model id real? ---")
    try:
        ids = sorted(m["modelId"] for m in bedrock.list_foundation_models()["modelSummaries"])
    except Exception as error:  # noqa: BLE001
        print("  FAILED:", type(error).__name__, error)
        return False

    print(f"  {len(ids)} models visible in this region")
    if configured not in ids:
        print(f"  FAIL: `{configured}` is not among them")
        print("  qwen ids that are:", [i for i in ids if "qwen" in i.lower()])
        return False
    print(f"  OK: `{configured}` is available (no inference-profile ARN needed)")
    return True


def check_tool_use(runtime, model_id):
    """Returns `(response, tool_use)` on success, or `None` on failure."""
    print("--- 2. does it support tool use? ---")
    try:
        response = runtime.converse(
            modelId=model_id,
            messages=[{"role": "user", "content": [{"text": ASK}]}],
            toolConfig={"tools": [TOOL]},
            inferenceConfig={"maxTokens": 512, "temperature": 0.0},
        )
    except Exception as error:  # noqa: BLE001
        print("  FAILED:", type(error).__name__, error)
        return None

    blocks = response["output"]["message"]["content"]
    uses = [b["toolUse"] for b in blocks if "toolUse" in b]
    if not uses:
        print(f"  FAIL: stopReason={response.get('stopReason')}, no toolUse block")
        for block in blocks:
            if "text" in block:
                print("  the model answered in prose instead:", block["text"][:200])
        return None

    use = uses[0]
    print(f"  OK: stopReason={response.get('stopReason')}, called {use['name']}")
    print(f"      input={json.dumps(use['input'], sort_keys=True)}")
    return response, use


def check_round_trip(runtime, model_id, first_response, use):
    """Returns `True` if the model consumed the tool result and answered."""
    print("--- 3. can it consume a toolResult? ---")
    second = runtime.converse(
        modelId=model_id,
        messages=[
            {"role": "user", "content": [{"text": ASK}]},
            first_response["output"]["message"],
            {
                "role": "user",
                "content": [
                    {
                        "toolResult": {
                            "toolUseId": use["toolUseId"],
                            "content": [{"json": TOOL_RESULT}],
                        }
                    }
                ],
            },
        ],
        toolConfig={"tools": [TOOL]},
        inferenceConfig={"maxTokens": 512, "temperature": 0.0},
    )

    text = "".join(b["text"] for b in second["output"]["message"]["content"] if "text" in b)
    if second.get("stopReason") != "end_turn" or not text:
        print(f"  FAIL: stopReason={second.get('stopReason')}, {len(text)} chars of text")
        return False

    print(f"  OK: stopReason=end_turn, {len(text)} chars")
    print("\n  final answer:\n   ", text.replace("\n", "\n    "))

    # The model reformats numbers it is given (`103250.5` -> `$103,250.50`), so
    # this is expected to miss. It is printed because it is the reason the thesis
    # object must carry structured numbers rather than parsed prose.
    print("\n  (for the record) raw values found verbatim in the prose:")
    for key, value in TOOL_RESULT.items():
        if isinstance(value, float):
            print(f"    {key}={value}: {str(value) in text}")
    return True


def main():
    load_env()
    region = os.environ.get("AWS_BEDROCK_REGION", "us-east-1")
    model_id = os.environ.get("AWS_BEDROCK_MODEL_ID", "")
    if not model_id:
        raise SystemExit("AWS_BEDROCK_MODEL_ID is not set")

    print(f"region: {region}")
    print(f"model : {model_id}")
    print(f"keys  : access={bool(os.environ.get('AWS_ACCESS_KEY_ID'))} "
          f"secret={bool(os.environ.get('AWS_SECRET_ACCESS_KEY'))}\n")

    config = Config(retries={"max_attempts": 1}, connect_timeout=15, read_timeout=120)
    bedrock = boto3.client("bedrock", region_name=region, config=config)
    runtime = boto3.client("bedrock-runtime", region_name=region, config=config)

    if not check_models(bedrock, model_id):
        return 1

    hop = check_tool_use(runtime, model_id)
    if hop is None:
        return 1

    if not check_round_trip(runtime, model_id, hop[0], hop[1]):
        return 1

    print("\nAll three checks passed. Phase 5 can be built on this model.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
