"""Bounded semantic oracle for the release detector's MCP snapshot contract.

This is deliberately not a general JSON Schema validator. The grammar is:
  value := {} | {type: T} | {enum: E} | {const: V} | {required: [K]}
  atom := true | false | value | {properties: {a: P}} | {additionalProperties: false}
        | {type: object, properties: {a: {}}, additionalProperties: false}
        | {propertyNames: {enum: E}}
  choice := {anyOf|oneOf|allOf: [atom, atom]}
  nested := {anyOf|oneOf: [choice, atom]}
  conditional := {if: S, then?: B, else?: B}
The finite terminals below bound T, E, V and K; P is {}, type: string or
enum: ["a"]. Pairs also wrap atoms (including true/false) in a property
schema, and choices may have a sibling applicator. Pairs
exhaust atom substitutions, substitutions in a branch/property, branch
insertion/deletion/reordering, keyword deletion under one outer choice,
allOf member/conjunction edits (including independent sibling applicators), conditional selector/branch edits, moving
required names between root and then/else, and propertyNames enum edits.
This is a finite soundness check, not a proof over arbitrary JSON Schema.
"""

from itertools import product


# Explicit witnesses include every JSON type, numeric equality and objects
# with absent, declared, extra and nested properties. No randomness or network.
INSTANCES = [None, False, True, 0, 1, 1.0, 1.5, "a", "b", [], [1], {}]
INSTANCES += [{"a": value} for value in (None, False, 0, 1, "a", "b", {}, {"a": "a"})]
INSTANCES += [{"b": "b"}, {"c": 0}, {"c": "c"}]
INSTANCES += [{"a": left, "b": right} for left, right in product((0, "a", "b"), repeat=2)]
INSTANCES += [{"a": "a", "c": "c"}, {"a": 0, "c": "c"}, {"b": "b", "c": "c"}, {"a": "a", "b": "b", "c": "c"}]


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
                               "additionalProperties", "anyOf", "oneOf",
                               "allOf", "if", "then", "else", "propertyNames"}
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
        if "propertyNames" in schema:
            name_schema = schema["propertyNames"]
            if not all(schema_accepts_instance(name_schema, name) for name in instance):
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
    if "allOf" in schema and not all(
            schema_accepts_instance(branch, instance) for branch in schema["allOf"]):
        return False
    if "if" in schema:
        branch_key = "then" if schema_accepts_instance(schema["if"], instance) else "else"
        if branch_key in schema and not schema_accepts_instance(schema[branch_key], instance):
            return False
    return True


