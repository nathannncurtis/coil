def f(x):
    match x:
        case [a, b]:
            return f"two: {a},{b}"
        case [a]:
            return f"one: {a}"
        case []:
            return "empty"
print(f([1, 2]), f([9]), f([]))
def g(x):
    match x:
        case {"k": v}:
            return "dict " + str(v)
        case {"a": av, "b": bv}:
            return "ab"
        case {}:
            return "any-dict"
print(g({"k": 1}), g({"a": 1, "b": 2}), g({}))
def h(x):
    match x:
        case first, *rest:
            return (first, rest)
        case [*all]:
            return ("empty", all)
print(h([1, 2, 3]), h([]), h((4, 5)))
def cap(x):
    match x:
        case [y] as whole:
            return (y, whole)
        case _:
            return None
print(cap([7]))
def m(x):
    match x:
        case {"a": _, "b": b}:
            return b
print(m({"a": 1, "b": 2}))
