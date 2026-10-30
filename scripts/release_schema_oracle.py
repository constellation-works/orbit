"""Bounded semantic oracle for the release detector's MCP snapshot contract.

This is deliberately not a general JSON Schema validator. The grammar is:
  value := {} | {type: T} | {enum: E} | {const: V} | {required: [K]}
  atom := value | {properties: {a: P}} | {additionalProperties: false}
        | {type: object, properties: {a: {}}, additionalProperties: false}
  choice := {anyOf|oneOf: [atom, atom]}
  nested := {anyOf|oneOf: [choice, atom]}
The finite terminals below bound T, E, V and K; P is {}, type: string or
enum: ["a"]. Pairs also wrap atoms in a property schema, and choices may
have a sibling applicator. Pairs
exhaust atom substitutions, substitutions in a branch/property, branch
insertion/deletion/reordering, and keyword deletion under one outer choice.
This is a finite soundness check, not a proof over arbitrary JSON Schema.
"""

from itertools import product


# Explicit witnesses include every JSON type, numeric equality and objects
# with absent, declared, extra and nested properties. No randomness or network.
INSTANCES = [None, False, True, 0, 1, 1.0, 1.5, "a", "b", [], [1], {}]
INSTANCES += [{"a": value} for value in (None, False, 0, 1, "a", "b", {}, {"a": "a"})]
INSTANCES += [{"b": "b"}, {"c": 0}]
INSTANCES += [{"a": left, "b": right} for left, right in product((0, "a", "b"), repeat=2)]


def schema_instance_equal(left, right):
    """JSON equality: bool is not a number; integral floats equal integers."""
    if isinstance(left, bool) or isinstance(right, bool):
        return type(left) is type(right) and left == right
    if isinstance(left, (int, float)) and isinstance(right, (int, float)):
        return left == right
    if type(left) is not type(right):
        return False
    if isinstance(left, dict):
        return left.keys() == right.keys() and all(
            schema_instance_equal(left[key], right[key]) for key in left)
    if isinstance(left, list):
        return len(left) == len(right) and all(
            schema_instance_equal(a, b) for a, b in zip(left, right))
    return left == right


def schema_accepts_instance(schema, instance):
    """Independently interpret every keyword in the grammar; reject unknowns."""
    if isinstance(schema, bool):
        return schema
    unknown = schema.keys() - {"type", "enum", "const", "required", "properties",
                               "additionalProperties", "anyOf", "oneOf"}
    if unknown:
        raise ValueError(f"oracle does not implement {unknown}")
    types = {
        "null": instance is None,
        "boolean": isinstance(instance, bool),
        "integer": type(instance) in (int, float) and int(instance) == instance,
        "number": type(instance) in (int, float),
        "string": isinstance(instance, str),
        "array": isinstance(instance, list),
        "object": isinstance(instance, dict),
    }
    if "type" in schema:
        names = schema["type"] if isinstance(schema["type"], list) else [schema["type"]]
        if not any(types[name] for name in names):
            return False
    if "enum" in schema and not any(
            schema_instance_equal(instance, value) for value in schema["enum"]):
        return False
    if "const" in schema and not schema_instance_equal(instance, schema["const"]):
        return False
    # Object keywords have no effect on non-objects, even without type: object.
    if isinstance(instance, dict):
        if not set(schema.get("required", [])) <= instance.keys():
            return False
        properties = schema.get("properties", {})
        for name, value in instance.items():
            child = properties.get(name, schema.get("additionalProperties", True))
            if not schema_accepts_instance(child, value):
                return False
    if "anyOf" in schema and not any(
            schema_accepts_instance(branch, instance) for branch in schema["anyOf"]):
        return False
    if "oneOf" in schema and sum(
            schema_accepts_instance(branch, instance) for branch in schema["oneOf"]) != 1:
        return False
    return True


def bounded_schema_pairs():
    """Generate deterministic pairs, with no detector-specific expectations."""
    atoms = [{}] + [{"type": name} for name in
                    ("null", "boolean", "integer", "number", "string", "array", "object")]
    atoms += [{"type": ["string", "null"]}]
    atoms += [{"enum": values} for values in (["a"], ["b"], ["a", "b"], [0, False])]
    atoms += [{"const": value} for value in ("a", "b", 0, False)]
    atoms += [{"required": [name]} for name in ("a", "b")]
    atoms += [{"properties": {"a": child}} for child in ({}, {"type": "string"}, {"enum": ["a"]})]
    atoms += [{"additionalProperties": False},
              {"type": "object", "properties": {"a": {}}, "additionalProperties": False}]
    siblings = [{"type": "string"}, {"enum": ["b"]},
                {"required": ["b"]}, {"properties": {"a": {"enum": ["b"]}}}]
    yield from product(atoms, repeat=2)
    for before, after in product(atoms, repeat=2):
        yield {"properties": {"a": before}}, {"properties": {"a": after}}
        for keyword, sibling in product(("anyOf", "oneOf"), siblings):
            yield {keyword: [before, sibling]}, {keyword: [after, sibling]}
    # Branch identity, duplicate branches, insertions, deletions and reordering.
    for keyword, left, right in product(("anyOf", "oneOf"), atoms, siblings):
        for branches in ([left], [right, left], [left, right, left]):
            yield {keyword: [left, right]}, {keyword: branches}
    # Whole-keyword deletion, with and without a surviving sibling applicator,
    # nested one level. Covers the non-monotonic relaxation defect family.
    for outer, inner, left, right in product(
            ("anyOf", "oneOf"), ("anyOf", "oneOf"), atoms, siblings):
        for keep in ({}, {"anyOf" if inner == "oneOf" else "oneOf": [right]}):
            before = dict(keep, **{inner: [left]})
            yield {outer: [before, right]}, {outer: [keep, right]}
