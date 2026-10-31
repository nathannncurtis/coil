class NS:
    value = 5
    pair = (1, 2)
    text = "hi"
def vp(x):
    match x:
        case NS.value:
            return "is5"
        case NS.pair:
            return "is-pair"
        case NS.text:
            return "is-hi"
        case _:
            return "other"
print(vp(5), vp((1, 2)), vp("hi"), vp(6))
def vp2(x):
    match x:
        case [NS.value, NS.pair]:
            return "combo"
        case _:
            return "no"
print(vp2([5, (1, 2)]), vp2([5, 5]))
def or_capture(x):
    match x:
        case [1] | [2] as c:
            return c
        case _:
            return None
print(or_capture([1]), or_capture([2]), or_capture([3]))
def in_guard(x):
    match x:
        case n if isinstance(n, int) and n > 0:
            return "pos"
        case n if isinstance(n, int):
            return "nonpos"
        case _:
            return "not-int"
print(in_guard(5), in_guard(-5), in_guard("s"))
def with_walrus(x):
    match x:
        case [y] if (z := y * 2) > 4:
            return z
        case [y]:
            return y
print(with_walrus([3]), with_walrus([1]))
match_var = "assign-ok"
print(match_var)
