"""Bind the release's declared interfaces to the operations its probes exercise."""

import json
import sys
import tomllib
from pathlib import Path

# A change to either side requires a matching change to the black-box suite.
OPERATIONS = {
    ("post", "/v1/authorize"): ("authorizeCharging", "authorize"),
    ("post", "/v1/deliveries"): ("receiveChargeDetailRecord", "receiver"),
    ("post", "/v1/subscriber-events/recover"): ("recoverSubscriberEvent", "recover"),
    ("get", "/v1/driver-tariffs/{id}"): ("currentDriverTariff", "tariff"),
    ("get", "/v1/driver-tariffs/{id}/{version}"): ("versionedDriverTariff", "tariff"),
}
PROBED_SERVICES = {
    "authorize": {"empd"},
    "receiver": {"billd", "roamd"},
    "recover": {"csmsd", "billd"},
    "tariff": {"tarifd"},
    "mcp": {"emob"},
}
WEBHOOKS = {
    "chargeDetailRecordIssued": ("deliverChargeDetailRecord", "de.emob.cdr.issued"),
    "invoiceIssued": ("deliverInvoice", "de.emob.invoice.issued"),
}
METHODS = {"get", "put", "post", "delete", "options", "head", "patch", "trace"}
EVENT_MEDIA = "application/cloudevents+json"


def expect(label, actual, expected):
    if actual != expected:
        raise ValueError(f"{label}: expected {expected}, found {actual}")


def component(document, kind, value):
    if "$ref" not in value:
        return value
    prefix = f"#/components/{kind}/"
    reference = value["$ref"]
    if not reference.startswith(prefix):
        raise ValueError(f"unrecognized {kind} reference {reference}")
    return document["components"][kind][reference.removeprefix(prefix)]


def request_schema(document, operation):
    body = component(document, "requestBodies", operation["requestBody"])
    return body["content"][EVENT_MEDIA]["schema"]["$ref"]


def operation_keys(items):
    return {
        (method, name)
        for name, item in items.items()
        for method in METHODS.intersection(item)
    }


def check():
    manifest = tomllib.loads(Path("platform.toml").read_text())
    contracts = manifest["contracts"]
    openapi = Path(contracts["openapi"])
    document = json.loads(openapi.read_text())
    paths = document["paths"]
    hooks = document["webhooks"]

    events = {path.stem: path for path in Path("schemas/events").glob("*.json")}
    expect(
        "published event types", set(events), {event for _, event in WEBHOOKS.values()}
    )
    expect(
        "manifest event schemas",
        set(contracts["event_schemas"]),
        {str(p) for p in events.values()},
    )
    for event, path in events.items():
        schema = json.loads(path.read_text())
        expect(str(path), schema["$id"], "https://hupe1980.github.io/emob/" + str(path))
        expect(str(path), schema["properties"]["type"]["const"], event)

    roles = {role for _, role in OPERATIONS.values()}
    expect(
        "reference service probes",
        {role: set(owners) for role, owners in manifest["ports"].items()},
        PROBED_SERVICES,
    )
    expect("release probe paths", set(paths), {path for _, path in OPERATIONS})
    expect("release probe operations", operation_keys(paths), set(OPERATIONS))
    for (method, route), (operation_id, role) in OPERATIONS.items():
        item = paths[route]
        expect(route, item[method]["operationId"], operation_id)
        owners = {server["x-emob-service"] for server in item["servers"]}
        expect(f"owners of {route}", owners, PROBED_SERVICES[role])
        if role in ("authorize", "tariff"):
            response = component(
                document, "responses", item[method]["responses"]["200"]
            )
            reference = response["content"]["application/json"]["schema"]["$ref"]
            schema = (
                "openapi/authorize-answer.json"
                if role == "authorize"
                else "driver-tariff.v1.json"
            )
            expect(f"schema of {route}", reference, schema)

    expect("webhook names", set(hooks), set(WEBHOOKS))
    expect(
        "webhook probes", operation_keys(hooks), {("post", name) for name in WEBHOOKS}
    )
    for name, (operation_id, event) in WEBHOOKS.items():
        operation = hooks[name]["post"]
        expect(name, operation["operationId"], operation_id)
        expect(
            name, openapi.parent / request_schema(document, operation), events[event]
        )

    receive = paths["/v1/deliveries"]["post"]
    expect(
        "CDR receiver schema",
        openapi.parent / request_schema(document, receive),
        events["de.emob.cdr.issued"],
    )
    recover = paths["/v1/subscriber-events/recover"]["post"]["responses"]["200"]
    choices = recover["content"][EVENT_MEDIA]["schema"]["oneOf"]
    expect(
        "recovery response schemas",
        {openapi.parent / choice["$ref"] for choice in choices},
        set(events.values()),
    )
    expect("release target", manifest["interface"], "L4")
    expect("MCP command", contracts["mcp"], ["emob", "mcp"])
    expect("release recipe", contracts["conformance"], "just interfaces")
    expect("image definition", contracts["images"], "docker-bake.hcl")
    if not Path(contracts["images"]).is_file():
        raise ValueError(f"missing image definition {contracts['images']}")
    print(
        f"{len(OPERATIONS)} HTTP operations across {len(roles)} roles, {len(events)} webhooks, MCP; target L4"
    )


if __name__ == "__main__":
    try:
        check()
    except (OSError, ValueError, KeyError, TypeError) as error:
        sys.exit(f"interface contract check failed: {error}")
