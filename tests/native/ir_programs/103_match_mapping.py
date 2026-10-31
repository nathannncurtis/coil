def f(d):
    match d:
        case {"type": "point", "x": x, "y": y}:
            return (x, y)
        case {"type": "circle", "r": r}:
            return ("circle", r)
        case {"type": t}:
            return ("unknown-type", t)
        case {}:
            return "empty"
print(f({"type": "point", "x": 1, "y": 2}))
print(f({"type": "circle", "r": 3.0}))
print(f({"type": "blob", "extra": 1}))
print(f({}))
def rest_pattern(d):
    match d:
        case {"a": 1, **rest}:
            return sorted(rest.items())
        case _:
            return None
print(rest_pattern({"a": 1, "b": 2, "c": 3}))
print(rest_pattern({"a": 2}))
def nested(d):
    match d:
        case {"outer": {"inner": v}}:
            return v
        case _:
            return None
print(nested({"outer": {"inner": 42}}), nested({"outer": {}}))
def guard_map(d):
    match d:
        case {"v": v} if v > 10:
            return "big"
        case {"v": v}:
            return "small"
print(guard_map({"v": 20}), guard_map({"v": 2}))
def star_in_mapping_no():
    match {"x": 1}:
        case {"x": 1, **r}:
            return r
print(star_in_mapping_no())