def bounded_schema_pairs():
    """Generate deterministic pairs, with no detector-specific expectations."""
    atoms = [True, False, {}] + [{"type": name} for name in
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

    # allOf member substitutions, insertions, deletions, reorderings, and
    # interactions with enclosing/nested applicators.
    allof_atoms = [
        True, False, {}, {"type": "string"}, {"type": "object"},
        {"enum": ["a"]}, {"enum": ["a", "b"]}, {"const": "a"},
        {"required": ["a"]}, {"required": ["b"]}, {"required": ["a", "b"]},
        {"properties": {"a": {"type": "string"}}},
        {"additionalProperties": False},
        {"propertyNames": {"enum": ["a", "b"]}},
        {"propertyNames": {"enum": ["a"]}},
    ]
    for before, after in product(allof_atoms, repeat=2):
        yield {"allOf": [before]}, {"allOf": [after]}
        yield {"allOf": [before, {"type": "object"}]}, {"allOf": [after, {"type": "object"}]}
        yield {"allOf": [before, {"required": ["b"]}]}, {"allOf": [after, {"required": ["b"]}]}
    for left, right in product(allof_atoms, siblings):
        for branches in ([left], [right, left], [left, right, left]):
            yield {"allOf": [left, right]}, {"allOf": branches}
        for branches in ([left, right], [right, left]):
            yield {"allOf": [left]}, {"allOf": branches}
        yield left, {"allOf": [left, right]}
        yield left, {"allOf": [right]}
        yield {"allOf": [left, right]}, left
        yield {"allOf": [left]}, {}
        for outer in ("anyOf", "oneOf"):
            yield {outer: [{"allOf": [left]}, right]}, {outer: [{}, right]}
            yield {outer: [{"allOf": [left, right]}, right]}, {outer: [{"allOf": [left]}, right]}
            yield {outer: [{"allOf": [left]}, right]}, {outer: [{"allOf": [left, right]}, right]}
            yield {outer: [left, right]}, {outer: [{"allOf": [left, right]}, right]}
            yield {"allOf": [{outer: [left, right]}]}, {"allOf": [{outer: [left]}]}
            yield {"allOf": [{outer: [left]}]}, {"allOf": [{outer: [left, right]}]}
            yield {"allOf": [{outer: [left, right]}]}, {"allOf": [{}]}

    # Independent same-keyword applicators in one allOf: adding, deleting or
    # narrowing one conjunct while a sibling of the same keyword survives, and
    # wrapping/unwrapping a bare applicator into allOf alongside a sibling.
    conjuncts = [{"anyOf": [{"type": "string"}, {"type": "boolean"}]},
                 {"anyOf": [{"type": "string"}, {"type": "integer"}]},
                 {"oneOf": [{"type": "string"}, {"type": "boolean"}]},
                 {"oneOf": [{"type": "string"}, {"type": "integer"}]}]
    for left, right in product(conjuncts, repeat=2):
        yield {"allOf": [left, right]}, {"allOf": [left]}
        yield {"allOf": [left]}, {"allOf": [left, right]}
        yield {"allOf": [left, right]}, {"allOf": [right, left]}
        yield left, {"allOf": [left, right]}
        yield left, {"allOf": [right, left]}
        yield {"allOf": [left, right]}, left
        for keyword in ("anyOf", "oneOf"):
            if keyword in right:
                yield ({"allOf": [left, right]},
                       {"allOf": [left, {keyword: right[keyword][:1]}]})
            if keyword in left:
                yield left, {"allOf": [{keyword: left[keyword][:1]}, right]}
                yield {"allOf": [left, right]}, {keyword: left[keyword][:1]}

    # if / then / else conditionals: selector edits, branch edits, adding/removing
    # if/then/else, moving required names between root and then/else, and
    # propertyNames enum changes inside branches.
    def make_cond(if_s, then_s, else_s):
        schema = {"if": if_s} if if_s is not None else {}
        if then_s is not None:
            schema["then"] = then_s
        if else_s is not None:
            schema["else"] = else_s
        return schema

    selectors = [
        True,
        False,
        {"required": []},
        {"required": ["a"]},
        {"required": ["b"]},
        {"required": ["a", "b"]},
        {"type": "object"},
        {"properties": {"a": {"type": "string"}}, "required": ["a"]},
    ]
    cond_branches = [
        None,
        True,
        False,
        {},
        {"required": ["a"]},
        {"required": ["b"]},
        {"required": ["c"]},
        {"required": ["b", "c"]},
        {"type": "string"},
        {"properties": {"a": {"type": "string"}}},
        {"additionalProperties": False},
        {"propertyNames": {"enum": ["a", "b"]}},
        {"propertyNames": {"enum": ["a"]}},
        {"allOf": [{"required": ["b"]}]},
    ]
    # Selector (`if`) edits with representative then/else pairs.
    branch_pairs = [
        (None, {"required": ["c"]}),
        ({"required": ["b"]}, None),
        ({"required": ["b"]}, {"required": ["c"]}),
        ({"required": ["b"]}, {"type": "string"}),
        ({"type": "string"}, {"required": ["c"]}),
        ({"propertyNames": {"enum": ["a", "b"]}}, {"required": ["c"]}),
        ({"allOf": [{"required": ["b"]}]}, {"required": ["c"]}),
        (False, {"required": ["c"]}),
    ]
    for old_if, new_if in product(selectors, repeat=2):
        for then_s, else_s in branch_pairs:
            old_c = make_cond(old_if, then_s, else_s)
            new_c = make_cond(new_if, then_s, else_s)
            yield old_c, new_c
            yield {"allOf": [old_c]}, {"allOf": [new_c]}
            yield {"required": ["b"], "allOf": [old_c]}, {"required": ["b"], "allOf": [new_c]}
            yield {"oneOf": [old_c, {"type": "string"}]}, {"oneOf": [new_c, {"type": "string"}]}

    # Branch (`then` / `else`) edits including branch addition and removal.
    for if_s in ({"required": ["a"]}, {"type": "object"}, True):
        for old_b, new_b in product(cond_branches, repeat=2):
            for other in (None, {"required": ["c"]}):
                old_t = make_cond(if_s, old_b, other)
                new_t = make_cond(if_s, new_b, other)
                yield old_t, new_t
                yield {"allOf": [old_t]}, {"allOf": [new_t]}
                old_e = make_cond(if_s, other, old_b)
                new_e = make_cond(if_s, other, new_b)
                yield old_e, new_e
                yield {"allOf": [old_e]}, {"allOf": [new_e]}

    # Adding/removing `if` (with/without existing then/else) and adding/removing
    # a whole conditional inside root, allOf, anyOf, oneOf.
    sample_conds = [
        make_cond({"required": ["a"]}, {"required": ["b"]}, None),
        make_cond({"required": ["a"]}, None, {"required": ["c"]}),
        make_cond({"required": ["a"]}, {"required": ["b"]}, {"required": ["c"]}),
        make_cond({"required": ["a"]}, {"propertyNames": {"enum": ["a"]}}, {"type": "string"}),
    ]
    for cond in sample_conds:
        without_if = {k: v for k, v in cond.items() if k != "if"}
        for base in ({}, {"type": "object"}, without_if):
            yield base, cond
            yield cond, base
            yield {"allOf": [base]}, {"allOf": [cond]}
            yield {"allOf": [cond]}, {"allOf": [base]}
            yield {"allOf": [{"required": ["a"]}]}, {"allOf": [{"required": ["a"]}, cond]}
            yield {"allOf": [{"required": ["a"]}, cond]}, {"allOf": [{"required": ["a"]}]}
            for outer in ("anyOf", "oneOf"):
                yield {outer: [base, {"type": "string"}]}, {outer: [cond, {"type": "string"}]}
                yield {outer: [cond, {"type": "string"}]}, {outer: [base, {"type": "string"}]}

    # Moving a required name between root and then/else (both flat and allOf-wrapped).
    req_sets = [[], ["b"], ["c"], ["b", "c"]]
    for if_s in ({"required": ["a"]}, {"required": []}, True, {"type": "object"}):
        for r_old, t_old, e_old in product(req_sets, req_sets, req_sets):
            if r_old == t_old == e_old:
                continue
            for r_new, t_new, e_new in (
                ([], sorted(set(t_old) | set(r_old)), e_old),
                ([], t_old, sorted(set(e_old) | set(r_old))),
                ([], sorted(set(t_old) | set(r_old)), sorted(set(e_old) | set(r_old))),
                (sorted(set(r_old) | set(t_old)), [], e_old),
                (sorted(set(r_old) | set(e_old)), t_old, []),
                (sorted(set(r_old) | (set(t_old) & set(e_old))),
                 sorted(set(t_old) - set(e_old)),
                 sorted(set(e_old) - set(t_old))),
            ):
                yield (
                    {"required": r_old, "if": if_s, "then": {"required": t_old}, "else": {"required": e_old}},
                    {"required": r_new, "if": if_s, "then": {"required": t_new}, "else": {"required": e_new}},
                )
                yield (
                    {"required": r_old, "allOf": [{"if": if_s, "then": {"required": t_old}, "else": {"required": e_old}}]},
                    {"required": r_new, "allOf": [{"if": if_s, "then": {"required": t_new}, "else": {"required": e_new}}]},
                )

    # Multiple conditionals inside allOf (reordering, adding, removing).
    c1 = {"if": {"required": ["a"]}, "then": {"required": ["b"]}, "else": {"required": ["c"]}}
    c2 = {"if": {"required": ["b"]}, "then": {"propertyNames": {"enum": ["a", "b"]}}, "else": {"type": "object"}}
    yield {"allOf": [c1, c2]}, {"allOf": [c2, c1]}
    yield {"allOf": [c1]}, {"allOf": [c2, c1]}
    yield {"allOf": [c1, c2]}, {"allOf": [c1]}

    # Standalone propertyNames edits (root, allOf, oneOf).
    prop_name_atoms = [{}, {"propertyNames": {"enum": ["a", "b"]}},
                       {"propertyNames": {"enum": ["a"]}}, {"propertyNames": False}]
    for old_pn, new_pn in product(prop_name_atoms, repeat=2):
        yield old_pn, new_pn
        yield {"allOf": [old_pn]}, {"allOf": [new_pn]}
        yield {"oneOf": [old_pn, {"type": "string"}]}, {"oneOf": [new_pn, {"type": "string"}]}
